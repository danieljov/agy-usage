use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::Sample;

pub fn parse_usage(json: &str, ts: DateTime<Utc>) -> Result<Vec<Sample>> {
    let root: Value = serde_json::from_str(json).context("invalid JSON")?;
    let groups = root
        .pointer("/command/data/groups")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("missing command.data.groups in usage JSON"))?;

    let mut samples = Vec::new();

    for group in groups {
        let group_name = group
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if let Some(buckets) = group.get("buckets").and_then(Value::as_array) {
            for bucket in buckets {
                let remaining = match bucket.get("remaining_fraction").and_then(Value::as_f64) {
                    Some(r) => r,
                    None => continue,
                };

                let reset_str = match bucket.get("reset_time").and_then(Value::as_str) {
                    Some(s) => s,
                    None => continue,
                };

                let reset_time = match DateTime::parse_from_rfc3339(reset_str) {
                    Ok(dt) => dt.with_timezone(&Utc),
                    Err(_) => continue,
                };

                let bucket_id = bucket.get("id").and_then(Value::as_str).unwrap_or_default();
                let window = bucket
                    .get("window")
                    .and_then(Value::as_str)
                    .unwrap_or_default();

                samples.push(Sample {
                    ts,
                    group: group_name.to_string(),
                    bucket_id: bucket_id.to_string(),
                    window: window.to_string(),
                    remaining,
                    reset_time,
                });
            }
        }
    }

    Ok(samples)
}

pub fn fetch(agy_bin: &str) -> Result<String> {
    let output = Command::new(agy_bin)
        .args(["-p", "/usage", "--output-format", "json"])
        .output()
        .with_context(|| format!("failed to execute agy binary: {}", agy_bin))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "agy command failed with exit code {:?}: {}",
            output.status.code(),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("agy output was not valid UTF-8")?;
    let val: Value = serde_json::from_str(&stdout).context("agy output was not valid JSON")?;
    if val.get("status").and_then(Value::as_str) != Some("SUCCESS") {
        bail!("agy reported non-SUCCESS status: {:?}", val.get("status"));
    }

    Ok(stdout)
}

pub fn agy_bin() -> String {
    std::env::var("AGY_BIN").unwrap_or_else(|_| "agy".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    const FIXTURE_JSON: &str = include_str!("../tests/fixtures/usage.json");

    #[test]
    fn parses_fixture() {
        let ts = Utc.with_ymd_and_hms(2026, 10, 11, 17, 20, 0).unwrap();
        let samples = parse_usage(FIXTURE_JSON, ts).unwrap();
        assert_eq!(samples.len(), 4);

        assert_eq!(samples[0].group, "Gemini Models");
        assert_eq!(samples[0].bucket_id, "gemini-weekly");
        assert_eq!(samples[0].window, "weekly");
        assert!((samples[0].remaining - 0.7730404734611511).abs() < 1e-6);
        assert_eq!(
            samples[0].reset_time,
            Utc.with_ymd_and_hms(2026, 10, 16, 19, 18, 13).unwrap()
        );
        assert_eq!(samples[0].ts, ts);

        assert_eq!(samples[3].bucket_id, "3p-5h");
        for s in &samples {
            assert_eq!(s.ts, ts);
        }
    }

    #[test]
    fn disabled_bucket_skipped() {
        let ts = Utc.with_ymd_and_hms(2026, 10, 11, 17, 20, 0).unwrap();
        let json = r#"{
            "status": "SUCCESS",
            "command": {
                "name": "usage",
                "data": {
                    "groups": [
                        {
                            "name": "Test Group",
                            "buckets": [
                                {
                                    "id": "disabled-1",
                                    "window": "weekly",
                                    "reset_time": "2026-10-16T19:18:13Z"
                                },
                                {
                                    "id": "disabled-2",
                                    "window": "weekly",
                                    "remaining_fraction": null,
                                    "reset_time": "2026-10-16T19:18:13Z"
                                },
                                {
                                    "id": "enabled",
                                    "window": "weekly",
                                    "remaining_fraction": 0.5,
                                    "reset_time": "2026-10-16T19:18:13Z"
                                }
                            ]
                        }
                    ]
                }
            }
        }"#;
        let samples = parse_usage(json, ts).unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].bucket_id, "enabled");
    }

    #[test]
    fn malformed_json_errors() {
        let ts = Utc.with_ymd_and_hms(2026, 10, 11, 17, 20, 0).unwrap();
        assert!(parse_usage("not json", ts).is_err());
        assert!(parse_usage("{\"command\":{}}", ts).is_err());
    }

    #[test]
    fn fetch_reports_failure() {
        assert!(fetch("false").is_err());
        assert!(fetch("/nonexistent/agy").is_err());
    }
}
