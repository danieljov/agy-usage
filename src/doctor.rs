use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::clock::ClockFormat;
use crate::source;
use crate::store;
use crate::timer;

pub const DOCTOR_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckLevel {
    Ok,
    Warn,
    Fail,
}

impl CheckLevel {
    fn tag(self) -> &'static str {
        match self {
            CheckLevel::Ok => "OK",
            CheckLevel::Warn => "WARN",
            CheckLevel::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub level: CheckLevel,
    pub message: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub healthy: bool,
    pub checks: Vec<DoctorCheck>,
}

/// Captures read-only paths and environment inputs for `doctor` diagnostics.
pub struct DoctorContext<'a> {
    pub data_dir: &'a Path,
    pub agy_resolution: Result<PathBuf, String>,
    pub timer_dir: Result<PathBuf, String>,
    pub detected_clock: ClockFormat,
    pub desktop_env: Option<String>,
    pub is_macos: bool,
    pub now: DateTime<Utc>,
}

const FUTURE_SKEW_TOLERANCE_SECS: i64 = 60;

fn format_age(delta: Duration) -> String {
    if delta < Duration::seconds(-FUTURE_SKEW_TOLERANCE_SECS) {
        let secs = delta.num_seconds().unsigned_abs();
        return if secs < 3600 {
            format!("in {}m", secs / 60)
        } else if secs < 86400 {
            format!("in {}h {}m", secs / 3600, (secs % 3600) / 60)
        } else {
            format!("in {}d", secs / 86400)
        };
    }
    let secs = delta.num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h {}m ago", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

fn check_agy_binary(resolution: &Result<PathBuf, String>) -> DoctorCheck {
    match resolution {
        Ok(path) => DoctorCheck {
            name: "agy_binary".to_string(),
            level: CheckLevel::Ok,
            message: format!("resolved agy binary at {}", path.display()),
            detail: None,
        },
        Err(err) => DoctorCheck {
            name: "agy_binary".to_string(),
            level: CheckLevel::Fail,
            message: "could not find executable agy binary".to_string(),
            detail: Some(format!(
                "{err}; install `agy` on PATH or set AGY_BIN=/path/to/agy"
            )),
        },
    }
}

fn check_data_dir(data_dir: &Path) -> DoctorCheck {
    match std::fs::metadata(data_dir) {
        Ok(meta) if meta.is_dir() => DoctorCheck {
            name: "data_dir".to_string(),
            level: CheckLevel::Ok,
            message: format!("data directory exists at {}", data_dir.display()),
            detail: None,
        },
        Ok(_) => DoctorCheck {
            name: "data_dir".to_string(),
            level: CheckLevel::Fail,
            message: format!("{} exists but is not a directory", data_dir.display()),
            detail: None,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DoctorCheck {
            name: "data_dir".to_string(),
            level: CheckLevel::Warn,
            message: format!("{} does not exist yet", data_dir.display()),
            detail: Some("run `agy-usage collect` to initialize storage".to_string()),
        },
        Err(e) => DoctorCheck {
            name: "data_dir".to_string(),
            level: CheckLevel::Fail,
            message: format!("cannot inspect {}: {e}", data_dir.display()),
            detail: None,
        },
    }
}

fn check_samples_store(
    data_dir: &Path,
    now: DateTime<Utc>,
    stale_threshold: Duration,
) -> DoctorCheck {
    let samples_path = data_dir.join("samples.jsonl");
    match std::fs::metadata(&samples_path) {
        Ok(meta) if !meta.is_file() => {
            return DoctorCheck {
                name: "samples_store".to_string(),
                level: CheckLevel::Fail,
                message: format!("{} is not a regular file", samples_path.display()),
                detail: None,
            };
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let lock_path = samples_path.with_extension("lock");
            match std::fs::symlink_metadata(&lock_path) {
                Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                    return DoctorCheck {
                        name: "samples_store".to_string(),
                        level: CheckLevel::Fail,
                        message: format!("{} is not a regular file", lock_path.display()),
                        detail: None,
                    };
                }
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                    return DoctorCheck {
                        name: "samples_store".to_string(),
                        level: CheckLevel::Fail,
                        message: format!("cannot stat {}: {err}", lock_path.display()),
                        detail: None,
                    };
                }
                Ok(_) | Err(_) => {}
            }
            return DoctorCheck {
                name: "samples_store".to_string(),
                level: CheckLevel::Warn,
                message: "no samples.jsonl found yet".to_string(),
                detail: Some("run `agy-usage collect` to capture initial samples".to_string()),
            };
        }
        Err(e) => {
            return DoctorCheck {
                name: "samples_store".to_string(),
                level: CheckLevel::Fail,
                message: format!("cannot stat {}: {e}", samples_path.display()),
                detail: None,
            };
        }
        Ok(_) => {}
    }

    match store::summarize(&samples_path) {
        Err(e) => DoctorCheck {
            name: "samples_store".to_string(),
            level: CheckLevel::Fail,
            message: format!("failed to read {}: {e}", samples_path.display()),
            detail: None,
        },
        Ok(summary) => match summary.newest {
            None => DoctorCheck {
                name: "samples_store".to_string(),
                level: CheckLevel::Warn,
                message: format!(
                    "samples.jsonl contains 0 valid samples ({} skipped)",
                    summary.skipped
                ),
                detail: Some("run `agy-usage collect` to record fresh quota data".to_string()),
            },
            Some(ts) => {
                let age = now.signed_duration_since(ts);
                let stale = age > stale_threshold;
                let future_skew = age < Duration::seconds(-FUTURE_SKEW_TOLERANCE_SECS);
                let level = if summary.skipped > 0 || stale || future_skew {
                    CheckLevel::Warn
                } else {
                    CheckLevel::Ok
                };
                let mut notes = Vec::new();
                if stale {
                    notes.push(format!(
                        "latest sample is >{}m old; check background timer",
                        stale_threshold.num_minutes()
                    ));
                }
                if future_skew {
                    notes.push(
                        "latest sample timestamp is in the future; check system clock".to_string(),
                    );
                }
                if summary.skipped > 0 {
                    notes.push(format!("{} corrupt line(s) skipped", summary.skipped));
                }
                DoctorCheck {
                    name: "samples_store".to_string(),
                    level,
                    message: format!(
                        "{} samples stored (latest {})",
                        summary.count,
                        format_age(age)
                    ),
                    detail: if notes.is_empty() {
                        None
                    } else {
                        Some(notes.join("; "))
                    },
                }
            }
        },
    }
}

fn check_html_dashboard(data_dir: &Path) -> DoctorCheck {
    let html_path = data_dir.join("dashboard.html");
    match std::fs::metadata(&html_path) {
        Ok(meta) if !meta.is_file() => {
            return DoctorCheck {
                name: "html_dashboard".to_string(),
                level: CheckLevel::Fail,
                message: format!("{} is not a regular file", html_path.display()),
                detail: None,
            };
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return DoctorCheck {
                name: "html_dashboard".to_string(),
                level: CheckLevel::Warn,
                message: "dashboard.html has not been generated yet".to_string(),
                detail: Some("run `agy-usage html` or `agy-usage collect`".to_string()),
            };
        }
        Err(e) => {
            return DoctorCheck {
                name: "html_dashboard".to_string(),
                level: CheckLevel::Fail,
                message: format!("cannot inspect {}: {e}", html_path.display()),
                detail: None,
            };
        }
        Ok(_) => {}
    }

    let mut file = match std::fs::File::open(&html_path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return DoctorCheck {
                name: "html_dashboard".to_string(),
                level: CheckLevel::Warn,
                message: "dashboard.html has not been generated yet".to_string(),
                detail: Some("run `agy-usage html` or `agy-usage collect`".to_string()),
            };
        }
        Err(e) => {
            return DoctorCheck {
                name: "html_dashboard".to_string(),
                level: CheckLevel::Fail,
                message: format!("cannot read {}: {e}", html_path.display()),
                detail: None,
            };
        }
    };
    let len = match file.metadata() {
        Ok(meta) => meta.len(),
        Err(e) => {
            return DoctorCheck {
                name: "html_dashboard".to_string(),
                level: CheckLevel::Fail,
                message: format!("cannot inspect {}: {e}", html_path.display()),
                detail: None,
            };
        }
    };

    use std::io::Read as _;
    let mut probe = [0u8; 1];
    match file.read(&mut probe) {
        Err(e) => DoctorCheck {
            name: "html_dashboard".to_string(),
            level: CheckLevel::Fail,
            message: format!("cannot read {}: {e}", html_path.display()),
            detail: None,
        },
        Ok(0) => DoctorCheck {
            name: "html_dashboard".to_string(),
            level: CheckLevel::Warn,
            message: format!("{} is empty (0 bytes)", html_path.display()),
            detail: Some("run `agy-usage html` to regenerate the dashboard".to_string()),
        },
        Ok(_) => DoctorCheck {
            name: "html_dashboard".to_string(),
            level: CheckLevel::Ok,
            message: format!("dashboard.html present ({len} bytes)"),
            detail: None,
        },
    }
}

fn read_timer_file(path: &Path) -> Result<Option<String>, DoctorCheck> {
    match std::fs::metadata(path) {
        Ok(meta) if !meta.is_file() => {
            return Err(DoctorCheck {
                name: "timer".to_string(),
                level: CheckLevel::Fail,
                message: format!("{} is not a regular file", path.display()),
                detail: None,
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(DoctorCheck {
                name: "timer".to_string(),
                level: CheckLevel::Fail,
                message: format!("cannot inspect {}: {e}", path.display()),
                detail: None,
            });
        }
        Ok(_) => {}
    }

    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(DoctorCheck {
            name: "timer".to_string(),
            level: CheckLevel::Fail,
            message: format!("cannot read {}: {e}", path.display()),
            detail: None,
        }),
    }
}

fn check_timer(timer_dir: &Result<PathBuf, String>, is_macos: bool) -> (DoctorCheck, Option<u16>) {
    let dir = match timer_dir {
        Ok(d) => d,
        Err(e) => {
            return (
                DoctorCheck {
                    name: "timer".to_string(),
                    level: CheckLevel::Warn,
                    message: "could not determine background timer directory".to_string(),
                    detail: Some(e.clone()),
                },
                None,
            );
        }
    };

    if is_macos {
        let plist = dir.join(format!("{}.plist", timer::LAUNCHD_LABEL));
        match read_timer_file(&plist) {
            Ok(Some(s)) => {
                let secs = timer::parse_launchd_interval_seconds(&s);
                let mins = secs
                    .and_then(|v| u16::try_from(v / 60).ok())
                    .filter(|&m| m > 0);
                if let (Some(secs), Some(mins)) = (secs, mins) {
                    (
                        DoctorCheck {
                            name: "timer".to_string(),
                            level: CheckLevel::Ok,
                            message: format!("launchd agent installed at {}", plist.display()),
                            detail: Some(format!("StartInterval={secs}s")),
                        },
                        Some(mins),
                    )
                } else {
                    (
                        DoctorCheck {
                            name: "timer".to_string(),
                            level: CheckLevel::Warn,
                            message: format!(
                                "launchd plist at {} has missing or invalid StartInterval",
                                plist.display()
                            ),
                            detail: Some(
                                "staleness check falls back to 15m; re-run `agy-usage install-timer` to repair"
                                    .to_string(),
                            ),
                        },
                        None,
                    )
                }
            }
            Ok(None) => (
                DoctorCheck {
                    name: "timer".to_string(),
                    level: CheckLevel::Warn,
                    message: "launchd background timer is not installed".to_string(),
                    detail: Some(
                        "run `agy-usage install-timer` to schedule collection".to_string(),
                    ),
                },
                None,
            ),
            Err(fail_check) => (fail_check, None),
        }
    } else {
        let service = dir.join("agy-usage.service");
        let unit = dir.join("agy-usage.timer");
        match (read_timer_file(&service), read_timer_file(&unit)) {
            (Err(fail_check), _) | (_, Err(fail_check)) => (fail_check, None),
            (Ok(Some(_)), Ok(Some(s))) => {
                let cal = s
                    .lines()
                    .find(|l| l.trim_start().starts_with("OnCalendar="))
                    .map(|l| l.trim().to_string());
                let mins = timer::parse_systemd_interval_minutes(&s).filter(|&m| m > 0);
                if let Some(mins) = mins {
                    (
                        DoctorCheck {
                            name: "timer".to_string(),
                            level: CheckLevel::Ok,
                            message: format!("systemd user timer installed in {}", dir.display()),
                            detail: cal,
                        },
                        Some(mins),
                    )
                } else {
                    let detail = match cal {
                        Some(spec) => format!(
                            "{spec}; staleness check falls back to 15m; re-run `agy-usage install-timer` to repair"
                        ),
                        None => "staleness check falls back to 15m; re-run `agy-usage install-timer` to repair".to_string(),
                    };
                    (
                        DoctorCheck {
                            name: "timer".to_string(),
                            level: CheckLevel::Warn,
                            message: format!(
                                "systemd timer at {} has missing or invalid OnCalendar interval",
                                unit.display()
                            ),
                            detail: Some(detail),
                        },
                        None,
                    )
                }
            }
            (Ok(None), Ok(None)) => (
                DoctorCheck {
                    name: "timer".to_string(),
                    level: CheckLevel::Warn,
                    message: "systemd user timer is not installed".to_string(),
                    detail: Some(
                        "run `agy-usage install-timer` to schedule collection".to_string(),
                    ),
                },
                None,
            ),
            (Ok(svc), Ok(timer_unit)) => (
                DoctorCheck {
                    name: "timer".to_string(),
                    level: CheckLevel::Warn,
                    message: format!(
                        "partial systemd timer installation (service={}, timer={})",
                        svc.is_some(),
                        timer_unit.is_some()
                    ),
                    detail: Some("re-run `agy-usage install-timer` to repair units".to_string()),
                },
                None,
            ),
        }
    }
}

fn check_clock(detected: ClockFormat, desktop_env: Option<&str>, is_macos: bool) -> DoctorCheck {
    let detail = if is_macos {
        "source=macOS defaults / locale".to_string()
    } else {
        let de = desktop_env
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("unset");
        format!("XDG_CURRENT_DESKTOP={de}")
    };
    DoctorCheck {
        name: "clock_detection".to_string(),
        level: CheckLevel::Ok,
        message: format!("auto clock resolved to {}", detected.as_arg()),
        detail: Some(detail),
    }
}

/// Evaluates all read-only diagnostics from a supplied context.
pub fn evaluate_checks(ctx: &DoctorContext<'_>) -> DoctorReport {
    let (timer_check, interval_mins) = check_timer(&ctx.timer_dir, ctx.is_macos);
    let stale_threshold = timer::stale_threshold_for_interval(interval_mins);
    let checks = vec![
        check_agy_binary(&ctx.agy_resolution),
        check_data_dir(ctx.data_dir),
        check_samples_store(ctx.data_dir, ctx.now, stale_threshold),
        check_html_dashboard(ctx.data_dir),
        timer_check,
        check_clock(ctx.detected_clock, ctx.desktop_env.as_deref(), ctx.is_macos),
    ];
    let healthy = checks.iter().all(|c| c.level != CheckLevel::Fail);
    DoctorReport {
        schema_version: DOCTOR_SCHEMA_VERSION,
        generated_at: ctx.now,
        healthy,
        checks,
    }
}

/// Runs read-only system diagnostics against `data_dir`.
pub fn run_doctor(data_dir: &Path, now: DateTime<Utc>) -> DoctorReport {
    let agy_resolution = source::resolve_agy_bin().map_err(|e| e.to_string());
    #[cfg(target_os = "macos")]
    let (timer_dir, is_macos) = (timer::launchd_agents_dir().map_err(|e| e.to_string()), true);
    #[cfg(not(target_os = "macos"))]
    let (timer_dir, is_macos) = (timer::systemd_user_dir().map_err(|e| e.to_string()), false);

    let ctx = DoctorContext {
        data_dir,
        agy_resolution,
        timer_dir,
        detected_clock: ClockFormat::detect(),
        desktop_env: std::env::var("XDG_CURRENT_DESKTOP").ok(),
        is_macos,
        now,
    };
    evaluate_checks(&ctx)
}

/// Renders a human-readable diagnostic checklist.
pub fn format_doctor_human(report: &DoctorReport) -> String {
    let mut out = String::new();
    for check in &report.checks {
        out.push_str(&format!(
            "[{}] {}: {}\n",
            check.level.tag(),
            check.name,
            check.message
        ));
        if let Some(detail) = &check.detail {
            out.push_str(&format!("       {detail}\n"));
        }
    }
    let summary = if report.healthy {
        "Doctor summary: healthy (no failing checks)"
    } else {
        "Doctor summary: unhealthy (one or more checks failed)"
    };
    out.push_str(summary);
    out
}

/// Writes the doctor report (JSON or human checklist), tolerating `BrokenPipe`.
pub fn write_doctor(report: &DoctorReport, json: bool, writer: &mut impl Write) -> Result<()> {
    let rendered = if json {
        serde_json::to_string_pretty(report).context("failed to serialize doctor JSON")?
    } else {
        format_doctor_human(report)
    };
    crate::write_line_broken_pipe_safe(&rendered, "failed to write doctor output", writer)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use tempfile::tempdir;

    use super::*;
    use crate::model::Sample;

    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn healthy_doctor_context_reports_all_ok() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let timer_dir = dir.path().join("systemd");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&timer_dir).unwrap();

        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
        store::append(
            &data_dir.join("samples.jsonl"),
            &[Sample {
                ts: now - Duration::minutes(4),
                group: "Gemini".to_string(),
                bucket_id: "g-5h".to_string(),
                window: "5h".to_string(),
                remaining: 0.9,
                reset_time: Some(now + Duration::hours(4)),
                disabled: false,
            }],
        )
        .unwrap();
        std::fs::write(data_dir.join("dashboard.html"), "<html>ok</html>").unwrap();
        std::fs::write(timer_dir.join("agy-usage.service"), "[Service]\n").unwrap();
        std::fs::write(
            timer_dir.join("agy-usage.timer"),
            "[Timer]\nOnCalendar=*:0/10\n",
        )
        .unwrap();

        let ctx = DoctorContext {
            data_dir: &data_dir,
            agy_resolution: Ok(PathBuf::from("/usr/local/bin/agy")),
            timer_dir: Ok(timer_dir),
            detected_clock: ClockFormat::H24,
            desktop_env: Some("KDE".to_string()),
            is_macos: false,
            now,
        };
        let report = evaluate_checks(&ctx);
        assert!(report.healthy);
        assert!(report.checks.iter().all(|c| c.level == CheckLevel::Ok));
        let timer_check = report.checks.iter().find(|c| c.name == "timer").unwrap();
        assert_eq!(timer_check.detail.as_deref(), Some("OnCalendar=*:0/10"));
        let clock_check = report
            .checks
            .iter()
            .find(|c| c.name == "clock_detection")
            .unwrap();
        assert_eq!(
            clock_check.detail.as_deref(),
            Some("XDG_CURRENT_DESKTOP=KDE")
        );
    }

    #[test]
    fn missing_agy_and_file_data_dir_mark_unhealthy_without_mutating() {
        let dir = tempdir().unwrap();
        let bad_data = dir.path().join("not-a-dir");
        std::fs::write(&bad_data, "oops").unwrap();
        let timer_dir = dir.path().join("missing-timer");

        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
        let ctx = DoctorContext {
            data_dir: &bad_data,
            agy_resolution: Err("agy not found".to_string()),
            timer_dir: Ok(timer_dir.clone()),
            detected_clock: ClockFormat::H12,
            desktop_env: None,
            is_macos: true,
            now,
        };
        let report = evaluate_checks(&ctx);
        assert!(!report.healthy);
        assert_eq!(report.checks[0].level, CheckLevel::Fail);
        assert_eq!(report.checks[1].level, CheckLevel::Fail);
        let clock_check = report
            .checks
            .iter()
            .find(|c| c.name == "clock_detection")
            .unwrap();
        assert_eq!(
            clock_check.detail.as_deref(),
            Some("source=macOS defaults / locale")
        );
        assert!(!timer_dir.exists(), "doctor must remain strictly read-only");
    }

    #[test]
    fn stale_or_corrupt_samples_and_partial_timer_emit_warnings() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let timer_dir = dir.path().join("systemd");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&timer_dir).unwrap();

        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
        store::append(
            &data_dir.join("samples.jsonl"),
            &[Sample {
                ts: now - Duration::minutes(40),
                group: "Gemini".to_string(),
                bucket_id: "g-5h".to_string(),
                window: "5h".to_string(),
                remaining: 0.9,
                reset_time: None,
                disabled: false,
            }],
        )
        .unwrap();
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(data_dir.join("samples.jsonl"))
            .unwrap();
        writeln!(f, "{{not json").unwrap();
        std::fs::write(timer_dir.join("agy-usage.service"), "[Service]\n").unwrap();

        let ctx = DoctorContext {
            data_dir: &data_dir,
            agy_resolution: Ok(PathBuf::from("/usr/bin/agy")),
            timer_dir: Ok(timer_dir),
            detected_clock: ClockFormat::H24,
            desktop_env: Some("GNOME".to_string()),
            is_macos: false,
            now,
        };
        let report = evaluate_checks(&ctx);
        assert!(report.healthy);
        let samples_check = report
            .checks
            .iter()
            .find(|c| c.name == "samples_store")
            .unwrap();
        assert_eq!(samples_check.level, CheckLevel::Warn);
        assert!(
            samples_check
                .detail
                .as_deref()
                .unwrap()
                .contains(">15m old")
        );
        assert!(
            samples_check
                .detail
                .as_deref()
                .unwrap()
                .contains("1 corrupt line")
        );

        let timer_check = report.checks.iter().find(|c| c.name == "timer").unwrap();
        assert_eq!(timer_check.level, CheckLevel::Warn);
        assert!(timer_check.message.contains("partial"));
    }

    #[test]
    fn write_doctor_handles_json_human_and_broken_pipe() {
        let dir = tempdir().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
        let ctx = DoctorContext {
            data_dir: &dir.path().join("missing"),
            agy_resolution: Ok(PathBuf::from("/usr/bin/agy")),
            timer_dir: Ok(dir.path().join("launchd")),
            detected_clock: ClockFormat::H24,
            desktop_env: None,
            is_macos: true,
            now,
        };
        let report = evaluate_checks(&ctx);
        let human = format_doctor_human(&report);
        assert!(human.contains("[OK] agy_binary"));
        assert!(human.contains("[WARN] data_dir"));
        assert!(human.contains("Doctor summary: healthy"));

        let mut json_buf = Vec::new();
        write_doctor(&report, true, &mut json_buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&json_buf).unwrap();
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["healthy"], true);

        let mut bp = BrokenPipeWriter;
        assert!(write_doctor(&report, true, &mut bp).is_ok());
    }

    #[test]
    fn samples_staleness_scales_with_installed_60m_timer_interval() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let timer_dir = dir.path().join("systemd");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&timer_dir).unwrap();

        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
        store::append(
            &data_dir.join("samples.jsonl"),
            &[Sample {
                ts: now - Duration::minutes(40),
                group: "Gemini".to_string(),
                bucket_id: "g-5h".to_string(),
                window: "5h".to_string(),
                remaining: 0.9,
                reset_time: None,
                disabled: false,
            }],
        )
        .unwrap();
        std::fs::write(data_dir.join("dashboard.html"), "<html>ok</html>").unwrap();
        std::fs::write(timer_dir.join("agy-usage.service"), "[Service]\n").unwrap();
        std::fs::write(
            timer_dir.join("agy-usage.timer"),
            "[Timer]\nOnCalendar=*:00\n",
        )
        .unwrap();

        let ctx = DoctorContext {
            data_dir: &data_dir,
            agy_resolution: Ok(PathBuf::from("/usr/bin/agy")),
            timer_dir: Ok(timer_dir),
            detected_clock: ClockFormat::H24,
            desktop_env: Some("GNOME".to_string()),
            is_macos: false,
            now,
        };
        let report = evaluate_checks(&ctx);
        let samples_check = report
            .checks
            .iter()
            .find(|c| c.name == "samples_store")
            .unwrap();
        assert_eq!(samples_check.level, CheckLevel::Ok);
        assert_eq!(samples_check.detail, None);
    }

    #[test]
    fn invalid_launchd_and_systemd_intervals_emit_warning_and_keep_15m_fallback() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
        // 20m old: stale under 15m default fallback, would be fresh under 30m interval.
        store::append(
            &data_dir.join("samples.jsonl"),
            &[Sample {
                ts: now - Duration::minutes(20),
                group: "Gemini".to_string(),
                bucket_id: "g-5h".to_string(),
                window: "5h".to_string(),
                remaining: 0.9,
                reset_time: None,
                disabled: false,
            }],
        )
        .unwrap();
        std::fs::write(data_dir.join("dashboard.html"), "<html>ok</html>").unwrap();

        // 1. launchd plist missing StartInterval
        let launchd_missing = dir.path().join("launchd-missing");
        std::fs::create_dir_all(&launchd_missing).unwrap();
        std::fs::write(
            launchd_missing.join(format!("{}.plist", timer::LAUNCHD_LABEL)),
            "<plist><dict><key>Label</key><string>x</string></dict></plist>\n",
        )
        .unwrap();
        let report_missing = evaluate_checks(&DoctorContext {
            data_dir: &data_dir,
            agy_resolution: Ok(PathBuf::from("/usr/bin/agy")),
            timer_dir: Ok(launchd_missing),
            detected_clock: ClockFormat::H24,
            desktop_env: None,
            is_macos: true,
            now,
        });
        assert!(report_missing.healthy);
        let timer_check = report_missing
            .checks
            .iter()
            .find(|c| c.name == "timer")
            .unwrap();
        assert_eq!(timer_check.level, CheckLevel::Warn);
        assert!(
            timer_check
                .message
                .contains("missing or invalid StartInterval")
        );

        // 2. launchd plist with StartInterval < 60s
        let launchd_submin = dir.path().join("launchd-submin");
        std::fs::create_dir_all(&launchd_submin).unwrap();
        std::fs::write(
            launchd_submin.join(format!("{}.plist", timer::LAUNCHD_LABEL)),
            "<plist><dict><key>StartInterval</key><integer>30</integer></dict></plist>\n",
        )
        .unwrap();
        let report_submin = evaluate_checks(&DoctorContext {
            data_dir: &data_dir,
            agy_resolution: Ok(PathBuf::from("/usr/bin/agy")),
            timer_dir: Ok(launchd_submin),
            detected_clock: ClockFormat::H24,
            desktop_env: None,
            is_macos: true,
            now,
        });
        let timer_check = report_submin
            .checks
            .iter()
            .find(|c| c.name == "timer")
            .unwrap();
        assert_eq!(timer_check.level, CheckLevel::Warn);

        // 3. Valid launchd plist with StartInterval = 1800 (30m) is Ok and scales threshold to 60m
        let launchd_ok = dir.path().join("launchd-ok");
        std::fs::create_dir_all(&launchd_ok).unwrap();
        std::fs::write(
            launchd_ok.join(format!("{}.plist", timer::LAUNCHD_LABEL)),
            "<plist><dict><key>StartInterval</key><integer>1800</integer></dict></plist>\n",
        )
        .unwrap();
        let report_ok = evaluate_checks(&DoctorContext {
            data_dir: &data_dir,
            agy_resolution: Ok(PathBuf::from("/usr/bin/agy")),
            timer_dir: Ok(launchd_ok),
            detected_clock: ClockFormat::H24,
            desktop_env: None,
            is_macos: true,
            now,
        });
        let timer_ok = report_ok.checks.iter().find(|c| c.name == "timer").unwrap();
        assert_eq!(timer_ok.level, CheckLevel::Ok);
        assert_eq!(timer_ok.detail.as_deref(), Some("StartInterval=1800s"));
        let samples_ok = report_ok
            .checks
            .iter()
            .find(|c| c.name == "samples_store")
            .unwrap();
        assert_eq!(samples_ok.level, CheckLevel::Ok);

        // 4. systemd timer with invalid OnCalendar=*:0/0 emits Warn
        let systemd_bad = dir.path().join("systemd-bad");
        std::fs::create_dir_all(&systemd_bad).unwrap();
        std::fs::write(systemd_bad.join("agy-usage.service"), "[Service]\n").unwrap();
        std::fs::write(
            systemd_bad.join("agy-usage.timer"),
            "[Timer]\nOnCalendar=*:0/0\n",
        )
        .unwrap();
        let report_sd = evaluate_checks(&DoctorContext {
            data_dir: &data_dir,
            agy_resolution: Ok(PathBuf::from("/usr/bin/agy")),
            timer_dir: Ok(systemd_bad),
            detected_clock: ClockFormat::H24,
            desktop_env: Some("GNOME".to_string()),
            is_macos: false,
            now,
        });
        let timer_sd = report_sd.checks.iter().find(|c| c.name == "timer").unwrap();
        assert_eq!(timer_sd.level, CheckLevel::Warn);
        assert!(timer_sd.message.contains("missing or invalid OnCalendar"));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_dashboard_and_inaccessible_timer_dir_fail_doctor() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let timer_dir = dir.path().join("timers");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&timer_dir).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();

        let html_path = data_dir.join("dashboard.html");
        std::fs::write(&html_path, "<html>secret</html>").unwrap();
        std::fs::set_permissions(&html_path, std::fs::Permissions::from_mode(0o000)).unwrap();

        std::fs::write(
            timer_dir.join("agy-usage.timer"),
            "[Timer]\nOnCalendar=*:0/5\n",
        )
        .unwrap();
        std::fs::set_permissions(&timer_dir, std::fs::Permissions::from_mode(0o000)).unwrap();

        if std::fs::File::open(&html_path).is_err()
            && std::fs::metadata(timer_dir.join("agy-usage.timer")).is_err()
        {
            let report = evaluate_checks(&DoctorContext {
                data_dir: &data_dir,
                agy_resolution: Ok(PathBuf::from("/usr/bin/agy")),
                timer_dir: Ok(timer_dir.clone()),
                detected_clock: ClockFormat::H24,
                desktop_env: Some("GNOME".to_string()),
                is_macos: false,
                now,
            });
            assert!(!report.healthy);
            let html_check = report
                .checks
                .iter()
                .find(|c| c.name == "html_dashboard")
                .unwrap();
            assert_eq!(html_check.level, CheckLevel::Fail);
            let timer_check = report.checks.iter().find(|c| c.name == "timer").unwrap();
            assert_eq!(timer_check.level, CheckLevel::Fail);

            // Also verify macOS launchd path propagates metadata permission errors as Fail.
            let report_mac = evaluate_checks(&DoctorContext {
                data_dir: &data_dir,
                agy_resolution: Ok(PathBuf::from("/usr/bin/agy")),
                timer_dir: Ok(timer_dir.clone()),
                detected_clock: ClockFormat::H24,
                desktop_env: None,
                is_macos: true,
                now,
            });
            let timer_mac = report_mac
                .checks
                .iter()
                .find(|c| c.name == "timer")
                .unwrap();
            assert_eq!(timer_mac.level, CheckLevel::Fail);
        }

        std::fs::set_permissions(&timer_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&html_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn directory_lockfile_fails_doctor_and_future_timestamp_warns() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data_lock_dir");
        std::fs::create_dir_all(data_dir.join("samples.lock")).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
        std::fs::write(data_dir.join("samples.jsonl"), "").unwrap();

        let lock_dir_report = evaluate_checks(&DoctorContext {
            data_dir: &data_dir,
            agy_resolution: Ok(PathBuf::from("/usr/bin/agy")),
            timer_dir: Ok(dir.path().join("timers")),
            detected_clock: ClockFormat::H24,
            desktop_env: Some("GNOME".to_string()),
            is_macos: false,
            now,
        });
        assert!(!lock_dir_report.healthy);
        let store_check = lock_dir_report
            .checks
            .iter()
            .find(|c| c.name == "samples_store")
            .unwrap();
        assert_eq!(store_check.level, CheckLevel::Fail);

        // Future timestamp warns with `in Xh Ym`; minor 10s negative drift stays Ok (`0s ago`).
        let future_dir = dir.path().join("data_future");
        std::fs::create_dir_all(&future_dir).unwrap();
        let future_sample = crate::model::Sample {
            ts: now + Duration::hours(2),
            group: "Gemini Models".to_string(),
            bucket_id: "gemini-5h".to_string(),
            window: "5h".to_string(),
            remaining: 0.8,
            reset_time: None,
            disabled: false,
        };
        std::fs::write(
            future_dir.join("samples.jsonl"),
            serde_json::to_string(&future_sample).unwrap() + "\n",
        )
        .unwrap();
        let future_check = check_samples_store(&future_dir, now, Duration::minutes(15));
        assert_eq!(future_check.level, CheckLevel::Warn);
        assert!(future_check.message.contains("in 2h 0m"));
        assert!(
            future_check
                .detail
                .as_deref()
                .unwrap()
                .contains("in the future")
        );

        let drift_check = check_samples_store(
            &future_dir,
            now + Duration::hours(2) - Duration::seconds(10),
            Duration::minutes(15),
        );
        assert_eq!(drift_check.level, CheckLevel::Ok);
        assert!(drift_check.message.contains("0s ago"));

        // Any future skew > 60s that displays `in Xm` must also warn (no Ok + future age mismatch).
        let two_min_skew = check_samples_store(
            &future_dir,
            now + Duration::hours(2) - Duration::minutes(2),
            Duration::minutes(15),
        );
        assert_eq!(two_min_skew.level, CheckLevel::Warn);
        assert!(two_min_skew.message.contains("in 2m"));

        #[cfg(unix)]
        {
            let sym_dir = dir.path().join("data_lock_symlink");
            std::fs::create_dir_all(&sym_dir).unwrap();
            std::os::unix::fs::symlink(
                dir.path().join("nonexistent_target"),
                sym_dir.join("samples.lock"),
            )
            .unwrap();
            let sym_check = check_samples_store(&sym_dir, now, Duration::minutes(15));
            assert_eq!(sym_check.level, CheckLevel::Fail);
        }
    }
}
