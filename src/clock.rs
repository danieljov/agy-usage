//! Detection and application of the user's preferred clock format (12h vs 24h).

use std::fmt::Display;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone};
use serde::Serialize;

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum ClockFormat {
    #[default]
    #[serde(rename = "24h")]
    H24,
    #[serde(rename = "12h")]
    H12,
}

impl ClockFormat {
    /// Detects the user's clock preference from the system.
    ///
    /// Order: desktop clock setting (GNOME `clock-format`), then the `LC_TIME`
    /// locale's time format, then 24h. The desktop setting wins because it is
    /// what the user's on-screen clock follows, even when the locale (e.g.
    /// `en_US`) defaults to the other format.
    pub fn detect() -> Self {
        let gsettings = run_probe(
            "gsettings",
            &["get", "org.gnome.desktop.interface", "clock-format"],
        );
        if let Some(c) = gsettings.as_deref().and_then(parse_gsettings_clock_format) {
            return c;
        }
        resolve(None, run_probe("locale", &["t_fmt"]).as_deref())
    }

    /// Formats a full date-time in the given time zone using this clock format.
    pub fn format_datetime<Tz: TimeZone>(self, dt: &DateTime<Tz>) -> String
    where
        Tz::Offset: Display,
    {
        let pattern = match self {
            ClockFormat::H24 => "%Y-%m-%d %H:%M:%S",
            ClockFormat::H12 => "%Y-%m-%d %I:%M:%S %p",
        };
        dt.format(pattern).to_string()
    }

    /// The `--clock` CLI value for this format.
    pub fn as_arg(self) -> &'static str {
        match self {
            ClockFormat::H24 => "24h",
            ClockFormat::H12 => "12h",
        }
    }
}

/// Parses `gsettings get org.gnome.desktop.interface clock-format` output.
pub fn parse_gsettings_clock_format(out: &str) -> Option<ClockFormat> {
    match out.trim().trim_matches('\'') {
        "24h" => Some(ClockFormat::H24),
        "12h" => Some(ClockFormat::H12),
        _ => None,
    }
}

/// Classifies a glibc `t_fmt` (e.g. from `locale t_fmt`) as 12h or 24h.
pub fn parse_locale_t_fmt(t_fmt: &str) -> Option<ClockFormat> {
    let mut saw_24h = false;
    let mut chars = t_fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            continue;
        }
        // Skip glibc flags, field width and E/O modifiers to reach the conversion.
        let conv = chars
            .by_ref()
            .find(|c| !matches!(c, '-' | '_' | '^' | '#' | '0'..='9' | 'E' | 'O'));
        match conv {
            Some('r' | 'I' | 'l' | 'p' | 'P') => return Some(ClockFormat::H12),
            Some('H' | 'k' | 'T' | 'R') => saw_24h = true,
            Some(_) => {}
            None => break,
        }
    }
    saw_24h.then_some(ClockFormat::H24)
}

/// Combines probe outputs: desktop setting, then locale, then 24h.
pub fn resolve(gsettings: Option<&str>, t_fmt: Option<&str>) -> ClockFormat {
    gsettings
        .and_then(parse_gsettings_clock_format)
        .or_else(|| t_fmt.and_then(parse_locale_t_fmt))
        .unwrap_or_default()
}

/// Runs a short-lived helper and returns its stdout on success, or `None` if
/// it is missing, fails, or exceeds [`PROBE_TIMEOUT`].
fn run_probe(program: &str, args: &[&str]) -> Option<String> {
    let path = find_in_path(program, std::env::var("PATH").ok().as_deref())?;
    run_probe_at(&path, args, PROBE_TIMEOUT)
}

/// Looks up `name` in `path_env`, skipping empty and relative entries, which
/// `execvp` would otherwise resolve against the current directory.
fn find_in_path(name: &str, path_env: Option<&str>) -> Option<PathBuf> {
    std::env::split_paths(path_env?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| crate::source::is_executable(candidate))
}

fn run_probe_at(program: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Own process group so a timeout also reaps helpers the probe spawned
    // (e.g. a D-Bus autolaunch) that would keep the stdout pipe open.
    cmd.process_group(0);

    let mut child = {
        let mut attempts = 0;
        loop {
            match cmd.spawn() {
                Ok(child) => break child,
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 20 => {
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return None,
            }
        }
    };

    // If a helper escapes the process group (setsid), this thread may outlive
    // the call until that helper exits; acceptable for a short-lived CLI.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        stdout.read_to_string(&mut out).map(|_| out)
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) if reader.is_finished() => break status,
            Ok(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                if let Some(pid) = i32::try_from(child.id())
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
                {
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()?.ok()?;
    status.success().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone};
    use std::os::unix::fs::PermissionsExt;

    fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn find_in_path_ignores_empty_and_relative_entries() {
        let dir = tempfile::tempdir().unwrap();
        let probe = write_script(dir.path(), "probe", "true");
        // Empty / relative entries mean "current directory" to execvp and
        // must never be searched.
        assert_eq!(find_in_path("probe", Some("")), None);
        assert_eq!(find_in_path("probe", Some(":.:relative")), None);
        let path = format!(":.:{}", dir.path().display());
        assert_eq!(find_in_path("probe", Some(&path)), Some(probe));
        assert_eq!(find_in_path("probe", None), None);
    }

    #[test]
    fn run_probe_returns_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_script(dir.path(), "ok", "echo \"'12h'\"");
        assert_eq!(
            run_probe_at(&p, &[], Duration::from_secs(5)).as_deref(),
            Some("'12h'\n")
        );
        let f = write_script(dir.path(), "fail", "echo \"'12h'\"; exit 1");
        assert_eq!(run_probe_at(&f, &[], Duration::from_secs(5)), None);
    }

    #[test]
    fn run_probe_is_bounded_when_grandchild_holds_stdout() {
        let dir = tempfile::tempdir().unwrap();
        // The direct child exits at once, but a background grandchild keeps
        // the stdout pipe open; reading to EOF would block for 30s.
        let p = write_script(dir.path(), "hold", "sleep 30 &\necho \"'12h'\"");
        let start = Instant::now();
        assert_eq!(run_probe_at(&p, &[], Duration::from_millis(300)), None);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "probe blocked for {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn gsettings_24h_and_12h_parse() {
        assert_eq!(
            parse_gsettings_clock_format("'24h'\n"),
            Some(ClockFormat::H24)
        );
        assert_eq!(
            parse_gsettings_clock_format("'12h'\n"),
            Some(ClockFormat::H12)
        );
    }

    #[test]
    fn gsettings_garbage_is_unknown() {
        assert_eq!(parse_gsettings_clock_format(""), None);
        assert_eq!(parse_gsettings_clock_format("No such schema"), None);
    }

    #[test]
    fn locale_t_fmt_12h_conversions() {
        // en_US: t_fmt="%r"
        assert_eq!(parse_locale_t_fmt("%r\n"), Some(ClockFormat::H12));
        assert_eq!(parse_locale_t_fmt("%I:%M:%S %p"), Some(ClockFormat::H12));
        // glibc flags / modifiers must not hide the conversion
        assert_eq!(parse_locale_t_fmt("%-I:%M %P"), Some(ClockFormat::H12));
        assert_eq!(parse_locale_t_fmt("%OI:%M"), Some(ClockFormat::H12));
    }

    #[test]
    fn locale_t_fmt_24h_conversions() {
        // de_DE / C locale: t_fmt="%T" or "%H:%M:%S"
        assert_eq!(parse_locale_t_fmt("%T"), Some(ClockFormat::H24));
        assert_eq!(parse_locale_t_fmt("%H:%M:%S"), Some(ClockFormat::H24));
        assert_eq!(parse_locale_t_fmt("%k.%M"), Some(ClockFormat::H24));
    }

    #[test]
    fn locale_t_fmt_unknown() {
        assert_eq!(parse_locale_t_fmt(""), None);
        assert_eq!(parse_locale_t_fmt("%M:%S"), None);
        // escaped percent is a literal, not a conversion
        assert_eq!(parse_locale_t_fmt("%%I"), None);
    }

    #[test]
    fn desktop_setting_wins_over_locale() {
        // The reported case: en_US locale (12h) but GNOME clock set to 24h.
        assert_eq!(resolve(Some("'24h'"), Some("%r")), ClockFormat::H24);
        assert_eq!(resolve(Some("'12h'"), Some("%T")), ClockFormat::H12);
    }

    #[test]
    fn falls_back_to_locale_then_24h() {
        assert_eq!(resolve(None, Some("%r")), ClockFormat::H12);
        assert_eq!(resolve(Some("garbage"), Some("%r")), ClockFormat::H12);
        assert_eq!(resolve(None, None), ClockFormat::H24);
        assert_eq!(resolve(None, Some("")), ClockFormat::H24);
    }

    #[test]
    fn formats_datetime_per_clock() {
        let tz = FixedOffset::east_opt(2 * 3600).unwrap();
        let dt = tz.with_ymd_and_hms(2026, 10, 11, 23, 5, 9).unwrap();
        assert_eq!(ClockFormat::H24.format_datetime(&dt), "2026-10-11 23:05:09");
        assert_eq!(
            ClockFormat::H12.format_datetime(&dt),
            "2026-10-11 11:05:09 PM"
        );
    }

    #[test]
    fn serializes_as_short_tokens() {
        for c in [ClockFormat::H24, ClockFormat::H12] {
            assert_eq!(
                serde_json::to_string(&c).unwrap(),
                format!("\"{}\"", c.as_arg())
            );
        }
        assert_eq!(ClockFormat::H24.as_arg(), "24h");
        assert_eq!(ClockFormat::H12.as_arg(), "12h");
    }
}
