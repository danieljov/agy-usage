//! Live demo dashboard generator for `agy-usage`.
//!
//! Renders the self-contained HTML dashboard from the same synthetic quota
//! dataset used for the README screenshots and writes it to
//! `<OUT_DIR>/index.html`, ready to be published on GitHub Pages.
//! Unlike `generate_screenshots`, this does not require Chrome/Chromium.
//!
//! Run with:
//! ```bash
//! cargo run --example generate_demo              # writes target/pages/index.html
//! cargo run --example generate_demo -- site      # writes site/index.html
//! ```

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use agy_usage::clock::ClockFormat;
use agy_usage::forecast;
use agy_usage::html;
use anyhow::Context;
use chrono::{DateTime, Utc};

#[path = "common/demo_data.rs"]
mod demo_data;
use demo_data::generate_synthetic_samples;

const DEFAULT_OUT_DIR: &str = "target/pages";
const TIGHT_THRESHOLD: f64 = 0.10;
const HISTORY_DAYS: u32 = 30;
const USAGE: &str = "Usage: generate_demo [OUT_DIR] (default: target/pages)";

/// Anchor in `assets/dashboard.html` after which the demo banner is inserted.
const BODY_TAG: &str = "<body>";

/// Notice shown at the top of the hosted demo so visitors arriving from the
/// repository homepage know the data is synthetic and frozen at build time.
const DEMO_BANNER: &str = r#"
    <div role="note" style="margin:0 0 1.5rem;padding:0.75rem 1rem;border:1px solid #30363d;border-radius:8px;background:#161b22;color:#c9d1d9;font-size:0.9rem;">
        <strong>Live demo</strong> &mdash; synthetic quota data generated at build time, so relative times and statuses age until the next rebuild.
        Install <a href="https://github.com/danieljov/agy-usage" style="color:#58a6ff;">agy-usage</a> to track your own Antigravity quota.
    </div>"#;

/// Renders the demo dashboard HTML for the synthetic dataset ending at `now`.
fn render_demo(now: DateTime<Utc>) -> anyhow::Result<String> {
    let samples = generate_synthetic_samples(now);
    let reports = forecast::analyze(&samples, TIGHT_THRESHOLD);
    let dashboard = html::render(&reports, now, HISTORY_DAYS, ClockFormat::H24);
    anyhow::ensure!(
        dashboard.matches(BODY_TAG).count() == 1,
        "Expected exactly one {BODY_TAG} tag in dashboard template"
    );
    Ok(dashboard.replacen(BODY_TAG, &format!("{BODY_TAG}{DEMO_BANNER}"), 1))
}

/// Writes the demo dashboard to `<out_dir>/index.html`, creating `out_dir`
/// if needed, and returns the path of the written file.
fn write_demo(out_dir: &Path, now: DateTime<Utc>) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("Failed to create output directory {}", out_dir.display()))?;
    let index = out_dir.join("index.html");
    std::fs::write(&index, render_demo(now)?)
        .with_context(|| format!("Failed to write {}", index.display()))?;
    Ok(index)
}

/// Parses `[OUT_DIR]`; rejects extra arguments and flag-like values such as
/// `--help` so they are not mistaken for a directory name.
fn parse_out_dir(args: impl IntoIterator<Item = OsString>) -> anyhow::Result<PathBuf> {
    let mut args = args.into_iter();
    let out_dir = match args.next() {
        None => PathBuf::from(DEFAULT_OUT_DIR),
        Some(arg) if arg.to_string_lossy().starts_with('-') => anyhow::bail!(USAGE),
        Some(arg) => PathBuf::from(arg),
    };
    anyhow::ensure!(args.next().is_none(), USAGE);
    Ok(out_dir)
}

fn main() -> anyhow::Result<()> {
    let out_dir = parse_out_dir(std::env::args_os().skip(1))?;
    let index = write_demo(&out_dir, Utc::now())?;
    println!("Wrote demo dashboard to {}", index.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_html_is_self_contained_and_populated() {
        let html = render_demo(Utc::now()).expect("failed to render demo");
        assert!(html.starts_with("<!DOCTYPE html>"));
        // Banner must be present.
        assert!(html.contains("Live demo"));
        assert!(html.contains("synthetic quota data"));
        // Data and vendored scripts must be injected (no leftover placeholders).
        assert!(!html.contains("/*__AGY_DATA__*/null"));
        assert!(!html.contains("/*__CHART_JS__*/"));
        assert!(!html.contains("/*__DATE_FNS_ADAPTER__*/"));
        // Must not depend on external scripts when hosted.
        assert!(!html.contains("<script src="));
        for bucket in ["gemini-weekly", "gemini-5h", "3p-weekly", "3p-5h"] {
            assert!(html.contains(bucket), "missing bucket {bucket}");
        }
    }

    #[test]
    fn write_demo_creates_index_html_in_nested_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().join("nested").join("site");
        let index = write_demo(&out_dir, Utc::now()).unwrap();
        assert_eq!(index, out_dir.join("index.html"));
        let written = std::fs::read_to_string(&index).unwrap();
        assert!(written.contains("gemini-weekly"));
        assert!(written.contains("Live demo"));
    }

    #[test]
    fn parse_out_dir_handles_defaults_and_validation() {
        // Default when no args
        assert_eq!(
            parse_out_dir(Vec::<OsString>::new()).unwrap(),
            PathBuf::from(DEFAULT_OUT_DIR)
        );

        // Custom output directory
        assert_eq!(
            parse_out_dir(vec![OsString::from("custom/path")]).unwrap(),
            PathBuf::from("custom/path")
        );

        // Flags are rejected
        assert!(parse_out_dir(vec![OsString::from("--help")]).is_err());
        assert!(parse_out_dir(vec![OsString::from("-h")]).is_err());

        // Extra positional arguments are rejected
        assert!(parse_out_dir(vec![OsString::from("dir1"), OsString::from("extra")]).is_err());
    }
}
