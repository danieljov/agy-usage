# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Pro Account Quota & Generalized Window Support (`src/model.rs`, `src/forecast.rs`, `src/tui.rs`, `src/html.rs`, `assets/dashboard.html`)**:
  - Added support for `"daily"` (`24h`) and strict positive `<N>h`, `<N>d`, `<N>w` quota window durations up to 7 days (`model::window_length`), with burn-rate lookback computed as `(window_length / 5).clamp(15m, 24h)` (`model::lookback`). Windows longer than 7 days return `None` and trigger the full-history analysis fallback (`forecast::supports_bounded_analysis`).
  - Added single-source-of-truth label disambiguation (`BucketReport::has_collision`, `BucketReport::label`, `BucketReport::group_label`) that appends `({bucket_id})` only when multiple buckets in a report slice share the exact same `(group, window)`.
  - Added a Google AI Pro `/usage` fixture (`tests/fixtures/usage_pro.json`, 3 groups / 5 buckets including `"Gemini 4 Argon"` `gemini-argon-wtu-weekly`) and full-width trailing card layout in 2-column TUI and HTML dashboard grids when the bucket count is odd.

### Changed
- Sorted quota buckets within each group by `model::window_length` from longest to shortest with unknown windows last (`forecast::analyze_owned`).
- Generalized short-window (`< 24h`) boundary decluttering and range-toggle templates across the TUI (`tui::chart_viewport`) and HTML dashboard (`CompactReport.window_secs`).

## [0.4.0] - 2026-10-28

### Added
- **Multi-Platform Release Binaries, macOS `launchd` Timer & MSRV CI (`src/timer.rs`, `src/main.rs`, `install.sh`, `.github/workflows/release.yml`, `.github/workflows/ci.yml`)**:
  - Added macOS `launchd` user agent support (`~/Library/LaunchAgents/com.agy-usage.collect.plist`) in `agy-usage install-timer` / `uninstall-timer` alongside Linux `systemd`, and used `open` on macOS for `agy-usage html --open`.
  - Expanded `.github/workflows/release.yml` and `install.sh` to build and install pre-compiled binaries for Linux (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`) and macOS (`x86_64-apple-darwin`, `aarch64-apple-darwin`).
  - Added an MSRV (`1.88`) verification job in `.github/workflows/ci.yml`.
- **Interactive TUI `?` Help Overlay & HTML Hover Crosshair Sync (`src/tui.rs`, `assets/dashboard.html`, `src/html.rs`, `README.md`)**:
  - Added a toggleable `?` help modal (`Help · Controls & Legend`) in `agy-usage show` displaying keyboard shortcuts, chart series descriptions (`Actual`, `Budget`, `Proj`, `Reset`), and prognosis status definitions (`on track`, `tight`, `in <t>`, `collecting`, `disabled`), with `?: Help` in the status bar (including a compact 80-column fallback that preserves `last sample ...`).
  - Added synchronized hover crosshairs and tooltips across all HTML dashboard charts (`crosshairSyncPlugin` in `assets/dashboard.html`), snapping sibling charts to the nearest visible `Actual` sample within a bounded time tolerance and clearing stale crosshairs on mouseout or time-range toggle.
- **Shell Completions (`src/main.rs`, `Cargo.toml`, `README.md`)**:
  - Added `agy-usage completions <bash|zsh|fish|elvish|powershell>` subcommand powered by `clap_complete` with directory/file `ValueHint` annotations for `--data-dir` and `html --out`.
  - Handled `BrokenPipe` cleanly when piping completions output and skipped data-directory resolution for `completions` and `uninstall-timer` so completion generation works when `$HOME` and `$XDG_DATA_HOME` are unset.
- **Discovery, SEO & Security Metadata (`Cargo.toml`, `SECURITY.md`, `examples/generate_demo.rs`, `.github/workflows/pages.yml`)**:
  - Added `rust-version = "1.88"` (MSRV) to `Cargo.toml` and synchronized `README.md` (`Ensure Rust 1.88+ is installed:`).
  - Added `SECURITY.md` documenting supported versions and GitHub Private Vulnerability Reporting (`https://github.com/danieljov/agy-usage/security/advisories/new`).
  - Enriched the GitHub Pages live demo (`examples/generate_demo.rs`) `<head>` with descriptive `<title>`, `<meta name="description">`, `<link rel="canonical">`, OpenGraph (`og:title`, `og:description`, `og:url`, `og:image`), and `twitter:card` metadata while keeping local `agy-usage html` output 100% offline.

## [0.3.1] - 2026-10-27

### Fixed
- **In-Window Reservation Hold Smoothing (`forecast::analyze`)**:
  - `BucketReport::history` and `BucketReport::current` now lift transient in-window reservation hold dips to each reset window's right-to-left non-increasing upper envelope (`smooth_window_holds`) so refunded backend holds do not render as visual chart jitter, while `forecast::prognose` continues to evaluate raw `current` samples and `remaining_now`.
- **HTML Dashboard Chart Rendering (`assets/dashboard.html`)**:
  - Added rounded line joins and caps (`borderJoinStyle: 'round'`, `borderCapStyle: 'round'`) to `Actual` chart strokes.
  - Filtered `Actual` dataset points to the active viewport range (`pt.x >= x.min`) so Chart.js does not draw an off-screen-to-on-screen vertical refill stroke along the left Y-axis border (`x = window_start`), and simplified `pointRadius` (`pts.length === 1 ? 2 : 0`) so marker dots are only drawn when a chart has a single visible sample.
- **Synthetic Demo Dataset & Release Screenshot Sync (`examples/common/demo_data.rs`, `examples/generate_screenshots.rs`, `tests/cli.rs`)**:
  - Expanded synthetic demo data from 2 hours to 30 days of window-aligned history (`15m`/`5m` intervals) so Weekly and 5h charts start at `100%` at `window_start` with multi-window reset boundaries across all range toggles (`5h`, `24h`, `7d`, `30d`, `All`).
  - Anchored `generate_screenshots` deterministically to the latest `CHANGELOG.md` release date in UTC, fixed 2-cell `⚠️` (`⚠\u{fe0f}`) emoji rendering and 122-column TUI layout, and enforced PNG `Software` metadata stamp synchronization in `release_metadata_and_docs_stay_in_sync`.

## [0.3.0] - 2026-10-25

### Added
- `store::load_since`, `forecast::analysis_cutoff`, `forecast::analyze_since`, and `forecast::supports_bounded_analysis` for analyzing only the samples needed for a history window, and `forecast::analyze_owned` for analyzing samples without cloning them.

### Changed
- **Bounded loading**: `collect`, `html`, and `show --once` keep only the last `--history-days` + 8 days of samples in memory instead of the full retention window (52 MB → 17 MB peak RSS and 0.16 s → 0.10 s for `html` on a 109k-line, 98-day store). Buckets without a complete rate lookback in that range are no longer reported by these commands. If a bucket sampled in that range has a window of unknown length, these commands fall back to full-history analysis.
- `show` (interactive) no longer loads and analyzes the samples file twice on startup, and now reports load errors and corrupt lines in its status bar instead of silently showing no data.
- The interactive TUI repaints only on input, resize, new data, or once per second for relative times, instead of on every 250 ms input poll (idle CPU on the 30d range: ~4.7% → ~0.8%). Chart ranges are sliced from the history by binary search; `tui::chart_viewport` now requires `BucketReport::history` sorted by timestamp, as produced by `forecast::analyze`.
- The HTML dashboard only embeds window boundaries inside `--history-days`, like the chart series.
- The `skipped N corrupt lines` warning of these commands no longer counts corrupt lines older than the loaded range whose timestamp is still readable.
- `store::prune` buffers writes to its temporary file.
- The HTML dashboard template is filled in one pass instead of three full-string replacements; substituted content is never re-scanned for placeholders.

### Fixed
- **Mid-Window Quota Reconciliation Boundary Detection**:
  - `forecast::window_boundaries` now ignores small mid-window backend quota reconciliations (`+0.5%` to `+10%`) when `reset_time` has not advanced (`<= 60s`), preventing false window reset boundaries from truncating `BucketReport::current` and burn-rate lookback while preserving detection of scheduled/early resets (`reset_moved && (reset_expired || quota_increased)`), large refill jumps (`> +10%`), and `disabled` state transitions.

## [0.2.0] - 2026-10-23

### Added
- **CLI `--version` / `-V` Support**:
  - Enabled `version` on the top-level CLI parser so `agy-usage --version` and `agy-usage -V` output `agy-usage <CARGO_PKG_VERSION>`.
- **Next Reset Countdown & Timestamp Display**:
  - Added a responsive `"Resets"` column (`in Xd Yh (HH:MM)` / `in Xh Ym (h:mm AM/PM)`) to the TUI summary table on terminals `>= 110` columns while preserving the 80-column 8-column table layout on narrower terminals.
  - Added Unicode-width-aware reset countdown suffixes (`· resets in Xd Yh (HH:MM)` with fallback to `· resets in Xd Yh`) to TUI chart block titles so 80-column stacked charts and 2-column split panes also surface reset times.
  - Added a `"Resets"` column to the HTML dashboard summary table with 30-second live countdown refreshes in `updateRelativeTimes()`.
  - Plumbed `ClockFormat` into `tui::run()` so interactive TUI and `--once` share consistent 12h/24h formatting.
- **Interactive TUI Time-Range Cycling (`t` / `w`)**:
  - Added `t` and `w` keybindings in `agy-usage show` to cycle chart time ranges (`Window` [default], `24h`, `7d`, `30d`, `All`), matching the web dashboard's range toggle.
  - Plots full bucket history across multi-window ranges and renders vertical `│` reset boundaries while suppressing 5-hour boundary barcode clutter on ranges `> 48h` (`7d`, `30d`, `All`).
- **Automated Crates.io Release Publishing**:
  - Extended `.github/workflows/release.yml` to idempotently publish new tag releases to crates.io via Crates.io Trusted Publishing (OIDC) or `CARGO_REGISTRY_TOKEN` after verification and GitHub Release asset uploads succeed.

### Changed
- **Library API (`agy_usage::tui::run`)**:
  - `tui::run` now accepts a `ClockFormat` parameter (`tui::run(data_dir, tight, agy_bin, clock)`) so interactive and single-frame terminal views share unified clock formatting.

## [0.1.4] - 2026-10-22

### Fixed
- **Zero-Rate Burn Projections in TUI and HTML Dashboard**:
  - Idle quota buckets with a computed `0.00%/h` burn rate (`rate_per_h == Some(0.0)`) now render their flat horizontal projection line to `reset_time` on both the terminal chart and web dashboard instead of omitting the projection dataset.
  - Added a positive-span guard (`end_x > start_x`) so already-exhausted (`0.0%`) buckets with `empty_at == last_sample` do not emit degenerate zero-length projection segments.

### Added
- **Automated Release Metadata & Tag Guardrails**:
  - Added `release_metadata_and_docs_stay_in_sync` integration test verifying `Cargo.toml`, `Cargo.lock`, `README.md`, and `CHANGELOG.md` stay synchronized on every test run.
  - Added tag-version verification (`v${CARGO_PKG_VERSION}`) and locked formatting/Clippy checks to the GitHub Release workflow.

## [0.1.3] - 2026-10-14

### Changed
- **Optimized Window Defaults in HTML Dashboard**:
  - 5-hour quota charts now default to `5h` (matching TUI pacing focus 1:1), displaying the full active window slope, diagonal budget line, and burn-rate projection.
  - Weekly quota charts now default to `7d` (with `30d` and `All` available via toggle).

### Fixed
- **Viewport Clamping Against Clock Drift**:
  - Clamped viewport lower bounds with `Math.min(rawMin, lastSampleMs)` during chart initialization and button clicks, ensuring data points are never clipped out if the client clock lags behind the API server's reset timestamp.
- **Weekly Card Reset Boundary Restoration**:
  - Fixed symmetrical boundary visibility restoration (`boundaries.display = !is5h`) on weekly charts when switching back to `7d` or `30d` after `All`.

## [0.1.2] - 2026-10-14

### Added
- **Window-Aware Defaults & Client-Side Range Toggles in HTML Dashboard**:
  - 5-hour quota charts default to a 24-hour viewport (`now - 20h` to `reset_time`), keeping ~20% of the canvas for the active window to display pacing and burn-rate slopes cleanly. Weekly charts default to 30 days.
  - Interactive pill range toggles on chart headers: `[ 5h | 24h | 7d | 30d ]` on 5-hour charts (where `5h` gives 1:1 parity with the TUI pacing focus), and `[ 7d | 30d | All ]` on weekly charts.
  - Automatic reset boundary line suppression on 5-hour charts when viewing ranges > 48h (`7d`, `30d`), eliminating vertical barcode clutter while keeping sawtooth resets clear.
  - Zero-allocation performance hardening in Chart.js `beforeDraw` boundary drawing loop via pre-parsed epoch timestamps.
  - Full WCAG accessibility support (`role="group"`, `aria-label="Time range"`, `aria-pressed`, `:focus-visible` keyboard focus outline).

## [0.1.1] - 2026-10-13

### Added
- **Live Demo on GitHub Pages**: interactive HTML dashboard rendered from the synthetic demo dataset, published at <https://danieljov.github.io/agy-usage> by the `Pages` workflow on pushes to `main` and scheduled every 6 hours.
- Chrome-free demo generator example (`examples/generate_demo.rs`) writing `index.html`; the synthetic dataset is shared with `generate_screenshots` via `examples/common/demo_data.rs`.

### Changed
- Point package `homepage` in `Cargo.toml` to the live GitHub Pages demo.

### Fixed
- **Disabled Quota Handling When Weekly Limit Reached**:
  - Automatically detect and support disabled quota buckets when a model group exhausts its weekly quota (`0.0%`).
  - Added backward-compatible `disabled: bool` and optional `reset_time` to `model::Sample`.
  - Added `Status::Disabled` (`🚫 disabled`) prognosis state and clear display across TUI and HTML dashboards.

## [0.1.0] - 2026-10-12

Initial release of `agy-usage`: quota history, burn-rate prognosis, and interactive TUI/HTML dashboards for Google Antigravity.

### Added
- **Interactive Terminal TUI (`agy-usage show`)**:
  - Real-time `ratatui` terminal interface with 2-column braille quota history charts.
  - Fullscreen bucket focus mode (`1`–`9`), live sample collection (`r`), and clean return (`Esc`).
  - Single-shot output mode (`--once`) with automatic TTY detection (ANSI colors on terminals, plain text when piped).
- **Self-Contained Web Dashboard (`agy-usage html`)**:
  - Interactive HTML dashboard featuring time-series charts, budget reference lines, and burn-rate projections.
  - Vendored Chart.js and date-fns adapter assets for 100% offline functionality with zero external CDN calls.
  - Browser auto-opening support (`--open`) and customizable output paths (`--out`).
- **Automated Background Collection (`agy-usage collect`, `install-timer`, `uninstall-timer`)**:
  - Direct quota sampling via `agy-usage collect` with automatic HTML dashboard regeneration (unless `--no-html` is specified).
  - One-command systemd user timer installation (`agy-usage install-timer`) creating `agy-usage.timer` and `agy-usage.service` to sample quota every 5 minutes.
  - Complete removal of timers and services via `agy-usage uninstall-timer`.
  - Atomic lazy pruning for data retention (`--retention-days`, default 90 days) with sidecar file locking (`samples.lock`) preventing race conditions during concurrent appends.
  - Automatic propagation of CLI threshold, retention, and clock settings into systemd unit files.
- **Prognosis & Burn-Rate Engine**:
  - Automatic window boundary detection for quota resets and sliding lookback analysis.
  - Real-time status prognoses (`✅ on track`, `⚠ tight`, `❌ will run out`) with time-to-exhaustion calculations.
- **System Clock & Locale Adaptation**:
  - Automatic 12h/24h time format detection based on GNOME desktop configuration and `LC_TIME` locale.
  - Command-line override via `--clock 12h|24h|auto`.
- **Documentation & Visual Previews**:
  - Embedded demo screenshots in `README.md`.
  - Reproducible screenshot generator example (`examples/generate_screenshots.rs`) supporting headless Chrome/Chromium capture with vector-rendered borders.
- **Pre-compiled Binary Releases & Installer**:
  - Standalone, stripped Linux x86_64 binary archives with SHA-256 checksums published via automated GitHub Release workflow.
  - Automated one-line curl installation script (`install.sh`) with automated SHA-256 checksum verification.

[0.4.0]: https://github.com/danieljov/agy-usage/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/danieljov/agy-usage/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/danieljov/agy-usage/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/danieljov/agy-usage/compare/v0.1.4...v0.2.0
[0.1.4]: https://github.com/danieljov/agy-usage/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/danieljov/agy-usage/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/danieljov/agy-usage/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/danieljov/agy-usage/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/danieljov/agy-usage/releases/tag/v0.1.0
