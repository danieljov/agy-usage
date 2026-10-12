# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Live Demo on GitHub Pages**: interactive HTML dashboard rendered from the synthetic demo dataset, published at <https://danieljov.github.io/agy-usage> by the `Pages` workflow on every push to `main`.
- Chrome-free demo generator example (`examples/generate_demo.rs`) writing `index.html`; the synthetic dataset is shared with `generate_screenshots` via `examples/common/demo_data.rs`.

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

[0.1.0]: https://github.com/danieljov/agy-usage/releases/tag/v0.1.0
