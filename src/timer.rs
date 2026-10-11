use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

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

pub fn unit_files(exe: &Path, agy_bin: &Path, data_dir: &Path) -> Result<(String, String)> {
    let enc_exe = encode_unit_path(exe, true)?;
    let enc_agy = encode_unit_path(agy_bin, false)?;
    let enc_data = encode_unit_path(data_dir, true)?;

    let service = format!(
        r#"[Unit]
Description=Antigravity quota usage collector

[Service]
Type=oneshot
TimeoutStartSec=30s
Environment="AGY_BIN={}"
ExecStart="{}" --data-dir "{}" collect
"#,
        enc_agy, enc_exe, enc_data
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

pub fn install(exe: &Path, agy_bin: &Path, data_dir: &Path) -> Result<()> {
    let dir = systemd_user_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create systemd user dir {}", dir.display()))?;

    let (service_content, timer_content) = unit_files(exe, agy_bin, data_dir)?;
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

pub fn uninstall() -> Result<()> {
    let dir = systemd_user_dir()?;
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", "agy-usage.timer"])
        .status();

    let service_path = dir.join("agy-usage.service");
    let timer_path = dir.join("agy-usage.timer");
    let remove_if_exists = |p: &Path| -> Result<()> {
        match fs::remove_file(p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("failed to remove {}", p.display())),
        }
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_files_contents() {
        let exe = Path::new("/x/agy-usage");
        let agy = Path::new("/home/u/.local/bin/agy");
        let data = Path::new("/d");

        let (service, timer) = unit_files(exe, agy, data).unwrap();

        assert!(service.contains("TimeoutStartSec=30s"));
        assert!(service.contains("Environment=\"AGY_BIN=/home/u/.local/bin/agy\""));
        assert!(service.contains("ExecStart=\"/x/agy-usage\" --data-dir \"/d\" collect"));
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

        let (service, _timer) = unit_files(exe, agy, data).unwrap();

        assert!(service.contains("Environment=\"AGY_BIN=/opt/%%h/$USER/bin/agy\""));
        assert!(service.contains(
            "ExecStart=\"/bin/agy%%u$$test\\\"bar\" --data-dir \"/var/data/$$DIR/%%n\" collect"
        ));
    }

    #[test]
    fn unit_files_rejects_control_characters() {
        let exe = Path::new("/x/agy-usage");
        let agy = Path::new("/home/u/.local/bin/agy");
        let bad_newline = Path::new("/d\nattack");
        let bad_cr = Path::new("/d\rattack");

        assert!(unit_files(exe, agy, bad_newline).is_err());
        assert!(unit_files(exe, agy, bad_cr).is_err());
        assert!(unit_files(bad_newline, agy, Path::new("/d")).is_err());
        assert!(unit_files(exe, bad_cr, Path::new("/d")).is_err());
    }

    #[test]
    fn unit_files_resolves_relative_paths() {
        let exe = Path::new("agy-usage");
        let agy = Path::new("agy");
        let data = Path::new("data_dir");

        let (service, _timer) = unit_files(exe, agy, data).unwrap();
        // std::path::absolute resolves to absolute paths
        assert!(!service.contains("ExecStart=\"agy-usage\""));
        assert!(!service.contains("--data-dir \"data_dir\""));
        assert!(service.contains("collect"));
    }
}
