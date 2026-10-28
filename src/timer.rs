use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::clock::ClockFormat;

pub const LAUNCHD_LABEL: &str = "com.agy-usage.collect";

fn encode_unit_path(path: &Path, escape_dollar: bool) -> Result<String> {
    let abs = std::path::absolute(path)
        .with_context(|| format!("failed to make path absolute: {}", path.display()))?;
    let s = abs
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("path contains invalid UTF-8: {}", abs.display()))?;
    if s.chars().any(|c| c.is_control()) {
        bail!("path contains control characters: {:?}", s);
    }
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => escaped.push_str("%%"),
            '$' if escape_dollar => escaped.push_str("$$"),
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            other => escaped.push(other),
        }
    }
    Ok(escaped)
}

pub fn unit_files(
    exe: &Path,
    agy_bin: &Path,
    data_dir: &Path,
    tight_threshold: f64,
    history_days: u32,
    retention_days: u32,
    clock: Option<ClockFormat>,
) -> Result<(String, String)> {
    let enc_exe = encode_unit_path(exe, true)?;
    let enc_agy = encode_unit_path(agy_bin, false)?;
    let enc_data = encode_unit_path(data_dir, true)?;
    // Only pin the clock when explicitly overridden; otherwise `collect`
    // keeps auto-detecting so later desktop setting changes are honored.
    let clock_arg = clock
        .map(|c| format!(" --clock {}", c.as_arg()))
        .unwrap_or_default();

    let service = format!(
        r#"[Unit]
Description=Antigravity quota usage collector

[Service]
Type=oneshot
TimeoutStartSec=30s
Environment="AGY_BIN={}"
ExecStart="{}" --data-dir "{}" --tight-threshold {} --history-days {} --retention-days {}{} collect
"#,
        enc_agy, enc_exe, enc_data, tight_threshold, history_days, retention_days, clock_arg
    );

    let timer = r#"[Unit]
Description=Antigravity quota usage collector timer

[Timer]
OnCalendar=*:0/5
Persistent=true
AccuracySec=30s

[Install]
WantedBy=timers.target
"#
    .to_string();

    Ok((service, timer))
}

fn encode_xml_text(s: &str) -> Result<String> {
    if s.chars().any(|c| c.is_control()) {
        bail!("string contains control characters: {:?}", s);
    }
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    Ok(escaped)
}

fn encode_plist_path(path: &Path) -> Result<String> {
    let abs = std::path::absolute(path)
        .with_context(|| format!("failed to make path absolute: {}", path.display()))?;
    let s = abs
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("path contains invalid UTF-8: {}", abs.display()))?;
    encode_xml_text(s)
}

pub fn launchd_plist(
    exe: &Path,
    agy_bin: &Path,
    data_dir: &Path,
    tight_threshold: f64,
    history_days: u32,
    retention_days: u32,
    clock: Option<ClockFormat>,
) -> Result<String> {
    let enc_exe = encode_plist_path(exe)?;
    let enc_agy = encode_plist_path(agy_bin)?;
    let enc_data = encode_plist_path(data_dir)?;

    let mut args = vec![
        enc_exe,
        "--data-dir".to_string(),
        enc_data,
        "--tight-threshold".to_string(),
        tight_threshold.to_string(),
        "--history-days".to_string(),
        history_days.to_string(),
        "--retention-days".to_string(),
        retention_days.to_string(),
    ];
    if let Some(c) = clock {
        args.push("--clock".to_string());
        args.push(c.as_arg().to_string());
    }
    args.push("collect".to_string());

    let mut program_args = String::new();
    for arg in &args {
        program_args.push_str(&format!("        <string>{arg}</string>\n"));
    }

    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{program_args}    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>AGY_BIN</key>
        <string>{enc_agy}</string>
    </dict>
    <key>StartInterval</key>
    <integer>300</integer>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#
    ))
}

pub fn launchd_agents_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join("Library").join("LaunchAgents"))
}

pub fn systemd_user_dir() -> Result<PathBuf> {
    if let Some(config_home) = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
    {
        Ok(PathBuf::from(config_home).join("systemd").join("user"))
    } else {
        let home = std::env::var("HOME").context("HOME not set")?;
        Ok(PathBuf::from(home)
            .join(".config")
            .join("systemd")
            .join("user"))
    }
}

fn remove_if_exists(p: &Path) -> Result<()> {
    match fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("failed to remove {}", p.display())),
    }
}

pub fn install(
    exe: &Path,
    agy_bin: &Path,
    data_dir: &Path,
    tight_threshold: f64,
    history_days: u32,
    retention_days: u32,
    clock: Option<ClockFormat>,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let dir = launchd_agents_dir()?;
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create LaunchAgents dir {}", dir.display()))?;
        let plist_content = launchd_plist(
            exe,
            agy_bin,
            data_dir,
            tight_threshold,
            history_days,
            retention_days,
            clock,
        )?;
        let plist_path = dir.join(format!("{LAUNCHD_LABEL}.plist"));
        fs::write(&plist_path, plist_content)
            .with_context(|| format!("failed to write {}", plist_path.display()))?;

        let uid = rustix::process::getuid().as_raw();
        let gui_domain = format!("gui/{uid}");
        let _ = Command::new("launchctl")
            .args(["bootout", &gui_domain])
            .arg(&plist_path)
            .status();

        let bootstrap = Command::new("launchctl")
            .args(["bootstrap", &gui_domain])
            .arg(&plist_path)
            .status()
            .context("failed to run launchctl bootstrap")?;
        if !bootstrap.success() {
            bail!(
                "launchctl bootstrap {} {} failed",
                gui_domain,
                plist_path.display()
            );
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let dir = systemd_user_dir()?;
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create systemd user dir {}", dir.display()))?;

        let (service_content, timer_content) = unit_files(
            exe,
            agy_bin,
            data_dir,
            tight_threshold,
            history_days,
            retention_days,
            clock,
        )?;
        let service_path = dir.join("agy-usage.service");
        let timer_path = dir.join("agy-usage.timer");

        fs::write(&service_path, service_content)
            .with_context(|| format!("failed to write {}", service_path.display()))?;
        fs::write(&timer_path, timer_content)
            .with_context(|| format!("failed to write {}", timer_path.display()))?;

        let reload = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status()
            .context("failed to run systemctl --user daemon-reload")?;
        if !reload.success() {
            bail!("systemctl --user daemon-reload failed");
        }

        let enable = Command::new("systemctl")
            .args(["--user", "enable", "--now", "agy-usage.timer"])
            .status()
            .context("failed to run systemctl --user enable --now agy-usage.timer")?;
        if !enable.success() {
            bail!("systemctl --user enable --now agy-usage.timer failed");
        }

        Ok(())
    }
}

pub fn uninstall() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let dir = launchd_agents_dir()?;
        let plist_path = dir.join(format!("{LAUNCHD_LABEL}.plist"));
        let uid = rustix::process::getuid().as_raw();
        let gui_domain = format!("gui/{uid}");
        let _ = Command::new("launchctl")
            .args(["bootout", &gui_domain])
            .arg(&plist_path)
            .status();
        remove_if_exists(&plist_path)?;
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let dir = systemd_user_dir()?;
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", "agy-usage.timer"])
            .status();

        let service_path = dir.join("agy-usage.service");
        let timer_path = dir.join("agy-usage.timer");
        remove_if_exists(&service_path)?;
        remove_if_exists(&timer_path)?;

        let reload = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status()
            .context("failed to run systemctl --user daemon-reload")?;
        if !reload.success() {
            bail!("systemctl --user daemon-reload failed");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_files_contents() {
        let exe = Path::new("/x/agy-usage");
        let agy = Path::new("/home/u/.local/bin/agy");
        let data = Path::new("/d");

        let (service, timer) = unit_files(exe, agy, data, 0.10, 30, 90, None).unwrap();

        assert!(service.contains("TimeoutStartSec=30s"));
        assert!(service.contains("Environment=\"AGY_BIN=/home/u/.local/bin/agy\""));
        assert!(service.contains(
            "ExecStart=\"/x/agy-usage\" --data-dir \"/d\" --tight-threshold 0.1 --history-days 30 --retention-days 90 collect"
        ));
        assert!(timer.contains("OnCalendar=*:0/5"));
        assert!(timer.contains("Persistent=true"));
        assert!(!timer.contains("OnUnitActiveSec"));
        assert!(!service.contains("OnUnitActiveSec"));
    }

    #[test]
    fn unit_files_escapes_systemd_directives() {
        let exe = Path::new("/bin/agy%u$test\"bar");
        let agy = Path::new("/opt/%h/$USER/bin/agy");
        let data = Path::new("/var/data/$DIR/%n");

        let (service, _timer) = unit_files(exe, agy, data, 0.15, 14, 90, None).unwrap();

        assert!(service.contains("Environment=\"AGY_BIN=/opt/%%h/$USER/bin/agy\""));
        assert!(service.contains(
            "ExecStart=\"/bin/agy%%u$$test\\\"bar\" --data-dir \"/var/data/$$DIR/%%n\" --tight-threshold 0.15 --history-days 14 --retention-days 90 collect"
        ));
    }

    #[test]
    fn unit_files_rejects_control_characters() {
        let exe = Path::new("/x/agy-usage");
        let agy = Path::new("/home/u/.local/bin/agy");
        let bad_newline = Path::new("/d\nattack");
        let bad_cr = Path::new("/d\rattack");

        assert!(unit_files(exe, agy, bad_newline, 0.10, 30, 90, None).is_err());
        assert!(unit_files(exe, agy, bad_cr, 0.10, 30, 90, None).is_err());
        assert!(unit_files(bad_newline, agy, Path::new("/d"), 0.10, 30, 90, None).is_err());
        assert!(unit_files(exe, bad_cr, Path::new("/d"), 0.10, 30, 90, None).is_err());
    }

    #[test]
    fn unit_files_resolves_relative_paths() {
        let exe = Path::new("agy-usage");
        let agy = Path::new("agy");
        let data = Path::new("data_dir");

        let (service, _timer) = unit_files(exe, agy, data, 0.10, 30, 90, None).unwrap();
        // std::path::absolute resolves to absolute paths
        assert!(!service.contains("ExecStart=\"agy-usage\""));
        assert!(!service.contains("--data-dir \"data_dir\""));
        assert!(
            service.contains("--tight-threshold 0.1 --history-days 30 --retention-days 90 collect")
        );
    }

    #[test]
    fn unit_files_propagates_retention_days() {
        let (service, _timer) = unit_files(
            Path::new("/x/agy-usage"),
            Path::new("/usr/bin/agy"),
            Path::new("/d"),
            0.10,
            30,
            120,
            None,
        )
        .unwrap();
        assert!(service.contains("--history-days 30 --retention-days 120 collect"));
    }

    #[test]
    fn unit_files_propagates_explicit_clock() {
        let (service, _timer) = unit_files(
            Path::new("/x/agy-usage"),
            Path::new("/usr/bin/agy"),
            Path::new("/d"),
            0.10,
            30,
            90,
            Some(ClockFormat::H12),
        )
        .unwrap();
        assert!(service.contains("--retention-days 90 --clock 12h collect"));
    }

    #[test]
    fn unit_files_omits_clock_when_auto() {
        let (service, _timer) = unit_files(
            Path::new("/x/agy-usage"),
            Path::new("/usr/bin/agy"),
            Path::new("/d"),
            0.10,
            30,
            90,
            None,
        )
        .unwrap();
        assert!(!service.contains("--clock"));
    }

    #[test]
    fn launchd_plist_contents_and_clock_propagation() {
        let exe = Path::new("/usr/local/bin/agy-usage");
        let agy = Path::new("/opt/homebrew/bin/agy");
        let data = Path::new("/Users/alice/.local/share/agy-usage");

        let plist = launchd_plist(exe, agy, data, 0.10, 30, 90, None).unwrap();
        assert!(plist.contains("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(plist.contains(&format!("<string>{LAUNCHD_LABEL}</string>")));
        assert!(plist.contains("<key>ProgramArguments</key>"));
        assert!(plist.contains("<string>/usr/local/bin/agy-usage</string>"));
        assert!(plist.contains("<string>--data-dir</string>"));
        assert!(plist.contains("<string>/Users/alice/.local/share/agy-usage</string>"));
        assert!(plist.contains("<string>--tight-threshold</string>"));
        assert!(plist.contains("<string>0.1</string>"));
        assert!(plist.contains("<string>--history-days</string>"));
        assert!(plist.contains("<string>30</string>"));
        assert!(plist.contains("<string>--retention-days</string>"));
        assert!(plist.contains("<string>90</string>"));
        assert!(plist.contains("<string>collect</string>"));
        assert!(!plist.contains("<string>--clock</string>"));
        assert!(plist.contains("<key>AGY_BIN</key>"));
        assert!(plist.contains("<string>/opt/homebrew/bin/agy</string>"));
        assert!(plist.contains("<key>StartInterval</key>"));
        assert!(plist.contains("<integer>300</integer>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<true/>"));

        let plist_12h =
            launchd_plist(exe, agy, data, 0.15, 14, 120, Some(ClockFormat::H12)).unwrap();
        assert!(plist_12h.contains("<string>--clock</string>"));
        assert!(plist_12h.contains("<string>12h</string>"));
        assert!(plist_12h.contains("<string>120</string>"));
    }

    #[test]
    fn launchd_plist_escapes_xml_and_preserves_percent_and_dollar() {
        let exe = Path::new("/Users/a&b/bin/agy<1>\"'usage");
        let agy = Path::new("/opt/$USER/%h/agy&bin");
        let data = Path::new("/Users/a&b/data<'dir'>");

        let plist = launchd_plist(exe, agy, data, 0.10, 30, 90, None).unwrap();
        // XML special chars must be escaped
        assert!(plist.contains("/Users/a&amp;b/bin/agy&lt;1&gt;&quot;&apos;usage"));
        assert!(plist.contains("/Users/a&amp;b/data&lt;&apos;dir&apos;&gt;"));
        // Negative boundary: systemd specifiers (% and $) must NOT be doubled in launchd plists
        assert!(plist.contains("/opt/$USER/%h/agy&amp;bin"));
        assert!(!plist.contains("$$USER"));
        assert!(!plist.contains("%%h"));
    }

    #[test]
    fn launchd_plist_rejects_control_characters() {
        let exe = Path::new("/x/agy-usage");
        let agy = Path::new("/usr/local/bin/agy");
        let bad_newline = Path::new("/Users/alice/\nattack");
        let bad_nul = Path::new("/Users/alice/\x07bell");

        assert!(launchd_plist(exe, agy, bad_newline, 0.10, 30, 90, None).is_err());
        assert!(launchd_plist(exe, agy, bad_nul, 0.10, 30, 90, None).is_err());
        assert!(launchd_plist(bad_newline, agy, Path::new("/d"), 0.10, 30, 90, None).is_err());
        assert!(launchd_plist(exe, bad_nul, Path::new("/d"), 0.10, 30, 90, None).is_err());
    }
}
