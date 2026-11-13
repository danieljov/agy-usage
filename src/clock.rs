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
    /// On Linux/BSD: desktop clock setting (GNOME-family `gsettings` when
    /// `XDG_CURRENT_DESKTOP` is unset/empty or belongs to a GNOME/GTK desktop),
    /// then the `LC_TIME` locale's time format, then 24h.
    ///
    /// On macOS: `defaults read -g` overrides (`AppleICUForce24HourTime`,
    /// `AppleICUForce12HourTime`), then `AppleLocale`, then `locale t_fmt`,
    /// then 24h.
    pub fn detect() -> Self {
        #[cfg(target_os = "macos")]
        {
            let macos_probes: [(&str, fn(&str) -> Option<ClockFormat>); 3] = [
                ("AppleICUForce24HourTime", parse_apple_force_24h),
                ("AppleICUForce12HourTime", parse_apple_force_12h),
                ("AppleLocale", parse_apple_locale_clock),
            ];
            for (key, parser) in macos_probes {
                if let Some(c) = run_probe("defaults", &["read", "-g", key])
                    .as_deref()
                    .and_then(parser)
                {
                    return c;
                }
            }
            resolve(None, run_probe("locale", &["t_fmt"]).as_deref())
        }

        #[cfg(not(target_os = "macos"))]
        {
            let gsettings =
                if should_probe_gsettings(std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref()) {
                    run_probe(
                        "gsettings",
                        &["get", "org.gnome.desktop.interface", "clock-format"],
                    )
                } else {
                    None
                };
            if let Some(c) = gsettings.as_deref().and_then(parse_gsettings_clock_format) {
                return c;
            }
            resolve(
                gsettings.as_deref(),
                run_probe("locale", &["t_fmt"]).as_deref(),
            )
        }
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

    /// Formats a short time of day in the given time zone using this clock format.
    pub fn format_time<Tz: TimeZone>(self, dt: &DateTime<Tz>) -> String
    where
        Tz::Offset: Display,
    {
        let pattern = match self {
            ClockFormat::H24 => "%H:%M",
            ClockFormat::H12 => "%I:%M %p",
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

/// GNOME/GTK desktop identifiers that store clock preferences in
/// `org.gnome.desktop.interface clock-format`.
const GNOME_FAMILY_DESKTOPS: &[&str] = &[
    "GNOME",
    "UBUNTU",
    "UNITY",
    "UNITY7",
    "POP",
    "PANTHEON",
    "BUDGIE",
    "CINNAMON",
    "X-CINNAMON",
    "MATE",
    "ZORIN",
];

fn is_gnome_family_desktop(desktop: &str) -> bool {
    let trimmed = desktop.trim();
    trimmed
        .as_bytes()
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"GNOME-"))
        || GNOME_FAMILY_DESKTOPS
            .iter()
            .any(|name| trimmed.eq_ignore_ascii_case(name))
}

/// Returns `true` when `gsettings` should be probed for GNOME clock preferences.
///
/// Unset or empty `XDG_CURRENT_DESKTOP` values return `true` to preserve
/// probing in minimal environments; explicit non-GNOME desktops (such as KDE,
/// XFCE, Sway, Hyprland, LXQt, i3) return `false`.
pub fn should_probe_gsettings(xdg_current_desktop: Option<&str>) -> bool {
    let Some(raw) = xdg_current_desktop else {
        return true;
    };
    let mut saw_entry = false;
    for entry in raw.split(':').map(str::trim).filter(|s| !s.is_empty()) {
        saw_entry = true;
        if is_gnome_family_desktop(entry) {
            return true;
        }
    }
    !saw_entry
}

/// Parses `gsettings get org.gnome.desktop.interface clock-format` output.
pub fn parse_gsettings_clock_format(out: &str) -> Option<ClockFormat> {
    match out.trim().trim_matches('\'') {
        "24h" => Some(ClockFormat::H24),
        "12h" => Some(ClockFormat::H12),
        _ => None,
    }
}

/// Parses macOS `defaults read` boolean/integer scalars (`1`/`true`/`yes` vs
/// `0`/`false`/`no`).
fn parse_defaults_bool(raw: &str) -> Option<bool> {
    let trimmed = raw.trim();
    if trimmed == "1" || trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("yes")
    {
        Some(true)
    } else if trimmed == "0"
        || trimmed.eq_ignore_ascii_case("false")
        || trimmed.eq_ignore_ascii_case("no")
    {
        Some(false)
    } else {
        None
    }
}

/// Parses `defaults read -g AppleICUForce24HourTime` output.
pub fn parse_apple_force_24h(raw: &str) -> Option<ClockFormat> {
    parse_defaults_bool(raw).map(|force| {
        if force {
            ClockFormat::H24
        } else {
            ClockFormat::H12
        }
    })
}

/// Parses `defaults read -g AppleICUForce12HourTime` output.
pub fn parse_apple_force_12h(raw: &str) -> Option<ClockFormat> {
    parse_defaults_bool(raw).map(|force| {
        if force {
            ClockFormat::H12
        } else {
            ClockFormat::H24
        }
    })
}

const APPLE_H12_REGIONS: &[&str] = &[
    "AE", "AU", "BD", "BH", "CA", "CO", "DO", "EG", "GT", "HN", "IN", "JM", "JO", "KW", "LB", "MX",
    "MY", "NI", "NZ", "OM", "PH", "PK", "PR", "QA", "SA", "SV", "TT", "US", "VE",
];

const APPLE_H24_REGIONS: &[&str] = &[
    "AL", "AR", "AT", "BA", "BE", "BG", "BR", "BY", "CH", "CL", "CN", "CZ", "DE", "DK", "EE", "ES",
    "FI", "FR", "GB", "GR", "HK", "HR", "HU", "ID", "IE", "IL", "IS", "IT", "JP", "KR", "LT", "LU",
    "LV", "MK", "NL", "NO", "PE", "PL", "PT", "RO", "RS", "RU", "SE", "SI", "SK", "TH", "TR", "TW",
    "UA", "UY", "VN", "ZA",
];

fn extract_locale_region(base: &str) -> Option<&str> {
    let without_codeset = base.split('.').next().unwrap_or(base);
    let mut subtags = without_codeset.split(['_', '-']);
    // Skip the leading language subtag so e.g. `"de"` or `"in"` is never
    // mistaken for a region code.
    subtags.next()?;
    for subtag in subtags {
        if subtag.len() == 1 {
            // BCP 47 singletons (`u`, `t`, `x`) start extension/private-use subtags.
            break;
        }
        if subtag.len() == 2 && subtag.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Some(subtag);
        }
    }
    None
}

/// Parses `defaults read -g AppleLocale` output (e.g. `en_US`, `de_DE`, or
/// ICU overrides like `en_US@hours=h23` / `de_DE@hours=h12`).
pub fn parse_apple_locale_clock(raw: &str) -> Option<ClockFormat> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (base, keywords) = match trimmed.split_once('@') {
        Some((b, k)) => (b, Some(k)),
        None => (trimmed, None),
    };

    let mut rg_override: Option<&str> = None;
    if let Some(kw_str) = keywords {
        for pair in kw_str.split(';') {
            let Some((key, val)) = pair.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let val = val.trim();
            if key.eq_ignore_ascii_case("hours") {
                if val.eq_ignore_ascii_case("h12") || val.eq_ignore_ascii_case("h11") {
                    return Some(ClockFormat::H12);
                }
                if val.eq_ignore_ascii_case("h23") || val.eq_ignore_ascii_case("h24") {
                    return Some(ClockFormat::H24);
                }
            } else if key.eq_ignore_ascii_case("rg")
                && val.len() == 6
                && val.as_bytes()[..2].iter().all(u8::is_ascii_alphabetic)
                && val[2..].eq_ignore_ascii_case("zzzz")
            {
                rg_override = Some(&val[..2]);
            }
        }
    }

    let region = rg_override.or_else(|| extract_locale_region(base))?;
    if APPLE_H12_REGIONS
        .iter()
        .any(|r| region.eq_ignore_ascii_case(r))
    {
        Some(ClockFormat::H12)
    } else if APPLE_H24_REGIONS
        .iter()
        .any(|r| region.eq_ignore_ascii_case(r))
    {
        Some(ClockFormat::H24)
    } else {
        None
    }
}

/// Combines macOS `defaults` probe outputs with `locale t_fmt` fallback, then 24h.
pub fn resolve_macos(
    force_24h: Option<&str>,
    force_12h: Option<&str>,
    apple_locale: Option<&str>,
    t_fmt: Option<&str>,
) -> ClockFormat {
    force_24h
        .and_then(parse_apple_force_24h)
        .or_else(|| force_12h.and_then(parse_apple_force_12h))
        .or_else(|| apple_locale.and_then(parse_apple_locale_clock))
        .unwrap_or_else(|| resolve(None, t_fmt))
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
    fn formats_time_per_clock() {
        let tz = FixedOffset::east_opt(2 * 3600).unwrap();
        let dt = tz.with_ymd_and_hms(2026, 10, 11, 23, 5, 9).unwrap();
        assert_eq!(ClockFormat::H24.format_time(&dt), "23:05");
        assert_eq!(ClockFormat::H12.format_time(&dt), "11:05 PM");
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

    #[test]
    fn should_probe_gsettings_allows_unset_empty_and_gnome_family() {
        for desktop in [
            None,
            Some(""),
            Some("   "),
            Some(":::"),
            Some("GNOME"),
            Some("gnome"),
            Some("ubuntu:GNOME"),
            Some("UBUNTU"),
            Some("Unity"),
            Some("Unity7"),
            Some("pop:GNOME"),
            Some("Pop"),
            Some("Pantheon"),
            Some("Budgie:GNOME"),
            Some("Budgie"),
            Some("X-Cinnamon"),
            Some("Cinnamon"),
            Some("MATE"),
            Some("zorin:GNOME"),
            Some("GNOME-Classic:GNOME"),
            Some("GNOME-Flashback"),
            Some("KDE:GNOME"),
        ] {
            assert!(
                should_probe_gsettings(desktop),
                "expected should_probe_gsettings({desktop:?}) to be true"
            );
        }
    }

    #[test]
    fn should_probe_gsettings_skips_explicit_non_gnome_desktops() {
        for desktop in [
            "KDE",
            "kde",
            "XFCE",
            "xfce",
            "sway",
            "SWAY",
            "Hyprland",
            "HYPRLAND",
            "LXQt",
            "lxqt",
            "i3",
            "I3",
            "COSMIC",
            "KDE:Wayland",
        ] {
            assert!(
                !should_probe_gsettings(Some(desktop)),
                "expected should_probe_gsettings(Some({desktop:?})) to be false"
            );
        }
    }

    #[test]
    fn macos_force_24h_and_12h_parse_cleanly() {
        for truthy in ["1", "1\n", " true ", "TRUE", "yes", "Yes"] {
            assert_eq!(parse_apple_force_24h(truthy), Some(ClockFormat::H24));
            assert_eq!(parse_apple_force_12h(truthy), Some(ClockFormat::H12));
        }
        for falsy in ["0", "0\n", " false ", "FALSE", "no", "No"] {
            assert_eq!(parse_apple_force_24h(falsy), Some(ClockFormat::H12));
            assert_eq!(parse_apple_force_12h(falsy), Some(ClockFormat::H24));
        }
        for invalid in [
            "",
            "   ",
            "2",
            "-1",
            "maybe",
            "The domain/default pair does not exist",
        ] {
            assert_eq!(parse_apple_force_24h(invalid), None);
            assert_eq!(parse_apple_force_12h(invalid), None);
        }
    }

    #[test]
    fn apple_locale_honors_hours_override_and_region_tables() {
        // Explicit ICU @hours= overrides beat region defaults
        assert_eq!(
            parse_apple_locale_clock("en_US@hours=h23\n"),
            Some(ClockFormat::H24)
        );
        assert_eq!(
            parse_apple_locale_clock("en_US@hours=h24"),
            Some(ClockFormat::H24)
        );
        assert_eq!(
            parse_apple_locale_clock("de_DE@calendar=gregorian;hours=h12"),
            Some(ClockFormat::H12)
        );
        assert_eq!(
            parse_apple_locale_clock("en_US@rg=dezzzz"),
            Some(ClockFormat::H24)
        );

        // H12 regions
        for loc in [
            "en_US", "en-CA", "en_AU", "en_NZ", "en_PH", "en_IN", "ar_EG", "ar_SA", "es_CO",
            "es_MX",
        ] {
            assert_eq!(
                parse_apple_locale_clock(loc),
                Some(ClockFormat::H12),
                "expected H12 for {loc}"
            );
        }

        // H24 regions (including script subtag & codeset suffix)
        for loc in [
            "en_GB",
            "de_DE",
            "fr_FR",
            "es_ES",
            "it_IT",
            "ja_JP",
            "nl_NL",
            "sv_SE",
            "zh_Hans_CN",
            "de_DE.UTF-8",
        ] {
            assert_eq!(
                parse_apple_locale_clock(loc),
                Some(ClockFormat::H24),
                "expected H24 for {loc}"
            );
        }

        // Language-only (no region), unknown regions, and empty strings return None
        for unknown in ["", "   ", "C", "POSIX", "en", "de", "in", "en_ZZ", "es_419"] {
            assert_eq!(
                parse_apple_locale_clock(unknown),
                None,
                "expected None for {unknown:?}"
            );
        }

        // Region lookup tables must be disjoint
        for r in APPLE_H12_REGIONS {
            assert!(!APPLE_H24_REGIONS.contains(r), "overlapping region {r}");
        }
    }

    #[test]
    fn resolve_macos_follows_precedence_and_locale_fallback() {
        assert_eq!(
            resolve_macos(Some("1"), Some("1"), Some("en_US"), Some("%r")),
            ClockFormat::H24
        );
        assert_eq!(
            resolve_macos(Some("0"), Some("0"), Some("de_DE"), Some("%T")),
            ClockFormat::H12
        );
        assert_eq!(
            resolve_macos(None, Some("1"), Some("de_DE"), Some("%T")),
            ClockFormat::H12
        );
        assert_eq!(
            resolve_macos(None, None, Some("en_US"), Some("%T")),
            ClockFormat::H12
        );
        assert_eq!(
            resolve_macos(None, None, Some("en_ZZ"), Some("%r")),
            ClockFormat::H12
        );
        assert_eq!(resolve_macos(None, None, None, None), ClockFormat::H24);
    }
}
