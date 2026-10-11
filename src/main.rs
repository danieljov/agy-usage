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

fn is_executable(p: &Path) -> bool {
    p.is_file() && rustix::fs::access(p, rustix::fs::Access::EXEC_OK).is_ok()
}

fn resolve_agy_bin() -> Result<PathBuf> {
    if let Some(val) = std::env::var("AGY_BIN").ok().filter(|s| !s.is_empty()) {
        let p = PathBuf::from(val);
        if is_executable(&p) {
            return Ok(p.canonicalize().unwrap_or(p));
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("agy");
            if is_executable(&candidate) {
                return Ok(candidate.canonicalize().unwrap_or(candidate));
            }
        }
    }
    bail!(
        "could not find executable 'agy' on PATH or via $AGY_BIN. Please ensure agy is installed or set AGY_BIN"
    );
}

fn write_html_atomic(path: &Path, content: &str) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
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
            let exe = std::env::current_exe()?.canonicalize()?;
            let agy_bin = resolve_agy_bin()?;
            agy_usage::timer::install(&exe, &agy_bin, &data_dir)?;
            println!("installed and started agy-usage.timer");
        }
        Commands::UninstallTimer => {
            agy_usage::timer::uninstall()?;
            println!("uninstalled agy-usage timer");
        }
    }

    Ok(())
}
