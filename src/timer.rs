use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub fn unit_files(exe: &Path, agy_bin: &Path, data_dir: &Path) -> (String, String) {
    let service = format!(
        r#"[Unit]
Description=Antigravity quota usage collector

[Service]
Type=oneshot
Environment="AGY_BIN={}"
ExecStart="{}" --data-dir "{}" collect
"#,
        agy_bin.display(),
        exe.display(),
        data_dir.display()
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

    (service, timer)
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

    let (service_content, timer_content) = unit_files(exe, agy_bin, data_dir);
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
    let _ = fs::remove_file(service_path);
    let _ = fs::remove_file(timer_path);

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

        let (service, timer) = unit_files(exe, agy, data);

        assert!(service.contains("Environment=\"AGY_BIN=/home/u/.local/bin/agy\""));
        assert!(service.contains("ExecStart=\"/x/agy-usage\" --data-dir \"/d\" collect"));
        assert!(timer.contains("OnCalendar=*:0/5"));
        assert!(timer.contains("Persistent=true"));
        assert!(!timer.contains("OnUnitActiveSec"));
        assert!(!service.contains("OnUnitActiveSec"));
    }
}
