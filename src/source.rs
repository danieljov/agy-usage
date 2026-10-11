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
                    Some(r) if r.is_finite() => r.clamp(0.0, 1.0),
                    _ => continue,
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

const DEFAULT_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

pub fn fetch(agy_bin: &str) -> Result<String> {
    fetch_with_timeout(agy_bin, DEFAULT_FETCH_TIMEOUT)
}

pub fn fetch_with_timeout(agy_bin: &str, timeout: std::time::Duration) -> Result<String> {
    let mut cmd = Command::new(agy_bin);
    cmd.args(["-p", "/usage", "--output-format", "json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to execute agy binary: {}", agy_bin))?;

    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");

    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut stdout_pipe, &mut buf).map(|_| buf)
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut stderr_pipe, &mut buf).map(|_| buf)
    });

    let start = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().context("error waiting for agy process")? {
            break status;
        }
        if start.elapsed() >= timeout {
            #[cfg(unix)]
            if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            bail!("agy command timed out after {:?}", timeout);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };

    let stdout_bytes = stdout_handle
        .join()
        .map_err(|_| anyhow!("stdout reader thread panicked"))?
        .context("failed to read agy stdout")?;
    let stderr_bytes = stderr_handle
        .join()
        .map_err(|_| anyhow!("stderr reader thread panicked"))?
        .context("failed to read agy stderr")?;

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        bail!(
            "agy command failed with exit code {:?}: {}",
            status.code(),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(stdout_bytes).context("agy output was not valid UTF-8")?;
    let val: Value = serde_json::from_str(&stdout).context("agy output was not valid JSON")?;
    if val.get("status").and_then(Value::as_str) != Some("SUCCESS") {
        bail!("agy reported non-SUCCESS status: {:?}", val.get("status"));
    }

    Ok(stdout)
}

pub fn agy_bin() -> String {
    std::env::var("AGY_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "agy".to_string())
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
    fn out_of_bounds_remaining_fraction_skipped() {
        let ts = Utc.with_ymd_and_hms(2026, 10, 11, 17, 20, 0).unwrap();
        let json = r#"{
            "status": "SUCCESS",
            "command": {
                "name": "usage",
                "data": {
                    "groups": [
                        {
                            "name": "Validation Group",
                            "buckets": [
                                {
                                    "id": "nan",
                                    "window": "weekly",
                                    "remaining_fraction": "invalid",
                                    "reset_time": "2026-10-16T19:18:13Z"
                                },
                                {
                                    "id": "neg",
                                    "window": "weekly",
                                    "remaining_fraction": -0.05,
                                    "reset_time": "2026-10-16T19:18:13Z"
                                },
                                {
                                    "id": "over",
                                    "window": "weekly",
                                    "remaining_fraction": 1.05,
                                    "reset_time": "2026-10-16T19:18:13Z"
                                },
                                {
                                    "id": "zero",
                                    "window": "weekly",
                                    "remaining_fraction": 0.0,
                                    "reset_time": "2026-10-16T19:18:13Z"
                                },
                                {
                                    "id": "one",
                                    "window": "weekly",
                                    "remaining_fraction": 1.0,
                                    "reset_time": "2026-10-16T19:18:13Z"
                                }
                            ]
                        }
                    ]
                }
            }
        }"#;
        let samples = parse_usage(json, ts).unwrap();
        assert_eq!(samples.len(), 4);
        assert_eq!(samples[0].bucket_id, "neg");
        assert_eq!(samples[0].remaining, 0.0);
        assert_eq!(samples[1].bucket_id, "over");
        assert_eq!(samples[1].remaining, 1.0);
        assert_eq!(samples[2].bucket_id, "zero");
        assert_eq!(samples[2].remaining, 0.0);
        assert_eq!(samples[3].bucket_id, "one");
        assert_eq!(samples[3].remaining, 1.0);
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

    #[test]
    fn test_agy_bin_empty_env_fallback() {
        let res = std::env::var("AGY_BIN_NONEXISTENT_TEST_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "agy".to_string());
        assert_eq!(res, "agy");
    }

    #[test]
    #[cfg(unix)]
    fn fetch_timeout_kills_process() {
        let temp = tempfile::tempdir().unwrap();
        let slow_script = temp.path().join("slow.sh");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&slow_script, "#!/bin/sh\nexec sleep 10\n").unwrap();
            std::fs::set_permissions(&slow_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let start = std::time::Instant::now();
        let res = fetch_with_timeout(
            slow_script.to_str().unwrap(),
            std::time::Duration::from_millis(150),
        );
        assert!(res.is_err());
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "took too long: {:?}",
            elapsed
        );
        let err_msg = res.unwrap_err().to_string();
        assert!(err_msg.contains("timed out"), "unexpected err: {}", err_msg);
    }
}
