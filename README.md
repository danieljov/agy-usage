# agy-usage

[![CI](https://github.com/danieljov/agy-usage/actions/workflows/ci.yml/badge.svg)](https://github.com/danieljov/agy-usage/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/agy-usage.svg)](https://crates.io/crates/agy-usage)
[![Live Demo](https://img.shields.io/badge/demo-live-brightgreen?logo=github)](https://danieljov.github.io/agy-usage)

Quota history, burn-rate prognosis, and interactive TUI/HTML dashboards for Google Antigravity.

`agy-usage` samples your Antigravity quota periodically (every 5 minutes via a systemd user timer), computes burn rates and quota window projections, and displays the status in both a `ratatui` terminal interface and a self-contained HTML dashboard.

**🔗 [Live Demo](https://danieljov.github.io/agy-usage)** — try the interactive HTML dashboard in your browser (synthetic data).

## Screenshots

### Interactive Terminal Dashboard (`agy-usage show`)

![Interactive Terminal TUI Dashboard](https://raw.githubusercontent.com/danieljov/agy-usage/main/assets/screenshots/tui.png)

### Self-Contained HTML Dashboard (`agy-usage html --open`)

[![HTML Web Dashboard](https://raw.githubusercontent.com/danieljov/agy-usage/main/assets/screenshots/dashboard.png)](https://danieljov.github.io/agy-usage)

Features **window-aware defaults** (5h charts default to 5h, weekly to 7d) and **client-side range toggles** (`5h`, `24h`, `7d`, `30d`, `All`) with automatic reset line decluttering past 48h and 100% offline vendored assets.

The [live demo](https://danieljov.github.io/agy-usage) is rendered from synthetic data and rebuilt every 6 hours (as well as on pushes to `main`) via GitHub Actions (`cargo run --example generate_demo`). Relative timestamps and exhaustion prognoses reflect the state at the last deployment.

## Installation

### Quick Install (Linux x86_64)

Install the latest release binary into `~/.local/bin` with automated checksum verification:

```bash
curl -fsSL https://raw.githubusercontent.com/danieljov/agy-usage/main/install.sh | bash
```

### Manual Binary Download (Linux x86_64)

Pre-compiled standalone binaries for Linux (`x86_64-unknown-linux-gnu`) are published on [GitHub Releases](https://github.com/danieljov/agy-usage/releases/latest).

#### Using `curl`
```bash
TMP_DIR=$(mktemp -d)
curl -fsSL https://github.com/danieljov/agy-usage/releases/download/v0.2.0/agy-usage-v0.2.0-x86_64-unknown-linux-gnu.tar.gz | tar -xz -C "$TMP_DIR"
mkdir -p ~/.local/bin
mv "$TMP_DIR"/agy-usage-*/agy-usage ~/.local/bin/
rm -rf "$TMP_DIR"
```

#### Using GitHub CLI (`gh`)
```bash
TMP_DIR=$(mktemp -d)
gh release download --repo danieljov/agy-usage --pattern "*.tar.gz" --dir "$TMP_DIR"
tar -xzf "$TMP_DIR"/*.tar.gz -C "$TMP_DIR"
mkdir -p ~/.local/bin
mv "$TMP_DIR"/agy-usage-*/agy-usage ~/.local/bin/
rm -rf "$TMP_DIR"
```

Ensure `~/.local/bin` is in your `PATH` (e.g., `export PATH="$HOME/.local/bin:$PATH"` in `~/.bashrc` or `~/.zshrc`). Verify the installation:
```bash
agy-usage --help
```

### From Source (Cargo)

Ensure Rust 1.99+ is installed:

```bash
# From crates.io
cargo install agy-usage

# From git repository
cargo install --git https://github.com/danieljov/agy-usage.git

# Or build and install from a local clone
cargo install --path .
```

## Quick Start

1. **Install automated sampling timer:**
   ```bash
   agy-usage install-timer
   ```
   This sets up a systemd user timer running every 5 minutes (`~/.config/systemd/user/agy-usage.{service,timer}`).

2. **Sample quota immediately:**
   ```bash
   agy-usage collect
   ```

3. **View in interactive terminal dashboard:**
   ```bash
   agy-usage show
   ```
   Or print a single frame (falls back to plain text when redirected or non-TTY):
   ```bash
   agy-usage show --once
   ```

4. **View HTML dashboard in your browser:**
   ```bash
   agy-usage html --open
   ```

## TUI Controls

- `r`: Collect fresh samples now and refresh
- `t` (or `w`): Cycle chart time range (`Window` → `24h` → `7d` → `30d` → `All`)
- `1`–`9`: Focus a single quota bucket full-screen
- `Esc`: Return to 2-column grid view
- `q` or `Ctrl-C`: Quit

## CLI Reference

```text
Usage: agy-usage [OPTIONS] <COMMAND>

Commands:
  collect          Collect quota samples from agy and update dashboard
  show             Show terminal dashboard
  html             Generate HTML dashboard
  install-timer    Install systemd user timer for automated collection
  uninstall-timer  Uninstall systemd user timer
  help             Print this message or the help of the given subcommand(s)

Options:
      --data-dir <DATA_DIR>                Data directory [default: $XDG_DATA_HOME/agy-usage or ~/.local/share/agy-usage]
      --tight-threshold <TIGHT_THRESHOLD>  Tight threshold fraction [default: 0.10]
      --history-days <HISTORY_DAYS>        Days of history to include in HTML dashboard [default: 30]
      --retention-days <RETENTION_DAYS>    Days of raw samples to keep on disk [default: max(90, --history-days)]
      --clock <CLOCK>                      Clock format for displayed times [default: auto] [possible values: auto, 12h, 24h]
  -h, --help                               Print help
  -V, --version                            Print version
```

## Time Format

Displayed times (TUI header, HTML dashboard header, table, chart axes and tooltips) use your local time zone and your system's 12h/24h clock setting. With `--clock auto` (the default) the format is taken from the GNOME clock setting (`org.gnome.desktop.interface clock-format`), falling back to the `LC_TIME` locale and then 24h. Pass `--clock 12h` or `--clock 24h` to override.

> **Note:** On non-GNOME desktops (KDE, XFCE, …) that still have the GNOME schemas installed, `gsettings` reports the schema default (`24h`) and the locale is not consulted. Use `--clock 12h` there if needed.

## Data Retention & Pruning

- **Retention Window:** By default, raw 5-minute samples are retained on disk for `max(90, --history-days)` days. You can customize this with `--retention-days <N>` (minimum 8 days; must be $\ge$ `--history-days`).
- **Bounded Loading:** `collect`, `html`, and `show --once` keep only samples from the last `--history-days` plus an 8-day margin (one weekly window and a day of slack) in memory, so their memory use and analysis cost do not grow with `--retention-days` (older lines are still read, but only their timestamp is parsed). Buckets without a complete rate lookback inside that range (24h for weekly, 1h for 5h buckets) are left out. If a bucket sampled in that range reports a window of unknown length, the full history is analyzed instead; buckets with no samples in that range are omitted whatever their window. The interactive `show` TUI always loads the full retained history for its `All` range, so it can still list long-idle buckets that these commands omit.
- **Atomic Lazy Pruning:** During collection, `agy-usage` checks if the oldest sample exceeds the retention threshold plus a 1-day buffer, performing at most one atomic rewrite per day. File mutations and rewrites coordinate via a sidecar lock (`samples.lock`) to ensure zero lost writes during concurrent appends.
- **Offline HTML:** Generated HTML dashboards vendor Chart.js and date adapters locally with zero external network requests.
- **Timer Propagation:** Re-running `agy-usage --retention-days <N> install-timer` automatically propagates configured threshold, history, and retention settings into the systemd user service unit. An explicit `--clock 12h|24h` is propagated too; `auto` is left out so the timer keeps following the system setting.

## Releasing & Crates.io Publishing

Pushing a version tag (`v*` matching `Cargo.toml`) triggers `.github/workflows/release.yml`, which verifies formatting, lints, tests, and tag-version alignment, uploads the Linux `x86_64` binary tarball to GitHub Releases, and idempotently publishes the crate to crates.io.

Authentication with crates.io supports two setups:
1. **Crates.io Trusted Publishing (Recommended):** Configure `danieljov/agy-usage` (workflow `release.yml`) as a Trusted Publisher in your crate settings on crates.io (uses GitHub OIDC `id-token: write` via `rust-lang/crates-io-auth-action@v1`).
2. **Repository Secret Fallback:** Add a crates.io API token as `CARGO_REGISTRY_TOKEN` under GitHub repository `Settings → Secrets and variables → Actions`.

## License

MIT

