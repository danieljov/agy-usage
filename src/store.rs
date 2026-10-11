use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use chrono::{DateTime, Utc};
use rustix::fs::{FlockOperation, flock};

use crate::model::Sample;

#[derive(Debug, PartialEq, Clone)]
pub struct Loaded {
    pub samples: Vec<Sample>,
    pub skipped: usize,
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
    if let Some(parent) = path.parent() {
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

fn parse_line(raw: &[u8]) -> Option<Result<Sample, ()>> {
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
                Some(serde_json::from_str::<Sample>(trimmed).map_err(|_| ()))
            }
        }
        Err(_) => Some(Err(())),
    }
}

pub fn append(path: &Path, samples: &[Sample]) -> std::io::Result<()> {
    if samples.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
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
        match parse_line(&line_buf) {
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
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
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
    let mut pruned = 0;
    let mut line_buf = Vec::new();

    while reader.read_until(b'\n', &mut line_buf)? > 0 {
        match parse_line(&line_buf) {
            Some(Ok(sample)) => {
                if sample.ts >= cutoff {
                    serde_json::to_writer(&mut temp, &sample)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                    temp.write_all(b"\n")?;
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

    #[cfg(unix)]
    let _ = temp.as_file().set_permissions(permissions);

    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(pruned)
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
            reset_time: Utc.with_ymd_and_hms(2026, 10, 16, 12, 0, 0).unwrap(),
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
}
