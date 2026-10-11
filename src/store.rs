use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use rustix::fs::{FlockOperation, flock};

use crate::model::Sample;

#[derive(Debug, PartialEq, Clone)]
pub struct Loaded {
    pub samples: Vec<Sample>,
    pub skipped: usize,
}

pub fn append(path: &Path, samples: &[Sample]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut buffer = Vec::new();
    for sample in samples {
        serde_json::to_writer(&mut buffer, sample)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        buffer.push(b'\n');
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    flock(&file, FlockOperation::LockExclusive)
        .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
    let write_res = file.write_all(&buffer);
    let _ = flock(&file, FlockOperation::Unlock);
    write_res?;
    Ok(())
}

pub fn load(path: &Path) -> std::io::Result<Loaded> {
    if !path.exists() {
        return Ok(Loaded {
            samples: Vec::new(),
            skipped: 0,
        });
    }
    let file = fs::File::open(path)?;
    flock(&file, FlockOperation::LockShared)
        .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
    let mut reader = BufReader::new(file);
    let mut samples = Vec::new();
    let mut skipped = 0;
    let mut line_buf = Vec::new();

    while reader.read_until(b'\n', &mut line_buf)? > 0 {
        let trimmed_bytes = if line_buf.ends_with(b"\n") {
            let mut slice = &line_buf[..line_buf.len() - 1];
            if slice.ends_with(b"\r") {
                slice = &slice[..slice.len() - 1];
            }
            slice
        } else {
            &line_buf[..]
        };

        if let Ok(line_str) = std::str::from_utf8(trimmed_bytes) {
            let trimmed = line_str.trim();
            if !trimmed.is_empty() {
                match serde_json::from_str::<Sample>(trimmed) {
                    Ok(sample) => samples.push(sample),
                    Err(_) => skipped += 1,
                }
            }
        } else {
            skipped += 1;
        }
        line_buf.clear();
    }
    let _ = flock(reader.get_ref(), FlockOperation::Unlock);
    Ok(Loaded { samples, skipped })
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
}
