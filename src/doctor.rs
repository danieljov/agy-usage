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

fn format_age(delta: Duration) -> String {
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
                let level = if summary.skipped > 0 || stale {
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
        Ok(meta) if !meta.is_file() => DoctorCheck {
            name: "html_dashboard".to_string(),
            level: CheckLevel::Fail,
            message: format!("{} is not a regular file", html_path.display()),
            detail: None,
        },
        Ok(meta) if meta.len() == 0 => DoctorCheck {
            name: "html_dashboard".to_string(),
            level: CheckLevel::Warn,
            message: format!("{} is empty (0 bytes)", html_path.display()),
            detail: Some("run `agy-usage html` to regenerate the dashboard".to_string()),
        },
        Ok(meta) => DoctorCheck {
            name: "html_dashboard".to_string(),
            level: CheckLevel::Ok,
            message: format!("dashboard.html present ({} bytes)", meta.len()),
            detail: None,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DoctorCheck {
            name: "html_dashboard".to_string(),
            level: CheckLevel::Warn,
            message: "dashboard.html has not been generated yet".to_string(),
            detail: Some("run `agy-usage html` or `agy-usage collect`".to_string()),
        },
        Err(e) => DoctorCheck {
            name: "html_dashboard".to_string(),
            level: CheckLevel::Fail,
            message: format!("cannot inspect {}: {e}", html_path.display()),
            detail: None,
        },
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
        if plist.is_file() {
            match std::fs::read_to_string(&plist) {
                Ok(s) => {
                    let secs = timer::parse_launchd_interval_seconds(&s);
                    let mins = secs
                        .and_then(|v| u16::try_from(v / 60).ok())
                        .filter(|&m| m > 0);
                    (
                        DoctorCheck {
                            name: "timer".to_string(),
                            level: CheckLevel::Ok,
                            message: format!("launchd agent installed at {}", plist.display()),
                            detail: secs.map(|secs| format!("StartInterval={secs}s")),
                        },
                        mins,
                    )
                }
                Err(e) => (
                    DoctorCheck {
                        name: "timer".to_string(),
                        level: CheckLevel::Fail,
                        message: format!("cannot read {}: {e}", plist.display()),
                        detail: None,
                    },
                    None,
                ),
            }
        } else {
            (
                DoctorCheck {
                    name: "timer".to_string(),
                    level: CheckLevel::Warn,
                    message: "launchd background timer is not installed".to_string(),
                    detail: Some(
                        "run `agy-usage install-timer` to schedule collection".to_string(),
                    ),
                },
                None,
            )
        }
    } else {
        let service = dir.join("agy-usage.service");
        let unit = dir.join("agy-usage.timer");
        match (service.is_file(), unit.is_file()) {
            (true, true) => {
                if let Err(e) = std::fs::read_to_string(&service) {
                    return (
                        DoctorCheck {
                            name: "timer".to_string(),
                            level: CheckLevel::Fail,
                            message: format!("cannot read {}: {e}", service.display()),
                            detail: None,
                        },
                        None,
                    );
                }
                match std::fs::read_to_string(&unit) {
                    Ok(s) => {
                        let cal = s
                            .lines()
                            .find(|l| l.trim_start().starts_with("OnCalendar="))
                            .map(|l| l.trim().to_string());
                        let mins = timer::parse_systemd_interval_minutes(&s).filter(|&m| m > 0);
                        (
                            DoctorCheck {
                                name: "timer".to_string(),
                                level: CheckLevel::Ok,
                                message: format!(
                                    "systemd user timer installed in {}",
                                    dir.display()
                                ),
                                detail: cal,
                            },
                            mins,
                        )
                    }
                    Err(e) => (
                        DoctorCheck {
                            name: "timer".to_string(),
                            level: CheckLevel::Fail,
                            message: format!("cannot read {}: {e}", unit.display()),
                            detail: None,
                        },
                        None,
                    ),
                }
            }
            (false, false) => (
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
            (has_service, has_timer) => (
                DoctorCheck {
                    name: "timer".to_string(),
                    level: CheckLevel::Warn,
                    message: format!(
                        "partial systemd timer installation (service={}, timer={})",
                        has_service, has_timer
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
}
