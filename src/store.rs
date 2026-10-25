use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::Path;

use chrono::{DateTime, Utc};
use rustix::fs::{FlockOperation, flock};

use crate::model::Sample;

#[derive(Debug, PartialEq, Clone)]
pub struct Loaded {
    pub samples: Vec<Sample>,
    pub skipped: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub ino: u64,
    pub offset: u64,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Incremental {
    pub loaded: Loaded,
    pub cursor: Cursor,
    pub reset: bool,
}

struct LockGuard {
    file: fs::File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = flock(&self.file, FlockOperation::Unlock);
    }
}

fn lock_sidecar(path: &Path, op: FlockOperation) -> std::io::Result<LockGuard> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    flock(&file, op).map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
    Ok(LockGuard { file })
}

/// Timestamp-only view of a stored line, used to discard old lines without
/// allocating the string fields of a full [`Sample`].
#[derive(serde::Deserialize)]
struct TsOnly {
    ts: DateTime<Utc>,
}

fn trimmed_line(raw: &[u8]) -> Option<Result<&str, ()>> {
    let mut slice = raw;
    if slice.ends_with(b"\n") {
        slice = &slice[..slice.len() - 1];
        if slice.ends_with(b"\r") {
            slice = &slice[..slice.len() - 1];
        }
    }
    match std::str::from_utf8(slice) {
        Ok(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(Ok(trimmed))
            }
        }
        Err(_) => Some(Err(())),
    }
}

fn parse_line(raw: &[u8]) -> Option<Result<Sample, ()>> {
    trimmed_line(raw)
        .map(|line| line.and_then(|l| serde_json::from_str::<Sample>(l).map_err(|_| ())))
}

/// Parses a line, returning `None` for blank lines and lines older than `cutoff`.
fn parse_line_since(raw: &[u8], cutoff: DateTime<Utc>) -> Option<Result<Sample, ()>> {
    let line = match trimmed_line(raw)? {
        Ok(line) => line,
        Err(()) => return Some(Err(())),
    };
    match serde_json::from_str::<TsOnly>(line) {
        Ok(TsOnly { ts }) if ts < cutoff => None,
        Ok(_) => Some(serde_json::from_str::<Sample>(line).map_err(|_| ())),
        Err(_) => Some(Err(())),
    }
}

pub fn append(path: &Path, samples: &[Sample]) -> std::io::Result<()> {
    if samples.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut buffer = Vec::new();
    for sample in samples {
        serde_json::to_writer(&mut buffer, sample)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        buffer.push(b'\n');
    }
    let _guard = lock_sidecar(path, FlockOperation::LockExclusive)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&buffer)?;
    Ok(())
}

pub fn load(path: &Path) -> std::io::Result<Loaded> {
    load_filtered(path, None)
}

/// Loads only samples with `ts >= cutoff`.
///
/// Lines are filtered individually, so the result does not depend on the file being
/// chronologically ordered (e.g. after a backwards system clock jump). Older lines are
/// discarded after a timestamp-only parse; `skipped` counts lines whose timestamp
/// cannot be read and newer lines that fail to parse as a full [`Sample`].
pub fn load_since(path: &Path, cutoff: DateTime<Utc>) -> std::io::Result<Loaded> {
    load_filtered(path, Some(cutoff))
}

fn load_filtered(path: &Path, cutoff: Option<DateTime<Utc>>) -> std::io::Result<Loaded> {
    if !path.exists() {
        return Ok(Loaded {
            samples: Vec::new(),
            skipped: 0,
        });
    }
    let _guard = lock_sidecar(path, FlockOperation::LockShared)?;
    if !path.exists() {
        return Ok(Loaded {
            samples: Vec::new(),
            skipped: 0,
        });
    }
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut samples = Vec::new();
    let mut skipped = 0;
    let mut line_buf = Vec::new();

    while reader.read_until(b'\n', &mut line_buf)? > 0 {
        let parsed = match cutoff {
            Some(cutoff) => parse_line_since(&line_buf, cutoff),
            None => parse_line(&line_buf),
        };
        match parsed {
            Some(Ok(sample)) => samples.push(sample),
            Some(Err(())) => skipped += 1,
            None => {}
        }
        line_buf.clear();
    }
    Ok(Loaded { samples, skipped })
}

pub fn oldest_timestamp(path: &Path) -> std::io::Result<Option<DateTime<Utc>>> {
    if !path.exists() {
        return Ok(None);
    }
    let _guard = lock_sidecar(path, FlockOperation::LockShared)?;
    if !path.exists() {
        return Ok(None);
    }
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut line_buf = Vec::new();

    while reader.read_until(b'\n', &mut line_buf)? > 0 {
        if let Some(Ok(sample)) = parse_line(&line_buf) {
            return Ok(Some(sample.ts));
        }
        line_buf.clear();
    }
    Ok(None)
}

pub fn prune(path: &Path, cutoff: DateTime<Utc>) -> std::io::Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let _guard = lock_sidecar(path, FlockOperation::LockExclusive)?;
    if !path.exists() {
        return Ok(0);
    }

    let file = fs::File::open(path)?;
    #[cfg(unix)]
    let permissions = file.metadata()?.permissions();

    let mut reader = BufReader::new(file);
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    // serde_json emits many small writes per value; buffer them instead of issuing
    // a syscall per token against the temp file.
    let mut writer = std::io::BufWriter::new(temp.as_file_mut());
    let mut pruned = 0;
    let mut line_buf = Vec::new();

    while reader.read_until(b'\n', &mut line_buf)? > 0 {
        match parse_line(&line_buf) {
            Some(Ok(sample)) => {
                if sample.ts >= cutoff {
                    serde_json::to_writer(&mut writer, &sample)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                    writer.write_all(b"\n")?;
                } else {
                    pruned += 1;
                }
            }
            Some(Err(())) | None => {
                // Corrupt lines dropped during prune without incrementing pruned valid samples
            }
        }
        line_buf.clear();
    }

    writer.flush()?;
    drop(writer);
    drop(reader);

    #[cfg(unix)]
    let _ = temp.as_file().set_permissions(permissions);

    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(pruned)
}

/// Loads newly appended samples from `path` starting at `cursor.offset`.
///
/// If `cursor` is the default cursor, or if the file was replaced (different inode, e.g.
/// after a prune rename) or truncated (`len < cursor.offset`), the file is read from the
/// beginning and `reset = true` is reported.
///
/// Residual edge case: Inode reuse across two prunes between reloads could theoretically
/// defeat inode change detection; this is practically impossible because prunes occur at
/// most daily and the TUI reloads every 30 seconds.
pub fn load_from(path: &Path, cursor: Cursor) -> std::io::Result<Incremental> {
    if !path.exists() {
        return Ok(Incremental {
            loaded: Loaded {
                samples: Vec::new(),
                skipped: 0,
            },
            cursor: Cursor::default(),
            reset: cursor != Cursor::default(),
        });
    }

    let _guard = lock_sidecar(path, FlockOperation::LockShared)?;
    if !path.exists() {
        return Ok(Incremental {
            loaded: Loaded {
                samples: Vec::new(),
                skipped: 0,
            },
            cursor: Cursor::default(),
            reset: cursor != Cursor::default(),
        });
    }

    let mut file = fs::File::open(path)?;
    let meta = file.metadata()?;
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt;
        meta.ino()
    };
    #[cfg(not(unix))]
    let ino = 0;

    let (reset, start) =
        if cursor == Cursor::default() || ino != cursor.ino || meta.len() < cursor.offset {
            (true, 0u64)
        } else {
            (false, cursor.offset)
        };

    file.seek(SeekFrom::Start(start))?;

    let mut reader = BufReader::new(file);
    let mut samples = Vec::new();
    let mut skipped = 0;
    let mut consumed = 0u64;
    let mut line_buf = Vec::new();

    while reader.read_until(b'\n', &mut line_buf)? > 0 {
        if !line_buf.ends_with(b"\n") {
            break;
        }
        consumed += line_buf.len() as u64;
        match parse_line(&line_buf) {
            Some(Ok(sample)) => samples.push(sample),
            Some(Err(())) => skipped += 1,
            None => {}
        }
        line_buf.clear();
    }

    Ok(Incremental {
        loaded: Loaded { samples, skipped },
        cursor: Cursor {
            ino,
            offset: start + consumed,
        },
        reset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use tempfile::tempdir;

    fn make_sample(id: &str, remaining: f64) -> Sample {
        Sample {
            ts: Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap(),
            group: "Gemini Models".to_string(),
            bucket_id: id.to_string(),
            window: "weekly".to_string(),
            remaining,
            reset_time: Some(Utc.with_ymd_and_hms(2026, 10, 16, 12, 0, 0).unwrap()),
            disabled: false,
        }
    }

    #[test]
    fn round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");

        let s1 = make_sample("b1", 0.9);
        let s2 = make_sample("b2", 0.8);
        append(&path, &[s1.clone(), s2.clone()]).unwrap();

        let s3 = make_sample("b3", 0.7);
        append(&path, std::slice::from_ref(&s3)).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.skipped, 0);
        assert_eq!(loaded.samples, vec![s1, s2, s3]);
    }

    #[test]
    fn corrupt_line_skipped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");

        let s1 = make_sample("b1", 0.9);
        let valid_line = serde_json::to_string(&s1).unwrap();
        let content = format!("{{bad\n{}\n\n", valid_line);
        fs::write(&path, content).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.skipped, 1);
        assert_eq!(loaded.samples, vec![s1]);
    }

    #[test]
    fn invalid_utf8_line_skipped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");

        let s1 = make_sample("b1", 0.9);
        let mut content = Vec::new();
        content.extend_from_slice(b"\xff\xfe\xfd\n");
        serde_json::to_writer(&mut content, &s1).unwrap();
        content.push(b'\n');
        fs::write(&path, content).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.skipped, 1);
        assert_eq!(loaded.samples, vec![s1]);
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.skipped, 0);
        assert_eq!(loaded.samples, Vec::<Sample>::new());
    }

    #[test]
    fn creates_parent_dir() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("samples.jsonl");

        let s1 = make_sample("b1", 0.9);
        append(&path, std::slice::from_ref(&s1)).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.samples, vec![s1]);
    }

    #[test]
    fn concurrent_append() {
        use std::sync::Arc;
        use std::thread;

        let dir = tempdir().unwrap();
        let path = Arc::new(dir.path().join("samples.jsonl"));
        let mut handles = Vec::new();

        for i in 0..10 {
            let p = Arc::clone(&path);
            handles.push(thread::spawn(move || {
                let s = make_sample(&format!("b{i}"), 0.5);
                append(&p, &[s]).unwrap();
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.skipped, 0);
        assert_eq!(loaded.samples.len(), 10);
    }

    #[test]
    fn oldest_timestamp_skips_empty_and_corrupt_leading_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");

        assert_eq!(oldest_timestamp(&path).unwrap(), None);

        let s1 = make_sample("b1", 0.9);
        let s2 = make_sample("b2", 0.8);

        let mut content: Vec<u8> = b"\n{not json}\n\xff\n".to_vec();
        content.extend_from_slice((serde_json::to_string(&s1).unwrap() + "\n").as_bytes());
        content.extend_from_slice((serde_json::to_string(&s2).unwrap() + "\n").as_bytes());
        fs::write(&path, content).unwrap();

        assert_eq!(oldest_timestamp(&path).unwrap(), Some(s1.ts));
    }

    #[test]
    fn prune_missing_file_is_noop() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        let cutoff = Utc.with_ymd_and_hms(2026, 10, 11, 0, 0, 0).unwrap();
        assert_eq!(prune(&path, cutoff).unwrap(), 0);
    }

    #[test]
    fn prune_filters_old_records_and_skips_corrupt_lines() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");

        let cutoff = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();

        let mut old_sample = make_sample("old", 0.9);
        old_sample.ts = Utc.with_ymd_and_hms(2026, 9, 30, 23, 59, 59).unwrap();

        let mut boundary_sample = make_sample("boundary", 0.8);
        boundary_sample.ts = cutoff;

        let mut new_sample = make_sample("new", 0.7);
        new_sample.ts = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 1).unwrap();

        append(&path, &[old_sample]).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{bad json\n\xff\n")
            .unwrap();
        append(&path, &[boundary_sample.clone(), new_sample.clone()]).unwrap();

        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let pruned = prune(&path, cutoff).unwrap();
        assert_eq!(pruned, 1);

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.skipped, 0);
        assert_eq!(loaded.samples, vec![boundary_sample, new_sample]);

        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn prune_concurrent_with_append_no_lost_writes() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let dir = tempdir().unwrap();
        let path = Arc::new(dir.path().join("samples.jsonl"));

        let cutoff = Utc.with_ymd_and_hms(2026, 10, 11, 0, 0, 0).unwrap();

        // Seed sample before cutoff
        let mut seed = make_sample("seed", 1.0);
        seed.ts = Utc.with_ymd_and_hms(2026, 10, 10, 0, 0, 0).unwrap();
        append(&path, &[seed]).unwrap();

        let barrier = Arc::new(Barrier::new(11));

        let p_clone = Arc::clone(&path);
        let b_clone = Arc::clone(&barrier);
        let prune_handle = thread::spawn(move || {
            b_clone.wait();
            prune(&p_clone, cutoff).unwrap()
        });

        let mut appends = Vec::new();
        for i in 0..10 {
            let p = Arc::clone(&path);
            let b = Arc::clone(&barrier);
            appends.push(thread::spawn(move || {
                let mut s = make_sample(&format!("app-{i}"), 0.5);
                s.ts = Utc.with_ymd_and_hms(2026, 10, 11, 1, 0, i as u32).unwrap();
                b.wait();
                append(&p, &[s]).unwrap();
            }));
        }

        prune_handle.join().unwrap();
        for h in appends {
            h.join().unwrap();
        }

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.skipped, 0);
        // All 10 appended samples must survive
        assert_eq!(loaded.samples.len(), 10);
    }

    #[test]
    fn load_from_incremental() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");

        let s1 = make_sample("b1", 0.9);
        append(&path, std::slice::from_ref(&s1)).unwrap();
        let r1 = load_from(&path, Cursor::default()).unwrap();
        assert_eq!(r1.loaded.samples, vec![s1]);
        assert!(r1.reset); // first load from the default cursor is a reset
        assert!(r1.cursor.offset > 0);

        let s2 = make_sample("b2", 0.8);
        append(&path, std::slice::from_ref(&s2)).unwrap();
        let r2 = load_from(&path, r1.cursor).unwrap();
        assert_eq!(r2.loaded.samples, vec![s2]);
        assert!(!r2.reset);
        assert!(r2.cursor.offset > r1.cursor.offset);

        let r3 = load_from(&path, r2.cursor).unwrap();
        assert!(r3.loaded.samples.is_empty());
        assert!(!r3.reset);
        assert_eq!(r3.cursor, r2.cursor);
    }

    #[test]
    fn load_from_resets_after_prune_rename_even_if_file_grew() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        let cutoff = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        let new_ts = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();

        let mut old = make_sample("old", 0.9);
        old.ts = Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap();
        let mut kept = make_sample("kept", 0.8);
        kept.ts = new_ts;
        append(&path, &[old, kept.clone()]).unwrap();
        let before = load_from(&path, Cursor::default()).unwrap();

        prune(&path, cutoff).unwrap();
        let mut extra = Vec::new();
        for i in 0..3 {
            let mut s = make_sample(&format!("x{i}"), 0.5);
            s.ts = new_ts;
            extra.push(s);
        }
        append(&path, &extra).unwrap();
        assert!(fs::metadata(&path).unwrap().len() > before.cursor.offset);

        let after = load_from(&path, before.cursor).unwrap();
        assert!(after.reset);
        let mut expected = vec![kept];
        expected.extend(extra);
        assert_eq!(after.loaded.samples, expected);
    }

    #[test]
    fn load_from_resets_after_in_place_truncation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        append(&path, &[make_sample("b1", 0.9), make_sample("b2", 0.8)]).unwrap();
        let before = load_from(&path, Cursor::default()).unwrap();

        let s3 = make_sample("b3", 0.7);
        fs::write(&path, serde_json::to_string(&s3).unwrap() + "\n").unwrap(); // same inode, shorter

        let after = load_from(&path, before.cursor).unwrap();
        assert!(after.reset);
        assert_eq!(after.loaded.samples, vec![s3]);
        assert!(after.cursor.offset < before.cursor.offset);
    }

    #[test]
    fn load_from_does_not_consume_partial_trailing_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        let s1 = make_sample("b1", 0.9);
        let line = serde_json::to_string(&s1).unwrap() + "\n";
        fs::write(&path, format!("{line}{{\"ts\":\"2026-")).unwrap();

        let r = load_from(&path, Cursor::default()).unwrap();
        assert_eq!(r.loaded.samples, vec![s1]);
        assert_eq!(r.loaded.skipped, 0);
        assert_eq!(r.cursor.offset, line.len() as u64);
    }

    #[test]
    fn load_from_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        let r = load_from(
            &path,
            Cursor {
                ino: 42,
                offset: 100,
            },
        )
        .unwrap();
        assert!(r.loaded.samples.is_empty());
        assert!(r.reset);
        assert_eq!(r.cursor, Cursor::default());
    }

    fn sample_at(id: &str, ts: DateTime<Utc>) -> Sample {
        Sample {
            ts,
            ..make_sample(id, 0.5)
        }
    }

    #[test]
    fn load_since_keeps_only_samples_at_or_after_cutoff() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        let cutoff = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let old = sample_at("old", cutoff - chrono::Duration::seconds(1));
        let at = sample_at("at", cutoff);
        let new = sample_at("new", cutoff + chrono::Duration::days(3));
        // Out of chronological order on purpose (e.g. after a backwards clock jump).
        append(&path, &[new.clone(), old.clone(), at.clone()]).unwrap();

        let loaded = load_since(&path, cutoff).unwrap();
        assert_eq!(loaded.skipped, 0);
        assert_eq!(loaded.samples, vec![new.clone(), at.clone()]);
        assert!(!loaded.samples.contains(&old));

        // The unbounded loader still sees everything.
        assert_eq!(load(&path).unwrap().samples, vec![new, old, at]);
    }

    #[test]
    fn load_since_counts_unreadable_and_corrupt_recent_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        let cutoff = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let keep = sample_at("keep", cutoff + chrono::Duration::hours(1));
        let mut content = Vec::new();
        content.extend_from_slice(b"{bad\n");
        content.extend_from_slice(b"{\"ts\":\"2026-09-01T00:00:00Z\",\"group\":\"g\"}\n");
        content.extend_from_slice(b"{\"ts\":\"2026-10-02T00:00:00Z\",\"group\":\"g\"}\n");
        content.extend_from_slice(b"\xff\n\n");
        content.extend_from_slice(serde_json::to_string(&keep).unwrap().as_bytes());
        content.extend_from_slice(b"\r\n");
        fs::write(&path, content).unwrap();

        let loaded = load_since(&path, cutoff).unwrap();
        // `{bad`, the invalid UTF-8 line, and the recent-but-incomplete record are
        // skipped; the old incomplete record is discarded by timestamp alone.
        assert_eq!(loaded.skipped, 3);
        assert_eq!(loaded.samples, vec![keep]);
    }

    #[test]
    fn load_since_missing_file_is_empty() {
        let dir = tempdir().unwrap();
        let loaded = load_since(&dir.path().join("none.jsonl"), Utc::now()).unwrap();
        assert!(loaded.samples.is_empty());
        assert_eq!(loaded.skipped, 0);
    }

    #[test]
    fn prune_output_round_trips_through_buffered_writer() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        let cutoff = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        // Enough records to exceed BufWriter's internal buffer several times.
        let kept: Vec<Sample> = (0..2000)
            .map(|i| sample_at(&format!("b{i}"), cutoff + chrono::Duration::minutes(i)))
            .collect();
        let dropped: Vec<Sample> = (1..=500)
            .map(|i| sample_at("old", cutoff - chrono::Duration::minutes(i)))
            .collect();
        append(&path, &dropped).unwrap();
        append(&path, &kept).unwrap();

        assert_eq!(prune(&path, cutoff).unwrap(), 500);
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.skipped, 0);
        assert_eq!(loaded.samples, kept);
        assert!(fs::read_to_string(&path).unwrap().ends_with("}\n"));
    }
}
