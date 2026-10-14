//! Demo screenshot generator for `agy-usage`.
//!
//! Generates realistic synthetic quota samples starting at 100% and captures
//! high-resolution screenshots of both the interactive TUI dashboard and
//! self-contained HTML dashboard using headless Chrome or Chromium.
//!
//! Run with:
//! ```bash
//! cargo run --example generate_screenshots
//! ```

use std::path::Path;
use std::process::Command;

use agy_usage::clock::ClockFormat;
use agy_usage::forecast::{self, BucketReport};
use agy_usage::html;
use agy_usage::tui::{self, ViewState};
use anyhow::Context;
use chrono::{DateTime, Utc};
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

#[path = "common/demo_data.rs"]
mod demo_data;
use demo_data::generate_synthetic_samples;

fn resolve_chrome_bin() -> anyhow::Result<String> {
    if let Some(bin) = std::env::var("CHROME_BIN").ok().filter(|b| !b.is_empty()) {
        return Ok(bin);
    }
    let candidates = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ];
    for candidate in candidates {
        if Command::new(candidate)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Ok(candidate.to_string());
        }
    }
    anyhow::bail!(
        "Failed to locate a Chrome or Chromium executable. \
         Please ensure google-chrome or chromium is installed in PATH, or set CHROME_BIN."
    )
}

fn color_to_css(color: Color) -> Option<String> {
    match color {
        Color::Reset => None,
        Color::Black => Some("#0d1117".into()),
        Color::Red => Some("#f85149".into()),
        Color::Green => Some("#3fb950".into()),
        Color::Yellow => Some("#d29922".into()),
        Color::Blue => Some("#58a6ff".into()),
        Color::Magenta => Some("#bc8cff".into()),
        Color::Cyan => Some("#39c5cf".into()),
        Color::Gray => Some("#8b949e".into()),
        Color::DarkGray => Some("#484f58".into()),
        Color::LightRed => Some("#ff7b72".into()),
        Color::LightGreen => Some("#56d364".into()),
        Color::LightYellow => Some("#e3b341".into()),
        Color::LightBlue => Some("#79c0ff".into()),
        Color::LightMagenta => Some("#d2a8ff".into()),
        Color::LightCyan => Some("#56d4dd".into()),
        Color::White => Some("#f0f6fc".into()),
        Color::Indexed(i) => Some(format!("var(--ansi-{i}, #c9d1d9)")),
        Color::Rgb(r, g, b) => Some(format!("rgb({r},{g},{b})")),
    }
}

fn box_class(sym: &str) -> Option<&'static str> {
    match sym {
        "─" => Some("box-h"),
        "│" => Some("box-v"),
        "┌" => Some("box-tl"),
        "┐" => Some("box-tr"),
        "└" => Some("box-bl"),
        "┘" => Some("box-br"),
        "├" => Some("box-vl"),
        "┤" => Some("box-vr"),
        "┬" => Some("box-ht"),
        "┴" => Some("box-hb"),
        "┼" => Some("box-vh"),
        _ => None,
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_tui_to_html(
    reports: &[BucketReport],
    now: DateTime<Utc>,
    width: u16,
    height: u16,
    clock: ClockFormat,
) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create test terminal");
    let state = ViewState {
        is_once: false,
        clock,
        ..Default::default()
    };
    terminal
        .draw(|f| tui::draw(f, reports, &state, now))
        .expect("failed to draw frame");
    let buffer = terminal.backend().buffer();

    let mut rows_html = String::new();
    for y in 0..height {
        rows_html.push_str("<div class=\"row\">");
        let mut x = 0;
        while x < width {
            let cell = &buffer[(x, y)];
            let sym = cell.symbol();
            let w = UnicodeWidthStr::width(sym);
            let is_bold = cell.modifier.contains(Modifier::BOLD);

            let mut styles = Vec::new();
            if let Some(c) = color_to_css(cell.fg) {
                styles.push(format!("color:{c}"));
            }
            if let Some(c) = color_to_css(cell.bg) {
                styles.push(format!("background-color:{c}"));
            }
            if is_bold {
                styles.push("font-weight:bold".into());
            }

            let cell_width_px = if w > 1 { w * 9 } else { 9 };
            let style_attr = if styles.is_empty() {
                String::new()
            } else {
                format!(";{}", styles.join(";"))
            };

            if let Some(box_cls) = box_class(sym) {
                rows_html.push_str(&format!(
                    "<span class=\"cell {box_cls}\" style=\"width:{cell_width_px}px{style_attr}\"></span>"
                ));
            } else {
                let escaped = if sym == " " {
                    "&nbsp;".to_string()
                } else {
                    escape_html(sym)
                };
                rows_html.push_str(&format!(
                    "<span class=\"cell\" style=\"width:{cell_width_px}px{style_attr}\">{escaped}</span>"
                ));
            }

            x += (w.max(1)) as u16;
        }
        rows_html.push_str("</div>\n");
    }

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0;
    padding: 32px;
    background: #090d13;
    display: flex;
    justify-content: center;
    align-items: center;
    font-family: "DejaVu Sans Mono", "Ubuntu Mono", "Liberation Mono", monospace;
  }}
  .window {{
    background: #0d1117;
    border: 1px solid #30363d;
    border-radius: 12px;
    box-shadow: 0 20px 48px rgba(0, 0, 0, 0.75), 0 0 0 1px rgba(255, 255, 255, 0.05);
    overflow: hidden;
    display: inline-block;
  }}
  .titlebar {{
    background: #161b22;
    padding: 12px 18px;
    display: flex;
    align-items: center;
    border-bottom: 1px solid #21262d;
  }}
  .dots {{
    display: flex;
    gap: 8px;
  }}
  .dot {{
    width: 12px;
    height: 12px;
    border-radius: 50%;
  }}
  .dot-red {{ background: #ff5f56; }}
  .dot-yellow {{ background: #ffbd2e; }}
  .dot-green {{ background: #27c93f; }}
  .title {{
    flex-grow: 1;
    text-align: center;
    color: #8b949e;
    font-size: 13px;
    font-weight: 500;
    margin-right: 48px;
    letter-spacing: 0.3px;
  }}
  .terminal-body {{
    padding: 18px 24px;
    background: #0d1117;
    user-select: none;
  }}
  .row {{
    display: flex;
    height: 18px;
    line-height: 18px;
    white-space: nowrap;
  }}
  .cell {{
    display: inline-flex;
    justify-content: center;
    align-items: center;
    height: 18px;
    flex-shrink: 0;
    font-size: 14px;
    color: #c9d1d9;
    position: relative;
    overflow: visible;
  }}

  /* Crisp vector box-drawing borders connecting edges seamlessly */
  .box-h::after {{
    content: ""; position: absolute; left: 0; right: 0; top: 50%; height: 1px;
    background: currentColor; transform: translateY(-50%);
  }}
  .box-v::after {{
    content: ""; position: absolute; top: 0; bottom: 0; left: 50%; width: 1px;
    background: currentColor; transform: translateX(-50%);
  }}
  .box-tl::before {{
    content: ""; position: absolute; left: 50%; right: 0; top: 50%; height: 1px;
    background: currentColor; transform: translateY(-50%);
  }}
  .box-tl::after {{
    content: ""; position: absolute; top: 50%; bottom: 0; left: 50%; width: 1px;
    background: currentColor; transform: translateX(-50%);
  }}
  .box-tr::before {{
    content: ""; position: absolute; left: 0; right: 50%; top: 50%; height: 1px;
    background: currentColor; transform: translateY(-50%);
  }}
  .box-tr::after {{
    content: ""; position: absolute; top: 50%; bottom: 0; left: 50%; width: 1px;
    background: currentColor; transform: translateX(-50%);
  }}
  .box-bl::before {{
    content: ""; position: absolute; left: 50%; right: 0; top: 50%; height: 1px;
    background: currentColor; transform: translateY(-50%);
  }}
  .box-bl::after {{
    content: ""; position: absolute; top: 0; bottom: 50%; left: 50%; width: 1px;
    background: currentColor; transform: translateX(-50%);
  }}
  .box-br::before {{
    content: ""; position: absolute; left: 0; right: 50%; top: 50%; height: 1px;
    background: currentColor; transform: translateY(-50%);
  }}
  .box-br::after {{
    content: ""; position: absolute; top: 0; bottom: 50%; left: 50%; width: 1px;
    background: currentColor; transform: translateX(-50%);
  }}
  .box-vl::before {{
    content: ""; position: absolute; top: 0; bottom: 0; left: 50%; width: 1px;
    background: currentColor; transform: translateX(-50%);
  }}
  .box-vl::after {{
    content: ""; position: absolute; left: 50%; right: 0; top: 50%; height: 1px;
    background: currentColor; transform: translateY(-50%);
  }}
  .box-vr::before {{
    content: ""; position: absolute; top: 0; bottom: 0; left: 50%; width: 1px;
    background: currentColor; transform: translateX(-50%);
  }}
  .box-vr::after {{
    content: ""; position: absolute; left: 0; right: 50%; top: 50%; height: 1px;
    background: currentColor; transform: translateY(-50%);
  }}
  .box-ht::before {{
    content: ""; position: absolute; left: 0; right: 0; top: 50%; height: 1px;
    background: currentColor; transform: translateY(-50%);
  }}
  .box-ht::after {{
    content: ""; position: absolute; top: 50%; bottom: 0; left: 50%; width: 1px;
    background: currentColor; transform: translateX(-50%);
  }}
  .box-hb::before {{
    content: ""; position: absolute; left: 0; right: 0; top: 50%; height: 1px;
    background: currentColor; transform: translateY(-50%);
  }}
  .box-hb::after {{
    content: ""; position: absolute; top: 0; bottom: 50%; left: 50%; width: 1px;
    background: currentColor; transform: translateX(-50%);
  }}
  .box-vh::before {{
    content: ""; position: absolute; left: 0; right: 0; top: 50%; height: 1px;
    background: currentColor; transform: translateY(-50%);
  }}
  .box-vh::after {{
    content: ""; position: absolute; top: 0; bottom: 0; left: 50%; width: 1px;
    background: currentColor; transform: translateX(-50%);
  }}
</style>
</head>
<body>
  <div class="window">
    <div class="titlebar">
      <div class="dots">
        <div class="dot dot-red"></div>
        <div class="dot dot-yellow"></div>
        <div class="dot dot-green"></div>
      </div>
      <div class="title">agy-usage show</div>
    </div>
    <div class="terminal-body">
{}
    </div>
  </div>
</body>
</html>"#,
        rows_html
    )
}

fn main() -> anyhow::Result<()> {
    let chrome_bin = resolve_chrome_bin()?;
    println!("Using Chrome/Chromium binary: {chrome_bin}");

    let now = Utc::now();
    let samples = generate_synthetic_samples(now);
    let reports = forecast::analyze(&samples, 0.10);

    println!("Analyzed {} buckets:", reports.len());
    for r in &reports {
        println!(
            "  - {}: status={}, rate={:?}, remaining={:.1}%",
            r.bucket_id,
            r.prognosis.status_label(now),
            r.prognosis.rate_per_h,
            r.prognosis.remaining_now * 100.0
        );
    }

    // 1. Render and capture HTML dashboard
    let html_content = html::render(&reports, now, 30, ClockFormat::H24);
    let html_content = html_content
        .replace(
            "maintainAspectRatio: false,",
            "maintainAspectRatio: false, animation: false,",
        )
        // Disable transitions to eliminate mid-transition rendering artifacts
        .replace(
            "transition: color 0.15s ease, background 0.15s ease;",
            "transition: none !important;",
        );

    anyhow::ensure!(
        html_content.contains("animation: false,"),
        "Failed to inject 'animation: false,' into dashboard HTML"
    );
    anyhow::ensure!(
        html_content.contains("transition: none !important;"),
        "Failed to disable transitions in dashboard HTML"
    );
    anyhow::ensure!(
        html_content.contains("class=\"range-btn active\" data-range=\"5h\""),
        "Expected 5h range button active in dashboard HTML"
    );
    anyhow::ensure!(
        html_content.contains("class=\"range-btn active\" data-range=\"7d\""),
        "Expected 7d range button active in dashboard HTML"
    );

    let target_dir = Path::new("target");
    std::fs::create_dir_all(target_dir)?;
    let dashboard_html_path = target_dir.join("demo_dashboard.html");
    std::fs::write(&dashboard_html_path, html_content)?;

    let screenshots_dir = Path::new("assets/screenshots");
    std::fs::create_dir_all(screenshots_dir)?;
    let dashboard_png = screenshots_dir.join("dashboard.png");

    println!("Capturing HTML dashboard screenshot...");
    let chrome_dash = Command::new(&chrome_bin)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            "--window-size=1280,1080",
            &format!("--screenshot={}", dashboard_png.display()),
            &format!("file://{}", dashboard_html_path.canonicalize()?.display()),
        ])
        .output()
        .with_context(|| format!("Failed to execute {chrome_bin} for dashboard capture"))?;

    if !chrome_dash.status.success() {
        anyhow::bail!(
            "Chrome dashboard capture failed with status {}: {}",
            chrome_dash.status,
            String::from_utf8_lossy(&chrome_dash.stderr)
        );
    }
    println!("Captured {}", dashboard_png.display());

    // 2. Render and capture TUI dashboard
    let tui_html = render_tui_to_html(&reports, now, 110, 27, ClockFormat::H24);
    let tui_html_path = target_dir.join("demo_tui.html");
    std::fs::write(&tui_html_path, tui_html)?;

    let tui_png = screenshots_dir.join("tui.png");
    println!("Capturing TUI screenshot...");
    let chrome_tui = Command::new(&chrome_bin)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            "--window-size=1160,780",
            &format!("--screenshot={}", tui_png.display()),
            &format!("file://{}", tui_html_path.canonicalize()?.display()),
        ])
        .output()
        .with_context(|| format!("Failed to execute {chrome_bin} for TUI capture"))?;

    if !chrome_tui.status.success() {
        anyhow::bail!(
            "Chrome TUI capture failed with status {}: {}",
            chrome_tui.status,
            String::from_utf8_lossy(&chrome_tui.stderr)
        );
    }
    println!("Captured {}", tui_png.display());

    // Clean up temporary HTML files unless KEEP_TEMP=1 is set
    let keep_temp = std::env::var("KEEP_TEMP")
        .map(|v| v == "1")
        .unwrap_or(false);
    if !keep_temp {
        let _ = std::fs::remove_file(&dashboard_html_path);
        let _ = std::fs::remove_file(&tui_html_path);
    }

    Ok(())
}
