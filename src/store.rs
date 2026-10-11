use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

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
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut samples = Vec::new();
    let mut skipped = 0;
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Sample>(trimmed) {
            Ok(sample) => samples.push(sample),
            Err(_) => skipped += 1,
        }
    }
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
}
