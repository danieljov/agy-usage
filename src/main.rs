use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "agy-usage",
    about = "Antigravity quota usage tracker and prognosis"
)]
struct Cli {
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[arg(long, global = true, default_value_t = 0.10)]
    tight_threshold: f64,

    #[arg(long, global = true, default_value_t = 30, value_parser = clap::value_parser!(u32).range(0..=36500))]
    history_days: u32,

    /// Days of raw samples to keep on disk [default: max(90, --history-days)]
    #[arg(long, global = true, value_parser = clap::value_parser!(u32).range(8..=36500))]
    retention_days: Option<u32>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Collect quota samples from agy and update dashboard
    Collect {
        #[arg(long)]
        no_html: bool,
    },
    /// Show terminal dashboard
    Show {
        #[arg(long)]
        once: bool,
    },
    /// Generate HTML dashboard
    Html {
        #[arg(long)]
        open: bool,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Install systemd user timer for automated collection
    InstallTimer,
    /// Uninstall systemd user timer
    UninstallTimer,
}

fn effective_retention_days(explicit: Option<u32>, history_days: u32) -> Result<u32> {
    match explicit {
        Some(r) if r < history_days => {
            bail!("--retention-days ({r}) cannot be less than --history-days ({history_days})")
        }
        Some(r) => Ok(r),
        None => Ok(history_days.max(90)),
    }
}

fn resolve_data_dir(data_dir: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = data_dir {
        Ok(dir)
    } else if let Some(xdg) = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
    {
        Ok(PathBuf::from(xdg).join("agy-usage"))
    } else {
        let home = std::env::var("HOME").context("HOME environment variable not set")?;
        Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("agy-usage"))
    }
}

fn write_html_atomic(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    use std::io::Write;
    temp.write_all(content.as_bytes())
        .with_context(|| "failed to write html content to temporary file")?;
    temp.flush()
        .with_context(|| "failed to flush temporary file")?;
    temp.persist(path).map_err(|e| {
        anyhow::anyhow!(
            "failed to persist temporary file to {}: {}",
            path.display(),
            e.error
        )
    })?;
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let retention_days = effective_retention_days(cli.retention_days, cli.history_days)?;
    let data_dir = resolve_data_dir(cli.data_dir)?;

    match cli.command {
        Commands::Collect { no_html } => {
            let agy_bin = agy_usage::source::agy_bin();
            let json = agy_usage::source::fetch(&agy_bin)?;
            let now = Utc::now();
            let samples = agy_usage::source::parse_usage(&json, now)?;
            if samples.is_empty() {
                bail!("no enabled quota buckets in agy output");
            }

            let samples_path = data_dir.join("samples.jsonl");
            agy_usage::store::append(&samples_path, &samples)?;
            println!("collected {} buckets", samples.len());

            let retention = chrono::Duration::days(i64::from(retention_days));
            let prune_threshold = now - retention - chrono::Duration::days(1);
            match agy_usage::store::oldest_timestamp(&samples_path) {
                Ok(Some(oldest)) if oldest <= prune_threshold => {
                    match agy_usage::store::prune(&samples_path, now - retention) {
                        Ok(n) => eprintln!("pruned {n} samples older than {retention_days} days"),
                        Err(e) => {
                            eprintln!("warning: failed to prune {}: {e}", samples_path.display())
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => eprintln!("warning: failed to inspect {}: {e}", samples_path.display()),
            }

            if !no_html {
                let loaded = agy_usage::store::load(&samples_path)?;
                if loaded.skipped > 0 {
                    eprintln!("warning: skipped {} corrupt lines", loaded.skipped);
                }
                let reports = agy_usage::forecast::analyze(&loaded.samples, cli.tight_threshold);
                let html_content = agy_usage::html::render(&reports, now, cli.history_days);
                write_html_atomic(&data_dir.join("dashboard.html"), &html_content)?;
            }
        }
        Commands::Show { once } => {
            let samples_path = data_dir.join("samples.jsonl");
            let loaded = agy_usage::store::load(&samples_path)?;
            if loaded.skipped > 0 {
                eprintln!("warning: skipped {} corrupt lines", loaded.skipped);
            }
            let reports = agy_usage::forecast::analyze(&loaded.samples, cli.tight_threshold);
            if once {
                agy_usage::tui::print_once(&reports, Utc::now())?;
            } else {
                let agy_bin = agy_usage::source::agy_bin();
                agy_usage::tui::run(&data_dir, cli.tight_threshold, &agy_bin)?;
            }
        }
        Commands::Html { open, out } => {
            let samples_path = data_dir.join("samples.jsonl");
            let loaded = agy_usage::store::load(&samples_path)?;
            if loaded.skipped > 0 {
                eprintln!("warning: skipped {} corrupt lines", loaded.skipped);
            }
            let reports = agy_usage::forecast::analyze(&loaded.samples, cli.tight_threshold);
            let html_content = agy_usage::html::render(&reports, Utc::now(), cli.history_days);
            let out_path = out.unwrap_or_else(|| data_dir.join("dashboard.html"));
            write_html_atomic(&out_path, &html_content)?;
            println!("wrote HTML dashboard to {}", out_path.display());
            if open {
                let _ = Command::new("xdg-open").arg(&out_path).status();
            }
        }
        Commands::InstallTimer => {
            let exe = std::env::current_exe()?;
            let agy_bin = agy_usage::source::resolve_agy_bin()?;
            agy_usage::timer::install(
                &exe,
                &agy_bin,
                &data_dir,
                cli.tight_threshold,
                cli.history_days,
                retention_days,
            )?;
            println!("installed and started agy-usage.timer");
            println!("Hint: run 'loginctl enable-linger' to collect while logged out");
        }
        Commands::UninstallTimer => {
            agy_usage::timer::uninstall()?;
            println!("uninstalled agy-usage timer");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effective_retention_days() {
        assert_eq!(effective_retention_days(None, 30).unwrap(), 90);
        assert_eq!(effective_retention_days(None, 120).unwrap(), 120);
        assert_eq!(effective_retention_days(Some(30), 30).unwrap(), 30);
        assert_eq!(effective_retention_days(Some(120), 30).unwrap(), 120);
        assert!(effective_retention_days(Some(30), 60).is_err());
    }
}
