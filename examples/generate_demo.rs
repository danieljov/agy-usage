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

/// Renders the demo dashboard HTML for the synthetic dataset ending at `now`.
fn render_demo(now: DateTime<Utc>) -> String {
    let samples = generate_synthetic_samples(now);
    let reports = forecast::analyze(&samples, TIGHT_THRESHOLD);
    html::render(&reports, now, HISTORY_DAYS, ClockFormat::H24)
}

/// Writes the demo dashboard to `<out_dir>/index.html`, creating `out_dir`
/// if needed, and returns the path of the written file.
fn write_demo(out_dir: &Path, now: DateTime<Utc>) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("Failed to create output directory {}", out_dir.display()))?;
    let index = out_dir.join("index.html");
    std::fs::write(&index, render_demo(now))
        .with_context(|| format!("Failed to write {}", index.display()))?;
    Ok(index)
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let out_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_OUT_DIR));
    anyhow::ensure!(
        args.next().is_none(),
        "Usage: generate_demo [OUT_DIR] (default: {DEFAULT_OUT_DIR})"
    );

    let index = write_demo(&out_dir, Utc::now())?;
    println!("Wrote demo dashboard to {}", index.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_html_is_self_contained_and_populated() {
        let html = render_demo(Utc::now());
        assert!(html.starts_with("<!DOCTYPE html>") || html.starts_with("<!doctype html>"));
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
    }
}
