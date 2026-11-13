use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::Sample;

pub fn is_executable(p: &Path) -> bool {
    p.is_file() && rustix::fs::access(p, rustix::fs::Access::EXEC_OK).is_ok()
}

pub fn resolve_agy_bin() -> Result<PathBuf> {
    resolve_agy_bin_os(
        std::env::var_os("AGY_BIN").as_deref(),
        std::env::var_os("PATH").as_deref(),
    )
}

pub fn resolve_agy_bin_from(agy_env: Option<&str>, path_env: Option<&str>) -> Result<PathBuf> {
    resolve_agy_bin_os(
        agy_env.map(std::ffi::OsStr::new),
        path_env.map(std::ffi::OsStr::new),
    )
}

fn find_command_in_path(
    name: &std::ffi::OsStr,
    path_env: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let path_var = path_env?;
    for dir in std::env::split_paths(path_var) {
        if !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_bare_command_name(val: &std::ffi::OsStr) -> bool {
    let s = val.to_string_lossy();
    if s.contains('/') || s.contains(std::path::MAIN_SEPARATOR) {
        return false;
    }
    let mut comps = Path::new(val).components();
    matches!(
        (comps.next(), comps.next()),
        (Some(std::path::Component::Normal(_)), None)
    )
}

fn resolve_executable(
    val: &std::ffi::OsStr,
    path_env: Option<&std::ffi::OsStr>,
    source_prefix: Option<&str>,
) -> Result<PathBuf> {
    if is_bare_command_name(val) {
        if let Some(found) = find_command_in_path(val, path_env) {
            return Ok(found);
        }
        if let Some(prefix) = source_prefix {
            bail!(
                "{prefix} '{}', which could not be found as an executable on PATH",
                Path::new(val).display()
            );
        }
        bail!(
            "executable '{}' could not be found on PATH",
            Path::new(val).display()
        );
    }
    let p = PathBuf::from(val);
    if is_executable(&p) {
        return Ok(p);
    }
    if let Some(prefix) = source_prefix {
        bail!(
            "{prefix} '{}', but it is not an executable file",
            p.display()
        );
    }
    bail!("'{}' is not an executable file", p.display());
}

pub fn resolve_agy_bin_os(
    agy_env: Option<&std::ffi::OsStr>,
    path_env: Option<&std::ffi::OsStr>,
) -> Result<PathBuf> {
    if let Some(val) = agy_env.filter(|s| !s.is_empty()) {
        return resolve_executable(val, path_env, Some("AGY_BIN is set to"));
    }
    if let Some(found) = find_command_in_path(std::ffi::OsStr::new("agy"), path_env) {
        return Ok(found);
    }
    bail!(
        "could not find executable 'agy' on PATH or via $AGY_BIN. Please ensure agy is installed or set AGY_BIN"
    );
}

/// Returns the configured `$AGY_BIN` override (as a path) if non-empty, or `"agy"`.
pub fn configured_agy_bin() -> PathBuf {
    std::env::var_os("AGY_BIN")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("agy"))
}

pub fn agy_bin() -> String {
    std::env::var("AGY_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "agy".to_string())
}

pub fn parse_usage_with_warnings(
    json: &str,
    ts: DateTime<Utc>,
) -> Result<(Vec<Sample>, Vec<String>)> {
    let root: Value = serde_json::from_str(json).context("invalid JSON")?;
    let groups = root
        .pointer("/command/data/groups")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("missing command.data.groups in usage JSON"))?;

    let mut samples = Vec::new();
    let mut warnings = Vec::new();

    for group in groups {
        let group_name = group
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if let Some(buckets) = group.get("buckets").and_then(Value::as_array) {
            for bucket in buckets {
                let bucket_id = bucket.get("id").and_then(Value::as_str).unwrap_or_default();
                let window = bucket
                    .get("window")
                    .and_then(Value::as_str)
                    .unwrap_or_default();

                let is_disabled = bucket
                    .get("disabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

                let reset_time = bucket
                    .get("reset_time")
                    .and_then(Value::as_str)
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc));

                let remaining_opt = bucket
                    .get("remaining_fraction")
                    .and_then(Value::as_f64)
                    .filter(|r| r.is_finite())
                    .map(|r| r.clamp(0.0, 1.0));

                if is_disabled {
                    samples.push(Sample {
                        ts,
                        group: group_name.to_string(),
                        bucket_id: bucket_id.to_string(),
                        window: window.to_string(),
                        remaining: remaining_opt.unwrap_or(0.0),
                        reset_time,
                        disabled: true,
                    });
                    continue;
                }

                let (remaining, reset_time) = match (remaining_opt, reset_time) {
                    (Some(r), Some(rt)) => (r, rt),
                    (r, rt) => {
                        let reason = match (r.is_none(), rt.is_none()) {
                            (true, true) => "missing or invalid remaining_fraction and reset_time",
                            (true, false) => "missing or invalid remaining_fraction",
                            (false, true) => "missing or invalid reset_time",
                            (false, false) => "invalid bucket fields",
                        };
                        let bucket_label = if bucket_id.is_empty() {
                            "<unknown>"
                        } else {
                            bucket_id
                        };
                        let group_label = if group_name.is_empty() {
                            "<unknown>"
                        } else {
                            group_name
                        };
                        warnings.push(format!(
                            "skipped bucket '{bucket_label}' in group '{group_label}': {reason}"
                        ));
                        continue;
                    }
                };

                samples.push(Sample {
                    ts,
                    group: group_name.to_string(),
                    bucket_id: bucket_id.to_string(),
                    window: window.to_string(),
                    remaining,
                    reset_time: Some(reset_time),
                    disabled: false,
                });
            }
        }
    }

    Ok((samples, warnings))
}

pub fn parse_usage(json: &str, ts: DateTime<Utc>) -> Result<Vec<Sample>> {
    Ok(parse_usage_with_warnings(json, ts)?.0)
}

const DEFAULT_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

pub fn fetch<P: AsRef<Path>>(agy_bin: P) -> Result<String> {
    fetch_with_timeout(agy_bin, DEFAULT_FETCH_TIMEOUT)
}

pub fn fetch_with_timeout<P: AsRef<Path>>(
    agy_bin: P,
    timeout: std::time::Duration,
) -> Result<String> {
    fetch_with_timeout_and_cancel(agy_bin, timeout, None, None)
}

pub fn fetch_with_timeout_and_cancel<P: AsRef<Path>>(
    agy_bin: P,
    timeout: std::time::Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    child_pid: Option<&std::sync::atomic::AtomicU32>,
) -> Result<String> {
    if let Some(c) = cancel
        && c.load(std::sync::atomic::Ordering::SeqCst)
    {
        bail!("fetch cancelled");
    }

    let resolved = resolve_executable(
        agy_bin.as_ref().as_os_str(),
        std::env::var_os("PATH").as_deref(),
        None,
    )?;

    let mut cmd = Command::new(&resolved);
    cmd.args(["-p", "/usage", "--output-format", "json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = {
        let mut attempts = 0;
        loop {
            if let Some(c) = cancel
                && c.load(std::sync::atomic::Ordering::SeqCst)
            {
                bail!("fetch cancelled");
            }
            match cmd.spawn() {
                Ok(child) => break child,
                Err(e)
                    if (e.kind() == std::io::ErrorKind::ExecutableFileBusy
                        || e.raw_os_error() == Some(26))
                        && attempts < 20 =>
                {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("failed to execute agy binary: {}", resolved.display())
                    });
                }
            }
        }
    };

    if let Some(cp) = child_pid {
        cp.store(child.id(), std::sync::atomic::Ordering::SeqCst);
    }

    struct PidGuard<'a>(Option<&'a std::sync::atomic::AtomicU32>);
    impl<'a> Drop for PidGuard<'a> {
        fn drop(&mut self) {
            if let Some(cp) = self.0 {
                cp.store(0, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }
    let _pid_guard = PidGuard(child_pid);

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

    let kill_process_and_group = |child: &mut std::process::Child| {
        #[cfg(unix)]
        if let Ok(raw_pid) = i32::try_from(child.id())
            && raw_pid > 0
            && let Some(pid) = rustix::process::Pid::from_raw(raw_pid)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    };

    let start = std::time::Instant::now();
    let status = loop {
        if let Some(c) = cancel
            && c.load(std::sync::atomic::Ordering::SeqCst)
        {
            kill_process_and_group(&mut child);
            bail!("fetch cancelled");
        }

        if start.elapsed() >= timeout {
            kill_process_and_group(&mut child);
            bail!("agy command timed out after {:?}", timeout);
        }

        let child_done = child.try_wait().context("error waiting for agy process")?;
        if let Some(status) = child_done
            && stdout_handle.is_finished()
            && stderr_handle.is_finished()
        {
            if let Some(cp) = child_pid {
                cp.store(0, std::sync::atomic::Ordering::SeqCst);
            }
            break status;
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    const FIXTURE_JSON: &str = include_str!("../tests/fixtures/usage.json");
    const PRO_FIXTURE_JSON: &str = include_str!("../tests/fixtures/usage_pro.json");

    #[test]
    fn parses_pro_fixture() {
        let ts = Utc.with_ymd_and_hms(2026, 10, 28, 23, 35, 0).unwrap();
        let samples = parse_usage(PRO_FIXTURE_JSON, ts).unwrap();
        assert_eq!(samples.len(), 5);

        assert_eq!(samples[0].group, "Gemini Models");
        assert_eq!(samples[0].bucket_id, "gemini-weekly");
        assert_eq!(samples[1].bucket_id, "gemini-5h");
        assert_eq!(samples[2].group, "Claude and GPT models");
        assert_eq!(samples[2].bucket_id, "3p-weekly");
        assert_eq!(samples[3].bucket_id, "3p-5h");

        assert_eq!(samples[4].group, "Gemini 4 Argon");
        assert_eq!(samples[4].bucket_id, "gemini-argon-wtu-weekly");
        assert_eq!(samples[4].window, "weekly");
        assert!((samples[4].remaining - 0.9983522891998291).abs() < 1e-9);
        assert_eq!(
            samples[4].reset_time,
            Some(Utc.with_ymd_and_hms(2026, 11, 4, 9, 49, 12).unwrap())
        );
        assert!(!samples[4].disabled);
    }

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
            Some(Utc.with_ymd_and_hms(2026, 10, 16, 19, 18, 13).unwrap())
        );
        assert_eq!(samples[0].ts, ts);
        assert!(!samples[0].disabled);

        assert_eq!(samples[3].bucket_id, "3p-5h");
        assert!(!samples[3].disabled);
        for s in &samples {
            assert_eq!(s.ts, ts);
        }
    }

    #[test]
    fn disabled_bucket_when_weekly_limit_reached() {
        let ts = Utc.with_ymd_and_hms(2026, 10, 12, 13, 35, 0).unwrap();
        let json = r#"{
            "status": "SUCCESS",
            "command": {
                "name": "usage",
                "data": {
                    "groups": [
                        {
                            "name": "Claude and GPT models",
                            "buckets": [
                                {
                                    "id": "3p-weekly",
                                    "name": "Weekly Limit Remaining",
                                    "window": "weekly",
                                    "remaining_fraction": 0,
                                    "reset_time": "2026-10-16T20:52:27Z"
                                },
                                {
                                    "id": "3p-5h",
                                    "name": "Five Hour Limit Remaining",
                                    "description": "You have hit your weekly limit, the 5-hour limit does not currently apply.",
                                    "window": "5h",
                                    "disabled": true,
                                    "remaining_fraction": 1
                                }
                            ]
                        }
                    ]
                }
            }
        }"#;
        let samples = parse_usage(json, ts).unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].bucket_id, "3p-weekly");
        assert!(!samples[0].disabled);
        assert_eq!(samples[0].remaining, 0.0);

        assert_eq!(samples[1].bucket_id, "3p-5h");
        assert!(samples[1].disabled);
        assert_eq!(samples[1].reset_time, None);
        assert_eq!(samples[1].remaining, 1.0);
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
            use std::io::Write;
            use std::os::unix::fs::PermissionsExt;
            let mut f = std::fs::File::create(&slow_script).unwrap();
            f.write_all(b"#!/bin/sh\nexec sleep 10\n").unwrap();
            f.sync_all().unwrap();
            drop(f);
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
        let err_msg = format!("{:#}", res.as_ref().unwrap_err());
        assert!(err_msg.contains("timed out"), "unexpected err: {}", err_msg);
    }

    #[test]
    #[cfg(unix)]
    fn fetch_timeout_kills_grandchild_processes() {
        let temp = tempfile::tempdir().unwrap();
        let parent_script = temp.path().join("parent.sh");
        {
            use std::io::Write;
            use std::os::unix::fs::PermissionsExt;
            let mut f = std::fs::File::create(&parent_script).unwrap();
            f.write_all(b"#!/bin/sh\n(sleep 10 >&1) &\nexit 0\n")
                .unwrap();
            f.sync_all().unwrap();
            drop(f);
            std::fs::set_permissions(&parent_script, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let start = std::time::Instant::now();
        let res = fetch_with_timeout(
            parent_script.to_str().unwrap(),
            std::time::Duration::from_millis(200),
        );
        assert!(res.is_err());
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "grandchild held pipe open beyond deadline: {:?}",
            elapsed
        );
        let err_msg = format!("{:#}", res.as_ref().unwrap_err());
        assert!(err_msg.contains("timed out"), "unexpected err: {}", err_msg);
    }

    #[test]
    #[cfg(unix)]
    fn fetch_cancel_aborts_and_kills_process() {
        let temp = tempfile::tempdir().unwrap();
        let slow_script = temp.path().join("slow.sh");
        {
            use std::io::Write;
            use std::os::unix::fs::PermissionsExt;
            let mut f = std::fs::File::create(&slow_script).unwrap();
            f.write_all(b"#!/bin/sh\n(sleep 10 >&1) &\nexec sleep 10\n")
                .unwrap();
            f.sync_all().unwrap();
            drop(f);
            std::fs::set_permissions(&slow_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child_pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

        let script_path = slow_script.to_str().unwrap().to_string();
        let cancel_thread = cancel.clone();
        let child_pid_thread = child_pid.clone();

        let handle = std::thread::spawn(move || {
            fetch_with_timeout_and_cancel(
                &script_path,
                std::time::Duration::from_secs(10),
                Some(&cancel_thread),
                Some(&child_pid_thread),
            )
        });

        let wait_start = std::time::Instant::now();
        while child_pid.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            assert!(wait_start.elapsed() < std::time::Duration::from_secs(2));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let recorded_pid = child_pid.load(std::sync::atomic::Ordering::SeqCst);
        assert!(recorded_pid > 0);

        cancel.store(true, std::sync::atomic::Ordering::SeqCst);

        let start = std::time::Instant::now();
        let res = handle.join().unwrap();
        assert!(res.is_err());
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "cancel took too long (process group not killed): {:?}",
            elapsed
        );
        let err_msg = format!("{:#}", res.unwrap_err());
        assert!(
            err_msg.contains("fetch cancelled"),
            "unexpected err: {}",
            err_msg
        );
        assert_eq!(child_pid.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn fetch_cancel_before_spawn_aborts() {
        let cancel = std::sync::atomic::AtomicBool::new(true);
        let res = fetch_with_timeout_and_cancel(
            "dummy_nonexistent_binary",
            std::time::Duration::from_secs(10),
            Some(&cancel),
            None,
        );
        assert!(res.is_err());
        let err_msg = format!("{:#}", res.unwrap_err());
        assert!(err_msg.contains("fetch cancelled"));
    }

    #[test]
    #[cfg(unix)]
    fn resolve_agy_bin_fail_fast_on_invalid_agy_bin() {
        let temp = tempfile::tempdir().unwrap();
        let valid_bin = temp.path().join("agy");
        std::fs::write(&valid_bin, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&valid_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let path_env = temp.path().to_str().unwrap();
        let bad_agy = temp.path().join("nonexistent_agy");
        let res = resolve_agy_bin_from(bad_agy.to_str(), Some(path_env));
        assert!(res.is_err());
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("AGY_BIN is set to"),
            "expected fail-fast error, got: {}",
            err
        );
    }

    #[test]
    #[cfg(unix)]
    fn resolve_agy_bin_skips_empty_path_elements() {
        let temp = tempfile::tempdir().unwrap();
        let pwd_bin = temp.path().join("agy");
        std::fs::write(&pwd_bin, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pwd_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Empty elements in PATH should be skipped, not resolving relative to PWD
        let res = resolve_agy_bin_from(None, Some(":::/nonexistent_dir::"));
        assert!(res.is_err());
        let err = res.unwrap_err().to_string();
        assert!(err.contains("could not find executable 'agy' on PATH"));
    }

    #[test]
    #[cfg(unix)]
    fn resolve_agy_bin_skips_non_absolute_path_elements() {
        let temp = tempfile::tempdir().unwrap();
        let pwd_bin = temp.path().join("agy");
        std::fs::write(&pwd_bin, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pwd_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Relative PATH elements (like "." or "../../../tmp/...") must be skipped even
        // when they point to an existing executable 'agy'.
        let cwd = std::env::current_dir().unwrap();
        let mut rel_to_temp = PathBuf::new();
        for _ in 0..cwd.components().count().saturating_sub(1) {
            rel_to_temp.push("..");
        }
        for comp in temp.path().components().skip(1) {
            rel_to_temp.push(comp);
        }
        assert!(
            !rel_to_temp.is_absolute() && is_executable(&rel_to_temp.join("agy")),
            "sanity check: relative path points to executable agy"
        );

        let path_with_rel = format!(":.:./bin:rel:{}:/nonexistent_dir::", rel_to_temp.display());
        let res = resolve_agy_bin_from(None, Some(&path_with_rel));
        assert!(res.is_err());
        let err = res.unwrap_err().to_string();
        assert!(err.contains("could not find executable 'agy' on PATH"));

        // Negative boundary: adding the absolute path resolves cleanly even after relative entries.
        let path_with_abs = format!(":.:{}:{}", rel_to_temp.display(), temp.path().display());
        let resolved = resolve_agy_bin_from(None, Some(&path_with_abs)).unwrap();
        assert_eq!(resolved, pwd_bin);
    }

    #[test]
    #[cfg(unix)]
    fn resolve_agy_bin_looks_up_bare_command_name_via_path_not_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let path_dir = temp.path().join("bin");
        std::fs::create_dir_all(&path_dir).unwrap();
        let custom = path_dir.join("agy-custom");
        std::fs::write(&custom, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&custom, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Bare command name without path separator resolves via absolute PATH entries.
        let resolved = resolve_agy_bin_from(
            Some("agy-custom"),
            Some(&format!(":.:{}", path_dir.display())),
        )
        .unwrap();
        assert_eq!(resolved, custom);

        // If PATH has no match, a bare name must NOT match a relative file in CWD.
        let err = resolve_agy_bin_from(Some("Cargo.toml"), Some("")).unwrap_err();
        assert!(
            err.to_string().contains("could not be found") && err.to_string().contains("on PATH"),
            "unexpected error: {err}"
        );

        // Explicit relative path with separator still validates directly.
        let err_rel = resolve_agy_bin_from(Some("./Cargo.toml"), Some("")).unwrap_err();
        assert!(err_rel.to_string().contains("is not an executable file"));
    }

    #[test]
    fn parse_usage_with_warnings_emits_only_for_skipped_enabled_buckets() {
        let ts = Utc.with_ymd_and_hms(2026, 10, 12, 13, 35, 0).unwrap();

        // Valid fixtures emit zero warnings.
        let (pro_samples, pro_warnings) = parse_usage_with_warnings(PRO_FIXTURE_JSON, ts).unwrap();
        assert_eq!(pro_samples.len(), 5);
        assert!(pro_warnings.is_empty());

        // Disabled buckets with missing remaining_fraction or reset_time emit zero warnings,
        // whereas non-disabled buckets missing remaining_fraction and/or reset_time warn once each.
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
                                    "id": "valid-weekly",
                                    "window": "weekly",
                                    "remaining_fraction": 0.75,
                                    "reset_time": "2026-10-16T20:52:27Z"
                                },
                                {
                                    "id": "explicit-disabled",
                                    "window": "5h",
                                    "disabled": true
                                },
                                {
                                    "id": "missing-rem",
                                    "window": "5h",
                                    "reset_time": "2026-10-16T20:52:27Z"
                                },
                                {
                                    "id": "bad-reset",
                                    "window": "5h",
                                    "remaining_fraction": 0.5,
                                    "reset_time": "not-rfc3339"
                                },
                                {
                                    "id": "missing-both",
                                    "window": "5h"
                                }
                            ]
                        }
                    ]
                }
            }
        }"#;

        let (samples, warnings) = parse_usage_with_warnings(json, ts).unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].bucket_id, "valid-weekly");
        assert!(!samples[0].disabled);
        assert_eq!(samples[1].bucket_id, "explicit-disabled");
        assert!(samples[1].disabled);

        assert_eq!(warnings.len(), 3, "unexpected warnings: {warnings:?}");
        assert!(warnings[0].contains("missing-rem") && warnings[0].contains("remaining_fraction"));
        assert!(warnings[1].contains("bad-reset") && warnings[1].contains("reset_time"));
        assert!(
            warnings[2].contains("missing-both")
                && warnings[2].contains("remaining_fraction")
                && warnings[2].contains("reset_time")
        );

        // parse_usage preserves exact samples and stays quiet.
        assert_eq!(parse_usage(json, ts).unwrap(), samples);
    }

    #[test]
    #[cfg(unix)]
    fn resolve_agy_bin_preserves_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let real_bin = temp.path().join("real_agy");
        std::fs::write(&real_bin, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&real_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let symlink_bin = temp.path().join("symlink_agy");
        std::os::unix::fs::symlink(&real_bin, &symlink_bin).unwrap();

        let res = resolve_agy_bin_from(symlink_bin.to_str(), None).unwrap();
        assert_eq!(res, symlink_bin);
        assert_ne!(res, real_bin);
    }

    #[test]
    fn resolve_executable_enforces_absolute_path_lookup_for_bare_names() {
        let rel = std::ffi::OsStr::new("relative_dir");
        let err = resolve_executable(
            std::ffi::OsStr::new("agy-nonexistent-probe"),
            Some(rel),
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("could not be found on PATH"),
            "unexpected: {err}"
        );
        let empty_err = resolve_executable(std::ffi::OsStr::new(""), None, None).unwrap_err();
        assert!(empty_err.to_string().contains("not an executable file"));
    }
}
