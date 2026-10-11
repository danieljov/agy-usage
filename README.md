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
  -h, --help                               Print help
```

## License

MIT
