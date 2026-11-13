use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
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

struct BorrowedLockGuard<'a> {
    file: &'a fs::File,
}

impl Drop for BorrowedLockGuard<'_> {
    fn drop(&mut self) {
        let _ = flock(self.file, FlockOperation::Unlock);
    }
}

struct ReadSession {
    _sidecar: Option<LockGuard>,
    file: fs::File,
    unlock_file: bool,
}

impl Drop for ReadSession {
    fn drop(&mut self) {
        if self.unlock_file {
            let _ = flock(&self.file, FlockOperation::Unlock);
        }
    }
}

fn lock_borrowed(file: &fs::File, op: FlockOperation) -> std::io::Result<BorrowedLockGuard<'_>> {
    flock(file, op).map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
    Ok(BorrowedLockGuard { file })
}

fn ensure_regular_file(file: fs::File, path: &Path) -> std::io::Result<fs::File> {
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok(file)
}

fn validate_store_file_path(path: &Path) -> std::io::Result<()> {
    let ends_with_sep = {
        #[cfg(unix)]
        {
            path.as_os_str().as_encoded_bytes().ends_with(b"/")
        }
        #[cfg(not(unix))]
        {
            let bytes = path.as_os_str().as_encoded_bytes();
            bytes.ends_with(b"/") || bytes.ends_with(b"\\")
        }
    };
    if path.file_name().is_none() || ends_with_sep {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok(())
}

fn open_regular_with_options(
    opts: &mut OpenOptions,
    path: &Path,
    nofollow: bool,
) -> std::io::Result<fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Use O_NONBLOCK so FIFOs/blocking device nodes fail at ensure_regular_file instead of
        // hanging inside open(2).
        let mut flags = rustix::fs::OFlags::NONBLOCK;
        if nofollow {
            flags |= rustix::fs::OFlags::NOFOLLOW;
        }
        opts.custom_flags(flags.bits() as i32);
    }
    #[cfg(not(unix))]
    let _ = nofollow;
    let file = opts.open(path)?;
    ensure_regular_file(file, path)
}

fn lock_sidecar_exclusive(path: &Path) -> std::io::Result<LockGuard> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("lock");
    let file = open_regular_with_options(
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false),
        &lock_path,
        true,
    )?;
    flock(&file, FlockOperation::LockExclusive)
        .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
    Ok(LockGuard { file })
}

fn open_file_or_missing(path: &Path) -> std::io::Result<Option<fs::File>> {
    match open_regular_with_options(OpenOptions::new().read(true), path, false) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn finish_sidecar_read(lock_file: fs::File, path: &Path) -> std::io::Result<Option<ReadSession>> {
    flock(&lock_file, FlockOperation::LockShared)
        .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
    let sidecar = LockGuard { file: lock_file };
    let Some(file) = open_file_or_missing(path)? else {
        return Ok(None);
    };
    Ok(Some(ReadSession {
        _sidecar: Some(sidecar),
        file,
        unlock_file: false,
    }))
}

/// Opens `path` for reading under a shared lock without mutating read-only directories.
///
/// Only `ErrorKind::NotFound` treats a missing sidecar lockfile or data file as absent;
/// permission and I/O errors are propagated. When `samples.lock` does not exist yet,
/// takes a shared `flock` on `samples.jsonl` itself and re-checks the sidecar lockfile
/// (retaking the sidecar lock first if a writer initialized it concurrently) so readers
/// never scan `samples.jsonl` unsynchronized against a concurrent writer.
fn open_for_read(path: &Path) -> std::io::Result<Option<ReadSession>> {
    let lock_path = path.with_extension("lock");
    match open_regular_with_options(OpenOptions::new().read(true), &lock_path, true) {
        Ok(lock_file) => finish_sidecar_read(lock_file, path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let Some(file) = open_file_or_missing(path)? else {
                return Ok(None);
            };
            flock(&file, FlockOperation::LockShared)
                .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;

            // Re-check after locking `samples.jsonl` in case a writer created `samples.lock`
            // in the window above. Release `file`'s flock before locking `samples.lock` so
            // our lock acquisition order never inverts against writers (`sidecar -> data`).
            match open_regular_with_options(OpenOptions::new().read(true), &lock_path, true) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Some(ReadSession {
                    _sidecar: None,
                    file,
                    unlock_file: true,
                })),
                Err(e) => {
                    let _ = flock(&file, FlockOperation::Unlock);
                    Err(e)
                }
                Ok(lock_file) => {
                    let _ = flock(&file, FlockOperation::Unlock);
                    drop(file);
                    finish_sidecar_read(lock_file, path)
                }
            }
        }
        Err(e) => Err(e),
    }
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
    validate_store_file_path(path)?;
    match fs::metadata(path) {
        Ok(meta) if !meta.is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
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
    let _sidecar = lock_sidecar_exclusive(path)?;
    let file = open_regular_with_options(
        OpenOptions::new().create(true).read(true).append(true),
        path,
        false,
    )?;
    let _data_lock = lock_borrowed(&file, FlockOperation::LockExclusive)?;
    let needs_leading_newline = if file.metadata()?.len() > 0 {
        let mut reader = &file;
        reader.seek(SeekFrom::End(-1))?;
        let mut last = [0u8; 1];
        reader.read_exact(&mut last)?;
        last[0] != b'\n'
    } else {
        false
    };
    if needs_leading_newline {
        let mut prefixed = Vec::with_capacity(buffer.len() + 1);
        prefixed.push(b'\n');
        prefixed.extend_from_slice(&buffer);
        (&file).write_all(&prefixed)?;
    } else {
        (&file).write_all(&buffer)?;
    }
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
    let Some(session) = open_for_read(path)? else {
        return Ok(Loaded {
            samples: Vec::new(),
            skipped: 0,
        });
    };
    let mut reader = BufReader::new(&session.file);
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
    let Some(session) = open_for_read(path)? else {
        return Ok(None);
    };
    let mut reader = BufReader::new(&session.file);
    let mut line_buf = Vec::new();
    let mut oldest: Option<DateTime<Utc>> = None;

    while reader.read_until(b'\n', &mut line_buf)? > 0 {
        if let Some(Ok(line)) = trimmed_line(&line_buf)
            && let Ok(TsOnly { ts }) = serde_json::from_str::<TsOnly>(line)
            && oldest.is_none_or(|prev| ts < prev)
            && serde_json::from_str::<Sample>(line).is_ok()
        {
            oldest = Some(ts);
        }
        line_buf.clear();
    }
    Ok(oldest)
}

pub fn prune(path: &Path, cutoff: DateTime<Utc>) -> std::io::Result<usize> {
    validate_store_file_path(path)?;
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let _sidecar = lock_sidecar_exclusive(path)?;
    let Some(file) = open_file_or_missing(path)? else {
        return Ok(0);
    };
    let _data_lock = lock_borrowed(&file, FlockOperation::LockExclusive)?;
    #[cfg(unix)]
    let permissions = file.metadata()?.permissions();

    let mut reader = BufReader::new(&file);
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
    let Some(session) = open_for_read(path)? else {
        return Ok(Incremental {
            loaded: Loaded {
                samples: Vec::new(),
                skipped: 0,
            },
            cursor: Cursor::default(),
            reset: cursor != Cursor::default(),
        });
    };

    let mut file = &session.file;
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

/// Constant-memory summary of `samples.jsonl` used by diagnostics like `doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreSummary {
    pub count: usize,
    pub skipped: usize,
    pub newest: Option<DateTime<Utc>>,
}

/// Streams `path` under a shared lock without retaining samples in memory.
///
/// Missing files yield an empty `StoreSummary`; unreadable or non-regular files propagate the I/O error.
pub fn summarize(path: &Path) -> std::io::Result<StoreSummary> {
    let Some(session) = open_for_read(path)? else {
        return Ok(StoreSummary {
            count: 0,
            skipped: 0,
            newest: None,
        });
    };

    let mut reader = BufReader::new(&session.file);
    let mut summary = StoreSummary {
        count: 0,
        skipped: 0,
        newest: None,
    };
    let mut line_buf = Vec::new();

    while reader.read_until(b'\n', &mut line_buf)? > 0 {
        match parse_line(&line_buf) {
            Some(Ok(sample)) => {
                summary.count += 1;
                summary.newest = Some(match summary.newest {
                    Some(prev) => prev.max(sample.ts),
                    None => sample.ts,
                });
            }
            Some(Err(())) => summary.skipped += 1,
            None => {}
        }
        line_buf.clear();
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use tempfile::tempdir;

    #[test]
    fn summarize_streams_count_skipped_and_newest_without_missing_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");

        assert_eq!(
            summarize(&path).unwrap(),
            StoreSummary {
                count: 0,
                skipped: 0,
                newest: None,
            }
        );

        let t1 = Utc.with_ymd_and_hms(2026, 10, 12, 10, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 10, 12, 11, 0, 0).unwrap();
        let mut s1 = make_sample("b", 0.9);
        s1.ts = t1;
        let mut s2 = make_sample("b", 0.8);
        s2.ts = t2;
        append(&path, &[s2, s1]).unwrap();
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{corrupt").unwrap();
        write!(f, "{{\"partial\":true").unwrap();

        let summary = summarize(&path).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(summary.count, 2);
        assert_eq!(summary.skipped, 2);
        assert_eq!(summary.count, loaded.samples.len());
        assert_eq!(summary.skipped, loaded.skipped);
        assert_eq!(summary.newest, Some(t2));
    }

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

    #[test]
    fn read_operations_never_create_lockfile_and_work_without_lockfile() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        let lock_path = path.with_extension("lock");
        let ts = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let sample = sample_at("b1", ts);
        let line = serde_json::to_string(&sample).unwrap() + "\n";
        fs::write(&path, line).unwrap();
        assert!(!lock_path.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        }

        let all = load(&path).unwrap();
        assert_eq!(all.samples, vec![sample.clone()]);
        let since = load_since(&path, ts - chrono::Duration::hours(1)).unwrap();
        assert_eq!(since.samples, vec![sample.clone()]);
        assert_eq!(oldest_timestamp(&path).unwrap(), Some(ts));
        let inc = load_from(&path, Cursor::default()).unwrap();
        assert_eq!(inc.loaded.samples, vec![sample]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(!lock_path.exists(), "readers must not create samples.lock");
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_lockfile_propagates_error_instead_of_unlocked_read() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        let lock_path = path.with_extension("lock");
        let ts = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        append(&path, &[sample_at("b1", ts)]).unwrap();
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o000)).unwrap();

        if fs::File::open(&lock_path).is_err() {
            assert!(load(&path).is_err());
            assert!(load_since(&path, ts).is_err());
            assert!(oldest_timestamp(&path).is_err());
            assert!(load_from(&path, Cursor::default()).is_err());
            assert!(summarize(&path).is_err());
        }

        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn inaccessible_parent_dir_propagates_error_across_readers() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let sub = dir.path().join("locked");
        fs::create_dir_all(&sub).unwrap();
        let path = sub.join("samples.jsonl");
        let ts = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        fs::write(
            &path,
            serde_json::to_string(&sample_at("b1", ts)).unwrap() + "\n",
        )
        .unwrap();
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();

        if fs::File::open(&path).is_err() {
            assert!(summarize(&path).is_err());
            assert!(load(&path).is_err());
            assert!(load_since(&path, ts).is_err());
            assert!(oldest_timestamp(&path).is_err());
            assert!(load_from(&path, Cursor::default()).is_err());
        }

        fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn concurrent_readers_and_writers_without_initial_lockfile_see_no_partial_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        let base_ts = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        // Seed store externally without creating samples.lock.
        fs::write(
            &path,
            serde_json::to_string(&sample_at("seed", base_ts)).unwrap() + "\n",
        )
        .unwrap();
        assert!(!path.with_extension("lock").exists());

        std::thread::scope(|s| {
            s.spawn(|| {
                for i in 1..=40 {
                    let batch = vec![
                        sample_at(&format!("b{i}"), base_ts + chrono::Duration::minutes(i)),
                        sample_at(&format!("c{i}"), base_ts + chrono::Duration::minutes(i)),
                    ];
                    append(&path, &batch).unwrap();
                    if i % 10 == 0 {
                        prune(&path, base_ts + chrono::Duration::minutes(i - 5)).unwrap();
                    }
                }
            });
            for _ in 0..2 {
                s.spawn(|| {
                    let mut cursor = Cursor::default();
                    for _ in 0..40 {
                        let loaded = load(&path).unwrap();
                        assert_eq!(loaded.skipped, 0);
                        let sum = summarize(&path).unwrap();
                        assert_eq!(sum.skipped, 0);
                        assert!(sum.count >= 1);
                        let inc = load_from(&path, cursor).unwrap();
                        assert_eq!(inc.loaded.skipped, 0);
                        cursor = inc.cursor;
                    }
                });
            }
        });
    }

    #[test]
    fn append_isolates_unterminated_partial_line_and_skips_extra_newline_when_terminated() {
        let dir = tempdir().unwrap();
        let partial_path = dir.path().join("partial.jsonl");
        let ts = Utc.with_ymd_and_hms(2026, 11, 13, 10, 0, 0).unwrap();
        fs::write(
            &partial_path,
            r#"{"ts":"2026-11-13T10:00:00Z","partial":true"#,
        )
        .unwrap();

        append(&partial_path, &[sample_at("b1", ts), sample_at("b2", ts)]).unwrap();
        let loaded = load(&partial_path).unwrap();
        assert_eq!(loaded.skipped, 1);
        assert_eq!(loaded.samples.len(), 2);

        // Negative boundary assertion: already newline-terminated store receives no blank separator line.
        let clean_path = dir.path().join("clean.jsonl");
        append(&clean_path, &[sample_at("b1", ts)]).unwrap();
        append(&clean_path, &[sample_at("b2", ts)]).unwrap();
        let raw = fs::read_to_string(&clean_path).unwrap();
        assert_eq!(raw.lines().count(), 2);
        assert!(!raw.contains("\n\n"));
    }

    #[test]
    fn oldest_timestamp_finds_minimum_across_out_of_order_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        let recent = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        let old = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        fs::write(
            &path,
            format!(
                "{{\"ts\":\"2020-01-01T00:00:00Z\",\"partial\":true}}\n{}\n{}\n",
                serde_json::to_string(&sample_at("new", recent)).unwrap(),
                serde_json::to_string(&sample_at("old", old)).unwrap(),
            ),
        )
        .unwrap();

        assert_eq!(oldest_timestamp(&path).unwrap(), Some(old));
    }

    #[test]
    fn directory_at_lock_or_data_path_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        let ts = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        fs::write(
            &path,
            serde_json::to_string(&sample_at("b1", ts)).unwrap() + "\n",
        )
        .unwrap();
        fs::create_dir_all(path.with_extension("lock")).unwrap();

        assert!(load(&path).is_err());
        assert!(load_since(&path, ts).is_err());
        assert!(oldest_timestamp(&path).is_err());
        assert!(summarize(&path).is_err());
        assert!(append(&path, &[sample_at("b2", ts)]).is_err());
        assert!(prune(&path, ts).is_err());

        let dir_store = tempdir().unwrap();
        let dir_path = dir_store.path().join("samples.jsonl");
        fs::create_dir_all(&dir_path).unwrap();
        assert!(load(&dir_path).is_err());
        assert!(load_since(&dir_path, ts).is_err());
        assert!(oldest_timestamp(&dir_path).is_err());
        assert!(summarize(&dir_path).is_err());
        assert!(append(&dir_path, &[sample_at("b2", ts)]).is_err());
        assert!(prune(&dir_path, ts).is_err());
        assert!(!dir_path.with_extension("lock").exists());

        let slash_path = dir_store.path().join("samples.jsonl/");
        assert!(append(&slash_path, &[sample_at("b2", ts)]).is_err());
        assert!(prune(&slash_path, ts).is_err());
        assert!(!dir_store.path().join("samples.lock").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            use std::process::Command;

            // Dangling or target symlinks at samples.lock must be rejected without creating targets.
            let sym_dir = tempdir().unwrap();
            let sym_data = sym_dir.path().join("samples.jsonl");
            let sym_lock = sym_dir.path().join("samples.lock");
            let stray_target = sym_dir.path().join("stray_lock_target");
            fs::write(
                &sym_data,
                serde_json::to_string(&sample_at("b1", ts)).unwrap() + "\n",
            )
            .unwrap();
            symlink(&stray_target, &sym_lock).unwrap();
            assert!(load(&sym_data).is_err());
            assert!(summarize(&sym_data).is_err());
            assert!(append(&sym_data, &[sample_at("b2", ts)]).is_err());
            assert!(!stray_target.exists());

            // FIFOs at samples.lock or samples.jsonl must fail without blocking.
            let fifo_lock_dir = tempdir().unwrap();
            let fifo_data = fifo_lock_dir.path().join("samples.jsonl");
            let fifo_lock = fifo_lock_dir.path().join("samples.lock");
            fs::write(
                &fifo_data,
                serde_json::to_string(&sample_at("b1", ts)).unwrap() + "\n",
            )
            .unwrap();
            let st1 = Command::new("mkfifo").arg(&fifo_lock).status().unwrap();
            assert!(st1.success());
            assert!(load(&fifo_data).is_err());
            assert!(summarize(&fifo_data).is_err());
            assert!(append(&fifo_data, &[sample_at("b2", ts)]).is_err());

            let fifo_data_dir = tempdir().unwrap();
            let fifo_store = fifo_data_dir.path().join("samples.jsonl");
            let st2 = Command::new("mkfifo").arg(&fifo_store).status().unwrap();
            assert!(st2.success());
            assert!(load(&fifo_store).is_err());
            assert!(summarize(&fifo_store).is_err());
            assert!(append(&fifo_store, &[sample_at("b2", ts)]).is_err());
            assert!(prune(&fifo_store, ts).is_err());
            assert!(!fifo_store.with_extension("lock").exists());
        }
    }

    #[test]
    fn summarize_matches_load_on_unterminated_final_lines() {
        let dir = tempdir().unwrap();
        let ts = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();

        let valid_no_nl = dir.path().join("valid.jsonl");
        fs::write(
            &valid_no_nl,
            serde_json::to_string(&sample_at("b1", ts)).unwrap(),
        )
        .unwrap();
        let loaded_v = load(&valid_no_nl).unwrap();
        let sum_v = summarize(&valid_no_nl).unwrap();
        assert_eq!(sum_v.count, loaded_v.samples.len());
        assert_eq!(sum_v.skipped, loaded_v.skipped);

        let corrupt_no_nl = dir.path().join("corrupt.jsonl");
        fs::write(
            &corrupt_no_nl,
            format!(
                "{}\n{{\"partial\":true",
                serde_json::to_string(&sample_at("b1", ts)).unwrap()
            ),
        )
        .unwrap();
        let loaded_c = load(&corrupt_no_nl).unwrap();
        let sum_c = summarize(&corrupt_no_nl).unwrap();
        assert_eq!(sum_c.count, loaded_c.samples.len());
        assert_eq!(sum_c.skipped, loaded_c.skipped);
    }
}
