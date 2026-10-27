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
use chrono::{DateTime, NaiveDate, Utc};
use ratatui::buffer::Buffer;
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

/// Parses the latest `## [x.y.z] - YYYY-MM-DD` release date from `CHANGELOG.md`
/// and returns `(date_str, YYYY-MM-DDT14:00:00Z)`.
fn parse_latest_release_timestamp(changelog: &str) -> anyhow::Result<(String, DateTime<Utc>)> {
    let release_line = changelog
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("## [") && !line.starts_with("## [Unreleased]"))
        .ok_or_else(|| anyhow::anyhow!("No `## [x.y.z] - YYYY-MM-DD` section in CHANGELOG.md"))?;

    let (_, date_part) = release_line.split_once("] - ").ok_or_else(|| {
        anyhow::anyhow!("Malformed release header `{release_line}` in CHANGELOG.md")
    })?;
    let date_str = date_part.trim().to_string();
    let naive_date = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d")
        .with_context(|| format!("Invalid release date `{date_str}` in CHANGELOG.md"))?;
    let ts = naive_date
        .and_hms_opt(14, 0, 0)
        .expect("14:00:00 is a valid time")
        .and_utc();
    Ok((date_str, ts))
}

/// Formats the canonical PNG `Software` stamp for the current package version and release date.
fn expected_software_stamp(date_str: &str) -> String {
    format!("agy-usage v{} ({date_str})", env!("CARGO_PKG_VERSION"))
}

/// Computes ISO 3309 / PNG CRC-32 over `chunk_type || data`.
fn crc32_png(chunk_type: &[u8; 4], data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in chunk_type.iter().chain(data.iter()) {
        crc ^= u32::from(b);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xedb8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Walks the PNG chunks in `png_bytes`, strips any existing `Software` `tEXt` chunk,
/// and inserts a fresh `tEXt` chunk (`Software\0{software_value}`) immediately before `IEND`.
fn inject_png_software_chunk(png_bytes: &[u8], software_value: &str) -> anyhow::Result<Vec<u8>> {
    const PNG_SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    anyhow::ensure!(
        png_bytes.starts_with(PNG_SIG),
        "Input is not a valid PNG file (missing 8-byte PNG signature)"
    );

    let mut out = Vec::with_capacity(png_bytes.len() + 64);
    out.extend_from_slice(PNG_SIG);

    let mut pos = PNG_SIG.len();
    let mut saw_iend = false;

    while pos < png_bytes.len() {
        anyhow::ensure!(
            pos + 12 <= png_bytes.len(),
            "Truncated PNG chunk header at byte offset {pos}"
        );
        let len = u32::from_be_bytes(png_bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let chunk_type: [u8; 4] = png_bytes[pos + 4..pos + 8].try_into().unwrap();
        let chunk_end = pos
            .checked_add(12)
            .and_then(|n| n.checked_add(len))
            .ok_or_else(|| anyhow::anyhow!("PNG chunk length overflow at offset {pos}"))?;
        anyhow::ensure!(
            chunk_end <= png_bytes.len(),
            "Truncated PNG chunk `{}` at byte offset {pos}",
            String::from_utf8_lossy(&chunk_type)
        );
        let data = &png_bytes[pos + 8..pos + 8 + len];

        if &chunk_type == b"tEXt" && data.starts_with(b"Software\0") {
            // Skip any existing `Software` tEXt chunk so stamping is idempotent.
            pos = chunk_end;
            continue;
        }

        if &chunk_type == b"IEND" {
            anyhow::ensure!(len == 0, "PNG IEND chunk must have length 0, got {len}");
            anyhow::ensure!(
                chunk_end == png_bytes.len(),
                "Unexpected trailing bytes after PNG IEND chunk"
            );

            let payload = format!("Software\0{software_value}");
            let payload_bytes = payload.as_bytes();
            let text_type = b"tEXt";
            let crc = crc32_png(text_type, payload_bytes);

            out.extend_from_slice(&(payload_bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(text_type);
            out.extend_from_slice(payload_bytes);
            out.extend_from_slice(&crc.to_be_bytes());

            out.extend_from_slice(&png_bytes[pos..chunk_end]);
            saw_iend = true;
            break;
        }

        out.extend_from_slice(&png_bytes[pos..chunk_end]);
        pos = chunk_end;
    }

    anyhow::ensure!(saw_iend, "PNG file is missing terminal IEND chunk");
    Ok(out)
}

/// Reads a PNG file from `path`, injects the `Software` metadata stamp, and writes it back.
fn stamp_png_file(path: &Path, software_value: &str) -> anyhow::Result<()> {
    let raw = std::fs::read(path)
        .with_context(|| format!("Failed to read screenshot {}", path.display()))?;
    let stamped = inject_png_software_chunk(&raw, software_value)
        .with_context(|| format!("Failed to stamp screenshot {}", path.display()))?;
    std::fs::write(path, stamped)
        .with_context(|| format!("Failed to write stamped screenshot {}", path.display()))?;
    Ok(())
}

fn render_tui_buffer(
    reports: &[BucketReport],
    now: DateTime<Utc>,
    width: u16,
    height: u16,
    clock: ClockFormat,
) -> Buffer {
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
    terminal.backend().buffer().clone()
}

fn buffer_to_html(buffer: &Buffer) -> String {
    let width = buffer.area.width;
    let height = buffer.area.height;

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

            let style_attr = if styles.is_empty() {
                String::new()
            } else {
                format!(";{}", styles.join(";"))
            };

            if sym == "⚠" && x + 1 < width && buffer[(x + 1, y)].symbol() == " " {
                // `Status::Tight` uses `"⚠  tight"` (two spaces) because `U+26A0` has
                // unicode-width 1 whereas `✅` and `❌` have width 2. Rendering `⚠\u{fe0f}`
                // across the 2-cell (18px) span selects emoji presentation (`Noto Color Emoji`)
                // at the exact same 2-column width as `✅` and `❌` while keeping `"tight"` aligned.
                rows_html.push_str(&format!(
                    "<span class=\"cell\" style=\"width:18px{style_attr}\">⚠\u{fe0f}</span>"
                ));
                x += 2;
                continue;
            }

            let cell_width_px = if w > 1 { w * 9 } else { 9 };

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

fn render_tui_to_html(
    reports: &[BucketReport],
    now: DateTime<Utc>,
    width: u16,
    height: u16,
    clock: ClockFormat,
) -> String {
    let buffer = render_tui_buffer(reports, now, width, height, clock);
    buffer_to_html(&buffer)
}

fn main() -> anyhow::Result<()> {
    // SAFETY: called at the very start of `main` before any threads are spawned,
    // ensuring `chrono::Local` formats TUI timestamps deterministically in UTC.
    unsafe {
        std::env::set_var("TZ", "UTC");
    }

    let chrome_bin = resolve_chrome_bin()?;
    println!("Using Chrome/Chromium binary: {chrome_bin}");

    let changelog = include_str!("../CHANGELOG.md");
    let (release_date, now) = parse_latest_release_timestamp(changelog)?;
    let software_stamp = expected_software_stamp(&release_date);
    println!("Generating release screenshots for {software_stamp} (now = {now})");

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
        .replacen(
            "const data = ",
            &format!(
                "Date.now = () => {};\n        const data = ",
                now.timestamp_millis()
            ),
            1,
        )
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
        html_content.contains("Date.now = () =>"),
        "Failed to inject Date.now override into dashboard HTML"
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
        .env("TZ", "UTC")
        .env("LANG", "en_US.UTF-8")
        .env("LC_ALL", "en_US.UTF-8")
        .args([
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            "--lang=en-US",
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
    stamp_png_file(&dashboard_png, &software_stamp)?;
    println!("Captured and stamped {}", dashboard_png.display());

    // 2. Render and capture TUI dashboard
    let tui_html = render_tui_to_html(&reports, now, 122, 27, ClockFormat::H24);
    let tui_html_path = target_dir.join("demo_tui.html");
    std::fs::write(&tui_html_path, tui_html)?;

    let tui_png = screenshots_dir.join("tui.png");
    println!("Capturing TUI screenshot...");
    let chrome_tui = Command::new(&chrome_bin)
        .env("TZ", "UTC")
        .env("LANG", "en_US.UTF-8")
        .env("LC_ALL", "en_US.UTF-8")
        .args([
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            "--lang=en-US",
            "--window-size=1280,780",
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
    stamp_png_file(&tui_png, &software_stamp)?;
    println!("Captured and stamped {}", tui_png.display());

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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parses_latest_release_date_from_changelog() {
        let changelog = include_str!("../CHANGELOG.md");
        let (date_str, ts) = parse_latest_release_timestamp(changelog).unwrap();
        assert_eq!(date_str.len(), 10);
        assert_eq!(&date_str[4..5], "-");
        assert_eq!(&date_str[7..8], "-");
        assert_eq!(
            ts.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            format!("{date_str}T14:00:00Z")
        );
        assert!(
            expected_software_stamp(&date_str).starts_with("agy-usage v"),
            "software stamp must start with agy-usage v"
        );
    }

    #[test]
    fn injects_valid_png_text_chunk_before_iend_idempotently() {
        assert_eq!(crc32_png(b"IEND", b""), 0xae42_6082);
        assert_eq!(crc32_png(b"1234", b"56789"), 0xcbf4_3926);

        let minimal_png = [
            b"\x89PNG\r\n\x1a\n".as_slice(),
            b"\x00\x00\x00\x00IEND\xae\x42\x60\x82".as_slice(),
        ]
        .concat();

        let stamped_once = inject_png_software_chunk(&minimal_png, "agy-usage v0.3.0 (2026-10-25)")
            .expect("first stamp should succeed");
        assert!(stamped_once.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(stamped_once.ends_with(b"\x00\x00\x00\x00IEND\xae\x42\x60\x82"));

        let stamped_twice =
            inject_png_software_chunk(&stamped_once, "agy-usage v0.3.0 (2026-10-25)")
                .expect("second stamp should succeed idempotently");
        assert_eq!(
            stamped_once, stamped_twice,
            "re-stamping with the same value must be idempotent"
        );
    }

    #[test]
    fn tui_buffer_and_html_render_resets_column_range_hint_and_two_cell_warning_emoji() {
        let now = Utc.with_ymd_and_hms(2026, 10, 25, 14, 0, 0).unwrap();
        let samples = generate_synthetic_samples(now);
        let reports = forecast::analyze(&samples, 0.10);
        let buffer = render_tui_buffer(&reports, now, 122, 27, ClockFormat::H24);

        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }

        assert!(
            text.contains("Resets"),
            "expected Resets column in TUI buffer"
        );
        assert!(
            text.contains("resets in 5d"),
            "expected pane 1 reset suffix"
        );
        assert!(
            text.contains("resets in 2h 45m"),
            "expected pane 2 reset suffix"
        );
        assert!(
            text.contains("resets in 4d"),
            "expected pane 3 reset suffix"
        );
        assert!(
            text.contains("resets in 3h 15m"),
            "expected pane 4 reset suffix"
        );
        assert!(text.contains("t: Range"), "expected t: Range footer hint");

        let html = buffer_to_html(&buffer);
        assert!(html.contains("✅"), "expected on-track emoji in HTML");
        assert!(html.contains("❌"), "expected will-run-out emoji in HTML");
        assert!(
            html.contains("width:18px") && html.contains("⚠\u{fe0f}"),
            "expected 2-cell 18px emoji presentation for warning symbol in HTML"
        );
    }
}
