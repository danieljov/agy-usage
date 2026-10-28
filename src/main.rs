use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use agy_usage::clock::ClockFormat;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum, ValueHint};
use clap_complete::Shell;

#[derive(Parser)]
#[command(
    name = "agy-usage",
    version,
    about = "Antigravity quota usage tracker and prognosis"
)]
struct Cli {
    #[arg(long, global = true, value_hint = ValueHint::DirPath)]
    data_dir: Option<PathBuf>,

    #[arg(long, global = true, default_value_t = 0.10)]
    tight_threshold: f64,

    #[arg(long, global = true, default_value_t = 30, value_parser = clap::value_parser!(u32).range(0..=36500))]
    history_days: u32,

    /// Days of raw samples to keep on disk [default: max(90, --history-days)]
    #[arg(long, global = true, value_parser = clap::value_parser!(u32).range(8..=36500))]
    retention_days: Option<u32>,

    /// Clock format for displayed times
    ///
    /// `auto` follows the GNOME clock setting, then the LC_TIME locale, then 24h.
    #[arg(long, global = true, value_enum, default_value_t = ClockArg::Auto)]
    clock: ClockArg,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, ValueEnum)]
enum ClockArg {
    Auto,
    #[value(name = "12h")]
    H12,
    #[value(name = "24h")]
    H24,
}

impl ClockArg {
    /// The user's explicit override, or `None` for auto-detection.
    fn explicit(self) -> Option<ClockFormat> {
        match self {
            ClockArg::Auto => None,
            ClockArg::H12 => Some(ClockFormat::H12),
            ClockArg::H24 => Some(ClockFormat::H24),
        }
    }

    fn resolve(self) -> ClockFormat {
        self.explicit().unwrap_or_else(ClockFormat::detect)
    }
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
        #[arg(long, value_hint = ValueHint::FilePath)]
        out: Option<PathBuf>,
    },
    /// Install systemd user timer for automated collection
    InstallTimer,
    /// Uninstall systemd user timer
    UninstallTimer,
    /// Generate shell completion scripts
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: Shell,
    },
}

fn write_completions(shell: Shell, out: &mut impl Write) -> Result<()> {
    let mut cmd = Cli::command();
    let bin_name = cmd.get_name().to_string();
    let mut buf = Vec::new();
    clap_complete::generate(shell, &mut cmd, bin_name, &mut buf);
    match out.write_all(&buf).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e).context("failed to write shell completions to stdout"),
    }
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

/// Loads only the samples needed to display `history_days` of history (see
/// [`agy_usage::forecast::analysis_cutoff`]) and analyzes them. Falls back to the full
/// history when a bucket sampled since the cutoff has a window of unknown length, whose
/// prognosis can depend on arbitrarily old samples. Either way, buckets with no recent
/// samples are omitted by [`agy_usage::forecast::analyze_since`].
fn load_reports(
    samples_path: &Path,
    now: chrono::DateTime<Utc>,
    history_days: u32,
    tight_threshold: f64,
) -> Result<Vec<agy_usage::forecast::BucketReport>> {
    use agy_usage::forecast;
    let cutoff = forecast::analysis_cutoff(now, history_days);
    let mut loaded = agy_usage::store::load_since(samples_path, cutoff)?;
    if !forecast::supports_bounded_analysis(&loaded.samples) {
        loaded = agy_usage::store::load(samples_path)?;
    }
    if loaded.skipped > 0 {
        eprintln!("warning: skipped {} corrupt lines", loaded.skipped);
    }
    Ok(forecast::analyze_since(
        loaded.samples,
        cutoff,
        tight_threshold,
    ))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let retention_days = effective_retention_days(cli.retention_days, cli.history_days)?;

    match cli.command {
        Commands::Completions { shell } => {
            write_completions(shell, &mut std::io::stdout())?;
        }
        Commands::UninstallTimer => {
            agy_usage::timer::uninstall()?;
            println!("uninstalled agy-usage timer");
        }
        Commands::Collect { no_html } => {
            let data_dir = resolve_data_dir(cli.data_dir)?;
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
                let reports =
                    load_reports(&samples_path, now, cli.history_days, cli.tight_threshold)?;
                let html_content =
                    agy_usage::html::render(&reports, now, cli.history_days, cli.clock.resolve());
                write_html_atomic(&data_dir.join("dashboard.html"), &html_content)?;
            }
        }
        Commands::Show { once } => {
            let data_dir = resolve_data_dir(cli.data_dir)?;
            let clock = cli.clock.resolve();
            if once {
                let now = Utc::now();
                let reports = load_reports(
                    &data_dir.join("samples.jsonl"),
                    now,
                    cli.history_days,
                    cli.tight_threshold,
                )?;
                agy_usage::tui::print_once(&reports, now, clock)?;
            } else {
                // The interactive TUI loads the full retained history itself (its
                // "All" range needs it), so nothing is pre-loaded here.
                let agy_bin = agy_usage::source::agy_bin();
                agy_usage::tui::run(&data_dir, cli.tight_threshold, &agy_bin, clock)?;
            }
        }
        Commands::Html { open, out } => {
            let data_dir = resolve_data_dir(cli.data_dir)?;
            let now = Utc::now();
            let reports = load_reports(
                &data_dir.join("samples.jsonl"),
                now,
                cli.history_days,
                cli.tight_threshold,
            )?;
            let html_content =
                agy_usage::html::render(&reports, now, cli.history_days, cli.clock.resolve());
            let out_path = out.unwrap_or_else(|| data_dir.join("dashboard.html"));
            write_html_atomic(&out_path, &html_content)?;
            println!("wrote HTML dashboard to {}", out_path.display());
            if open {
                #[cfg(target_os = "macos")]
                let opener = "open";
                #[cfg(not(target_os = "macos"))]
                let opener = "xdg-open";
                let _ = Command::new(opener).arg(&out_path).status();
            }
        }
        Commands::InstallTimer => {
            let data_dir = resolve_data_dir(cli.data_dir)?;
            let exe = std::env::current_exe()?;
            let agy_bin = agy_usage::source::resolve_agy_bin()?;
            agy_usage::timer::install(
                &exe,
                &agy_bin,
                &data_dir,
                cli.tight_threshold,
                cli.history_days,
                retention_days,
                cli.clock.explicit(),
            )?;
            #[cfg(target_os = "macos")]
            println!(
                "installed and loaded {} launchd agent",
                agy_usage::timer::LAUNCHD_LABEL
            );
            #[cfg(not(target_os = "macos"))]
            {
                println!("installed and started agy-usage.timer");
                println!("Hint: run 'loginctl enable-linger' to collect while logged out");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailWriter {
        write_err: Option<std::io::ErrorKind>,
        flush_err: Option<std::io::ErrorKind>,
    }

    impl Write for FailWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Some(kind) = self.write_err {
                return Err(std::io::Error::new(kind, "simulated write failure"));
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if let Some(kind) = self.flush_err {
                return Err(std::io::Error::new(kind, "simulated flush failure"));
            }
            Ok(())
        }
    }

    #[test]
    fn test_effective_retention_days() {
        assert_eq!(effective_retention_days(None, 30).unwrap(), 90);
        assert_eq!(effective_retention_days(None, 120).unwrap(), 120);
        assert_eq!(effective_retention_days(Some(30), 30).unwrap(), 30);
        assert_eq!(effective_retention_days(Some(120), 30).unwrap(), 120);
        assert!(effective_retention_days(Some(30), 60).is_err());
    }

    #[test]
    fn test_write_completions_handles_broken_pipe_and_propagates_other_io_errors() {
        let mut buf = Vec::new();
        write_completions(Shell::Bash, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("agy-usage"));
        assert!(text.contains("completions"));

        let mut broken_write = FailWriter {
            write_err: Some(std::io::ErrorKind::BrokenPipe),
            flush_err: None,
        };
        assert!(write_completions(Shell::Zsh, &mut broken_write).is_ok());

        let mut broken_flush = FailWriter {
            write_err: None,
            flush_err: Some(std::io::ErrorKind::BrokenPipe),
        };
        assert!(write_completions(Shell::Zsh, &mut broken_flush).is_ok());

        let mut denied_write = FailWriter {
            write_err: Some(std::io::ErrorKind::PermissionDenied),
            flush_err: None,
        };
        let err = write_completions(Shell::Fish, &mut denied_write).unwrap_err();
        assert!(
            err.to_string()
                .contains("failed to write shell completions to stdout")
        );

        let mut denied_flush = FailWriter {
            write_err: None,
            flush_err: Some(std::io::ErrorKind::PermissionDenied),
        };
        let err_flush = write_completions(Shell::Fish, &mut denied_flush).unwrap_err();
        assert!(
            err_flush
                .to_string()
                .contains("failed to write shell completions to stdout")
        );
    }
}
