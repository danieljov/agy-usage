# agy-usage

Quota history, burn-rate prognosis, and interactive TUI/HTML dashboards for Google Antigravity.

`agy-usage` samples your Antigravity quota periodically (every 5 minutes via a systemd user timer), computes burn rates and quota window projections, and displays the status in both a `ratatui` terminal interface and a self-contained HTML dashboard.

## Installation

Ensure Rust 1.99+ is installed, then build and install locally:

```bash
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
```

## Time Format

Displayed times (TUI header, HTML dashboard header, table, chart axes and tooltips) use your local time zone and your system's 12h/24h clock setting. With `--clock auto` (the default) the format is taken from the GNOME clock setting (`org.gnome.desktop.interface clock-format`), falling back to the `LC_TIME` locale and then 24h. Pass `--clock 12h` or `--clock 24h` to override.

> **Note:** On non-GNOME desktops (KDE, XFCE, …) that still have the GNOME schemas installed, `gsettings` reports the schema default (`24h`) and the locale is not consulted. Use `--clock 12h` there if needed.

## Data Retention & Pruning

- **Retention Window:** By default, raw 5-minute samples are retained on disk for `max(90, --history-days)` days. You can customize this with `--retention-days <N>` (minimum 8 days; must be $\ge$ `--history-days`).
- **Atomic Lazy Pruning:** During collection, `agy-usage` checks if the oldest sample exceeds the retention threshold plus a 1-day buffer, performing at most one atomic rewrite per day. File mutations and rewrites coordinate via a sidecar lock (`samples.lock`) to ensure zero lost writes during concurrent appends.
- **Offline HTML:** Generated HTML dashboards vendor Chart.js and date adapters locally with zero external network requests.
- **Timer Propagation:** Re-running `agy-usage --retention-days <N> install-timer` automatically propagates configured threshold, history, and retention settings into the systemd user service unit. An explicit `--clock 12h|24h` is propagated too; `auto` is left out so the timer keeps following the system setting.

## License

MIT

