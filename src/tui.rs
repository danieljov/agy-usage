use std::io::IsTerminal;
use std::path::Path;
use std::time::{Duration as StdDuration, Instant};

use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Borders, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table,
};

use crate::clock::ClockFormat;
use crate::forecast::{BucketReport, Status};
use crate::model::Sample;

#[derive(Default, Debug, Clone)]
pub struct ViewState {
    pub focus: Option<usize>,
    pub message: Option<String>,
    pub is_once: bool,
    pub clock: ClockFormat,
}

fn format_window(win: &str) -> std::borrow::Cow<'_, str> {
    match win {
        "weekly" => "Weekly".into(),
        "5h" => "5h".into(),
        other => other.into(),
    }
}

fn format_time_ago(dt: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let diff = (now - dt).num_seconds();
    if diff < 0 {
        return "in the future".to_string();
    }
    if diff < 60 {
        format!("{}s ago", diff)
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

fn format_time_ago_compact(dt: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let diff = (now - dt).num_seconds();
    if diff < 0 {
        return "future".to_string();
    }
    if diff < 60 {
        format!("{}s", diff)
    } else if diff < 3600 {
        format!("{}m", diff / 60)
    } else if diff < 86400 {
        format!("{}h", diff / 3600)
    } else {
        format!("{}d", diff / 86400)
    }
}

pub fn draw(frame: &mut Frame, reports: &[BucketReport], state: &ViewState, now: DateTime<Utc>) {
    let area = frame.area();
    if area.width < 2 || area.height < 2 {
        return;
    }

    if reports.is_empty() {
        let empty_msg = Paragraph::new(
            "No samples yet — run \"agy-usage install-timer\" or \"agy-usage collect\".",
        )
        .block(Block::default().borders(Borders::ALL).title("agy-usage"));
        frame.render_widget(empty_msg, area);
        return;
    }

    if let Some(focus_idx) = state.focus.filter(|&idx| idx < reports.len()) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(4.min(area.height)),
                Constraint::Length(1),
            ])
            .split(area);

        draw_chart(frame, &reports[focus_idx], chunks[0], Some(focus_idx));
        draw_table(
            frame,
            &reports[focus_idx..focus_idx + 1],
            chunks[1],
            now,
            Some(focus_idx),
        );
        draw_status_bar(frame, reports, state, chunks[2], now);
        return;
    }

    // Grid layout
    let table_height = (reports.len() as u16 + 3).min(area.height.saturating_sub(4));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(table_height),
            Constraint::Length(1),
        ])
        .split(area);

    let top_area = chunks[0];
    let cols = if top_area.width >= 100 { 2 } else { 1 };
    let num_rows = reports.len().div_ceil(cols);

    let row_constraints = vec![Constraint::Ratio(1, num_rows.max(1) as u32); num_rows];
    let row_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(top_area);

    for (r_idx, r_chunk) in row_chunks.iter().enumerate() {
        let col_constraints = vec![Constraint::Ratio(1, cols as u32); cols];
        let col_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints)
            .split(*r_chunk);

        for (c_idx, c_chunk) in col_chunks.iter().enumerate() {
            let report_idx = r_idx * cols + c_idx;
            if report_idx < reports.len() {
                draw_chart(frame, &reports[report_idx], *c_chunk, Some(report_idx));
            }
        }
    }

    draw_table(frame, reports, chunks[1], now, None);
    draw_status_bar(frame, reports, state, chunks[2], now);
}

fn draw_chart(frame: &mut Frame, rep: &BucketReport, area: Rect, index: Option<usize>) {
    if area.width < 4 || area.height < 3 {
        return;
    }

    let title = if rep.prognosis.status == Status::Disabled {
        if let Some(idx) = index {
            format!(
                "[{}] {} · {}  disabled",
                idx + 1,
                rep.group,
                format_window(&rep.window),
            )
        } else {
            format!("{} · {}  disabled", rep.group, format_window(&rep.window),)
        }
    } else if let Some(idx) = index {
        format!(
            "[{}] {} · {}  {:.0}% left",
            idx + 1,
            rep.group,
            format_window(&rep.window),
            (rep.prognosis.remaining_now * 100.0).round()
        )
    } else {
        format!(
            "{} · {}  {:.0}% left",
            rep.group,
            format_window(&rep.window),
            (rep.prognosis.remaining_now * 100.0).round()
        )
    };

    if rep.prognosis.status == Status::Disabled {
        let inner_height = area.height.saturating_sub(2) as usize;
        let pad_top = if inner_height > 2 {
            (inner_height - 1) / 2
        } else {
            0
        };
        let mut msg = "\n".repeat(pad_top);
        msg.push_str("Disabled (weekly limit reached)");

        let text = Paragraph::new(msg)
            .alignment(ratatui::layout::Alignment::Center)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(text, area);
        return;
    }

    let reset_time = match rep.prognosis.reset_time {
        Some(rt) => rt,
        None => return,
    };

    let actual_pts: Vec<(f64, f64)> = rep
        .current
        .iter()
        .map(|s| (s.ts.timestamp() as f64, s.remaining * 100.0))
        .collect();

    let budget_pts: Vec<(f64, f64)> = if let Some(ws) = rep.window_start {
        vec![
            (ws.timestamp() as f64, 100.0),
            (reset_time.timestamp() as f64, 0.0),
        ]
    } else {
        Vec::new()
    };

    let mut proj_pts: Vec<(f64, f64)> = Vec::new();
    if let Some(_rate) = rep.prognosis.rate_per_h.filter(|&r| r > 0.0) {
        let start_x = rep.prognosis.last_sample.timestamp() as f64;
        let start_y = rep.prognosis.remaining_now * 100.0;
        let end_x = rep.prognosis.empty_at.unwrap_or(reset_time).timestamp() as f64;
        let end_y = if rep.prognosis.empty_at.is_some() {
            0.0
        } else {
            rep.prognosis.at_reset.unwrap_or(0.0) * 100.0
        };

        for i in 0..=30 {
            let t = i as f64 / 30.0;
            let x = start_x + t * (end_x - start_x);
            let y = start_y + t * (end_y - start_y);
            proj_pts.push((x, y));
        }
    }

    let reset_pts = vec![
        (reset_time.timestamp() as f64, 0.0),
        (reset_time.timestamp() as f64, 100.0),
    ];

    let mut x_min = rep
        .window_start
        .map(|ws| ws.timestamp() as f64)
        .unwrap_or_else(|| {
            rep.history
                .first()
                .map(|s| s.ts.timestamp() as f64)
                .unwrap_or(0.0)
        });
    let mut x_max = reset_time.timestamp() as f64;
    if x_min >= x_max {
        x_min = x_max - 3600.0;
        x_max += 1.0;
    }

    let x_axis = Axis::default()
        .bounds([x_min, x_max])
        .style(Style::default().fg(Color::DarkGray));
    let y_axis = Axis::default()
        .bounds([0.0, 100.0])
        .style(Style::default().fg(Color::DarkGray));

    let mut datasets = Vec::new();
    if !budget_pts.is_empty() {
        datasets.push(
            Dataset::default()
                .name("Budget")
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::DarkGray))
                .data(&budget_pts),
        );
    }
    if !actual_pts.is_empty() {
        datasets.push(
            Dataset::default()
                .name("Actual")
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Cyan))
                .data(&actual_pts),
        );
    }
    if !proj_pts.is_empty() {
        let proj_color = match rep.prognosis.status {
            Status::WillRunOut => Color::Red,
            Status::Tight => Color::Yellow,
            _ => Color::Green,
        };
        datasets.push(
            Dataset::default()
                .name("Projection")
                .marker(Marker::Braille)
                .graph_type(GraphType::Scatter)
                .style(Style::default().fg(proj_color))
                .data(&proj_pts),
        );
    }
    datasets.push(
        Dataset::default()
            .name("Reset")
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::White))
            .data(&reset_pts),
    );

    let chart = Chart::new(datasets)
        .block(Block::default().borders(Borders::ALL).title(title))
        .x_axis(x_axis)
        .y_axis(y_axis);

    frame.render_widget(chart, area);
}

fn draw_table(
    frame: &mut Frame,
    reports: &[BucketReport],
    area: Rect,
    now: DateTime<Utc>,
    focus_idx: Option<usize>,
) {
    if area.width < 4 || area.height < 2 {
        return;
    }

    let header = Row::new(vec![
        Cell::from("Group"),
        Cell::from("Window"),
        Cell::from("Left"),
        Cell::from("Rate/h"),
        Cell::from("Budget/h"),
        Cell::from("Reset"),
        Cell::from("Status"),
        Cell::from("Sample"),
    ])
    .style(
        Style::default()
            .add_modifier(Modifier::BOLD)
            .fg(Color::Cyan),
    );

    let rows: Vec<Row> = reports
        .iter()
        .enumerate()
        .map(|(i, rep)| {
            let idx = focus_idx.unwrap_or(i);
            let p = &rep.prognosis;
            let left_str = if p.status == Status::Disabled {
                "—".into()
            } else {
                format!("{:.1}%", p.remaining_now * 100.0)
            };
            let rate_str = p
                .rate_per_h
                .map(|r| format!("{:.2}%", r * 100.0))
                .unwrap_or_else(|| "—".into());
            let budget_str = p
                .safe_rate_per_h
                .map(|r| format!("{:.2}%", r * 100.0))
                .unwrap_or_else(|| "—".into());
            let reset_str = p
                .at_reset
                .map(|r| format!("{:.1}%", r * 100.0))
                .unwrap_or_else(|| "—".into());
            let status_str = p.status_label(now);
            let age_str = format_time_ago_compact(p.last_sample, now);

            let group_cell = if idx < 9 {
                format!("[{}] {}", idx + 1, rep.group)
            } else {
                rep.group.clone()
            };

            Row::new(vec![
                Cell::from(group_cell),
                Cell::from(format_window(&rep.window)),
                Cell::from(left_str),
                Cell::from(rate_str),
                Cell::from(budget_str),
                Cell::from(reset_str),
                Cell::from(status_str),
                Cell::from(age_str),
            ])
        })
        .collect();

    let widths = [
        Constraint::Min(18),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(6),
        Constraint::Length(15),
        Constraint::Length(6),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Prognosis"));

    frame.render_widget(table, area);
}

fn draw_status_bar(
    frame: &mut Frame,
    reports: &[BucketReport],
    state: &ViewState,
    area: Rect,
    now: DateTime<Utc>,
) {
    if area.width < 4 || area.height < 1 {
        return;
    }

    let left_text = if state.is_once {
        format!(
            "agy-usage · {}",
            state.clock.format_datetime(&now.with_timezone(&Local))
        )
    } else if let Some(msg) = &state.message {
        format!("msg: {} | Esc: Grid  1-9: Focus  r: Refresh  q: Quit", msg)
    } else {
        "Esc: Grid  1-9: Focus  r: Refresh  q: Quit".to_string()
    };

    let latest_sample = reports.iter().map(|r| r.prognosis.last_sample).max();
    let (right_text, is_stale) = if let Some(ls) = latest_sample {
        let age_secs = (now - ls).num_seconds();
        (
            format!("last sample {}", format_time_ago(ls, now)),
            age_secs > 15 * 60,
        )
    } else {
        ("".to_string(), false)
    };

    let right_color = if is_stale {
        Color::Red
    } else {
        Color::DarkGray
    };

    let left_len = left_text.len();
    let right_len = right_text.len();
    let total_width = area.width as usize;

    let line = if left_len + right_len + 2 <= total_width {
        let pad = total_width.saturating_sub(left_len + right_len);
        Line::from(vec![
            Span::styled(left_text, Style::default().fg(Color::Gray)),
            Span::raw(" ".repeat(pad)),
            Span::styled(right_text, Style::default().fg(right_color)),
        ])
    } else {
        Line::from(vec![Span::styled(
            left_text,
            Style::default().fg(Color::Gray),
        )])
    };

    frame.render_widget(Paragraph::new(line), area);
}

fn visible_cells(
    buffer: &ratatui::buffer::Buffer,
    y: u16,
    width: u16,
) -> impl Iterator<Item = &ratatui::buffer::Cell> {
    let mut skip = 0;
    (0..width).filter_map(move |x| {
        if skip > 0 {
            skip -= 1;
            return None;
        }
        let cell = &buffer[(x, y)];
        let sym = cell.symbol();
        let w = unicode_width::UnicodeWidthStr::width(sym);
        if w > 1 {
            skip = w - 1;
        }
        Some(cell)
    })
}

pub fn render_plain(
    reports: &[BucketReport],
    now: DateTime<Utc>,
    width: u16,
    height: u16,
    clock: ClockFormat,
) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let state = ViewState {
        is_once: true,
        clock,
        ..Default::default()
    };
    terminal
        .draw(|f| draw(f, reports, &state, now))
        .expect("draw failed");
    let buffer = terminal.backend().buffer();
    let mut lines = Vec::with_capacity(height as usize);
    for y in 0..height {
        let mut line = String::with_capacity(width as usize);
        for cell in visible_cells(buffer, y, width) {
            line.push_str(cell.symbol());
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

fn push_fg_color(out: &mut String, color: Color) {
    match color {
        Color::Reset => out.push_str("\x1b[39m"),
        Color::Black => out.push_str("\x1b[30m"),
        Color::Red => out.push_str("\x1b[31m"),
        Color::Green => out.push_str("\x1b[32m"),
        Color::Yellow => out.push_str("\x1b[33m"),
        Color::Blue => out.push_str("\x1b[34m"),
        Color::Magenta => out.push_str("\x1b[35m"),
        Color::Cyan => out.push_str("\x1b[36m"),
        Color::Gray => out.push_str("\x1b[37m"),
        Color::DarkGray => out.push_str("\x1b[90m"),
        Color::LightRed => out.push_str("\x1b[91m"),
        Color::LightGreen => out.push_str("\x1b[92m"),
        Color::LightYellow => out.push_str("\x1b[93m"),
        Color::LightBlue => out.push_str("\x1b[94m"),
        Color::LightMagenta => out.push_str("\x1b[95m"),
        Color::LightCyan => out.push_str("\x1b[96m"),
        Color::White => out.push_str("\x1b[97m"),
        Color::Indexed(i) => out.push_str(&format!("\x1b[38;5;{i}m")),
        Color::Rgb(r, g, b) => out.push_str(&format!("\x1b[38;2;{r};{g};{b}m")),
    }
}

fn push_bg_color(out: &mut String, color: Color) {
    match color {
        Color::Reset => out.push_str("\x1b[49m"),
        Color::Black => out.push_str("\x1b[40m"),
        Color::Red => out.push_str("\x1b[41m"),
        Color::Green => out.push_str("\x1b[42m"),
        Color::Yellow => out.push_str("\x1b[43m"),
        Color::Blue => out.push_str("\x1b[44m"),
        Color::Magenta => out.push_str("\x1b[45m"),
        Color::Cyan => out.push_str("\x1b[46m"),
        Color::Gray => out.push_str("\x1b[47m"),
        Color::DarkGray => out.push_str("\x1b[100m"),
        Color::LightRed => out.push_str("\x1b[101m"),
        Color::LightGreen => out.push_str("\x1b[102m"),
        Color::LightYellow => out.push_str("\x1b[103m"),
        Color::LightBlue => out.push_str("\x1b[104m"),
        Color::LightMagenta => out.push_str("\x1b[105m"),
        Color::LightCyan => out.push_str("\x1b[106m"),
        Color::White => out.push_str("\x1b[107m"),
        Color::Indexed(i) => out.push_str(&format!("\x1b[48;5;{i}m")),
        Color::Rgb(r, g, b) => out.push_str(&format!("\x1b[48;2;{r};{g};{b}m")),
    }
}

pub fn render_ansi(
    reports: &[BucketReport],
    now: DateTime<Utc>,
    width: u16,
    height: u16,
    clock: ClockFormat,
) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let state = ViewState {
        is_once: true,
        clock,
        ..Default::default()
    };
    terminal
        .draw(|f| draw(f, reports, &state, now))
        .expect("draw failed");
    let buffer = terminal.backend().buffer();

    let mut out = String::new();
    for y in 0..height {
        let mut current_fg = Color::Reset;
        let mut current_bg = Color::Reset;
        let mut current_bold = false;
        let mut line_buf = String::new();

        for cell in visible_cells(buffer, y, width) {
            let is_bold = cell.modifier.contains(Modifier::BOLD);

            if is_bold != current_bold {
                current_bold = is_bold;
                if is_bold {
                    line_buf.push_str("\x1b[1m");
                } else {
                    line_buf.push_str("\x1b[22m");
                }
            }

            if cell.fg != current_fg {
                current_fg = cell.fg;
                push_fg_color(&mut line_buf, current_fg);
            }

            if cell.bg != current_bg {
                current_bg = cell.bg;
                push_bg_color(&mut line_buf, current_bg);
            }

            line_buf.push_str(cell.symbol());
        }

        if current_bold || current_fg != Color::Reset || current_bg != Color::Reset {
            line_buf.push_str("\x1b[0m");
        }

        out.push_str(line_buf.trim_end());
        out.push('\n');
    }
    out
}

pub fn print_once(reports: &[BucketReport], now: DateTime<Utc>, clock: ClockFormat) -> Result<()> {
    let (width, height) = ratatui::crossterm::terminal::size().unwrap_or((120, 40));
    let needed = if reports.is_empty() {
        5
    } else {
        let cols = if width >= 100 { 2 } else { 1 };
        let num_rows = reports.len().div_ceil(cols) as u16;
        let table_h = reports.len() as u16 + 3;
        (num_rows * 9) + table_h + 1
    };
    let h = needed.min(height.saturating_sub(2)).clamp(10, 36);

    if std::io::stdout().is_terminal() {
        print!("{}", render_ansi(reports, now, width.max(80), h, clock));
    } else {
        println!("{}", render_plain(reports, now, 120, 40, clock));
    }
    Ok(())
}

enum RefreshOutcome {
    Success(String),
    Error(String),
}

struct ShutdownWorkerGuard {
    cancel_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    child_pid: std::sync::Arc<std::sync::atomic::AtomicU32>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ShutdownWorkerGuard {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            self.cancel_flag
                .store(true, std::sync::atomic::Ordering::SeqCst);
            #[cfg(unix)]
            {
                let pid = self.child_pid.load(std::sync::atomic::Ordering::SeqCst);
                if let Ok(raw_pid) = i32::try_from(pid)
                    && raw_pid > 0
                    && let Some(rpid) = rustix::process::Pid::from_raw(raw_pid)
                {
                    let _ =
                        rustix::process::kill_process_group(rpid, rustix::process::Signal::KILL);
                }
            }
            let _ = h.join();
        }
    }
}

fn reload_incremental(
    path: &Path,
    cursor: &mut crate::store::Cursor,
    acc: &mut Vec<Sample>,
    tight: f64,
) -> Option<Vec<BucketReport>> {
    let inc = crate::store::load_from(path, *cursor).ok()?;
    if inc.reset {
        *acc = inc.loaded.samples;
    } else if inc.loaded.samples.is_empty() {
        *cursor = inc.cursor;
        return None;
    } else {
        acc.extend(inc.loaded.samples);
    }
    *cursor = inc.cursor;
    Some(crate::forecast::analyze(acc, tight))
}

pub fn run(data_dir: &Path, tight: f64, agy_bin: &str) -> Result<()> {
    let mut terminal = ratatui::try_init()?;
    let result = (|| -> Result<()> {
        let samples_path = data_dir.join("samples.jsonl");
        let mut last_reload = Instant::now() - StdDuration::from_secs(60);
        let mut reports: Vec<BucketReport> = Vec::new();
        // The interactive view shows only relative times, so `state.clock` is
        // unused here; plumb `--clock` through `run` before adding absolute ones.
        let mut state = ViewState::default();
        let mut cursor = crate::store::Cursor::default();
        let mut accumulated_samples: Vec<Sample> = Vec::new();

        let (refresh_tx, refresh_rx) = std::sync::mpsc::channel::<RefreshOutcome>();
        let mut is_refreshing = false;
        let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child_pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut worker_guard = ShutdownWorkerGuard {
            cancel_flag: cancel_flag.clone(),
            child_pid: child_pid.clone(),
            handle: None,
        };

        loop {
            while let Ok(outcome) = refresh_rx.try_recv() {
                is_refreshing = false;
                if let Some(h) = worker_guard.handle.take() {
                    let _ = h.join();
                }
                match outcome {
                    RefreshOutcome::Success(msg) => {
                        state.message = Some(msg);
                        if let Some(new_reports) = reload_incremental(
                            &samples_path,
                            &mut cursor,
                            &mut accumulated_samples,
                            tight,
                        ) {
                            reports = new_reports;
                        }
                        last_reload = Instant::now();
                    }
                    RefreshOutcome::Error(err) => {
                        state.message = Some(err);
                    }
                }
            }

            if is_refreshing
                && worker_guard
                    .handle
                    .as_ref()
                    .is_some_and(|h| h.is_finished())
            {
                if let Some(h) = worker_guard.handle.take()
                    && h.join().is_err()
                {
                    state.message = Some("Refresh worker panicked".to_string());
                }
                is_refreshing = false;
            }

            let now_instant = Instant::now();
            if now_instant.duration_since(last_reload) >= StdDuration::from_secs(30) {
                if let Some(new_reports) =
                    reload_incremental(&samples_path, &mut cursor, &mut accumulated_samples, tight)
                {
                    reports = new_reports;
                }
                last_reload = now_instant;
            }

            let now_utc = Utc::now();
            terminal.draw(|f| draw(f, &reports, &state, now_utc))?;

            if event::poll(StdDuration::from_millis(250))?
                && let Event::Key(key) = event::read()?
            {
                if (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
                    || key.code == KeyCode::Char('q')
                {
                    break;
                }
                match key.code {
                    KeyCode::Esc => {
                        state.focus = None;
                    }
                    KeyCode::Char('r') => {
                        if is_refreshing {
                            state.message = Some("Refresh already in progress...".to_string());
                        } else {
                            is_refreshing = true;
                            state.message = Some("Refreshing in background...".to_string());
                            let tx = refresh_tx.clone();
                            let agy = agy_bin.to_string();
                            let sp = samples_path.clone();
                            let thread_cancel = cancel_flag.clone();
                            let thread_child_pid = child_pid.clone();
                            thread_cancel.store(false, std::sync::atomic::Ordering::SeqCst);
                            thread_child_pid.store(0, std::sync::atomic::Ordering::SeqCst);
                            worker_guard.handle = Some(std::thread::spawn(move || {
                                let outcome =
                                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                                        || match crate::source::fetch_with_timeout_and_cancel(
                                            &agy,
                                            std::time::Duration::from_secs(25),
                                            Some(&thread_cancel),
                                            Some(&thread_child_pid),
                                        ) {
                                            Ok(json) => {
                                                match crate::source::parse_usage(&json, Utc::now())
                                                {
                                                    Ok(samples) => {
                                                        if samples.is_empty() {
                                                            RefreshOutcome::Error(
                                                            "No enabled quota buckets in agy output"
                                                                .to_string(),
                                                        )
                                                        } else if let Err(e) =
                                                            crate::store::append(&sp, &samples)
                                                        {
                                                            RefreshOutcome::Error(format!(
                                                                "Error appending samples: {}",
                                                                e
                                                            ))
                                                        } else {
                                                            RefreshOutcome::Success(
                                                                "Refreshed successfully"
                                                                    .to_string(),
                                                            )
                                                        }
                                                    }
                                                    Err(e) => RefreshOutcome::Error(format!(
                                                        "Error parsing: {}",
                                                        e
                                                    )),
                                                }
                                            }
                                            Err(e) => RefreshOutcome::Error(format!(
                                                "Error fetching: {}",
                                                e
                                            )),
                                        },
                                    )) {
                                        Ok(res) => res,
                                        Err(_) => RefreshOutcome::Error(
                                            "Refresh worker panicked".to_string(),
                                        ),
                                    };
                                let _ = tx.send(outcome);
                            }));
                        }
                    }
                    KeyCode::Char(c) if ('1'..='9').contains(&c) => {
                        let idx = (c as usize) - ('1' as usize);
                        if idx < reports.len() {
                            state.focus = Some(idx);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forecast::{BucketReport, analyze};
    use chrono::{TimeZone, Utc};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn make_test_reports() -> Vec<BucketReport> {
        let fixture = include_str!("../tests/fixtures/usage.json");
        let t1 = Utc.with_ymd_and_hms(2026, 10, 11, 16, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 10, 11, 17, 0, 0).unwrap();
        let mut s1 = crate::source::parse_usage(fixture, t1).unwrap();
        let mut s2 = crate::source::parse_usage(fixture, t2).unwrap();
        for s in &mut s2 {
            s.remaining -= 0.02;
        }
        s1.append(&mut s2);
        analyze(&s1, 0.10)
    }

    #[test]
    fn renders_titles_and_table() {
        let reports = make_test_reports();
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let text = render_plain(&reports, now, 120, 40, ClockFormat::H24);

        assert!(text.contains("Gemini Models · Weekly"));
        assert!(text.contains("Claude and GPT models · 5h"));
        assert!(text.contains("in 1d 13h"));
        assert!(text.contains("on track"));
    }

    #[test]
    fn tiny_terminal_no_panic() {
        let reports = make_test_reports();
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = ViewState::default();
        let res = terminal.draw(|f| draw(f, &reports, &state, now));
        assert!(res.is_ok());
    }

    #[test]
    fn many_buckets() {
        let base_reports = make_test_reports();
        let mut reports = base_reports.clone();
        for i in 1..=2 {
            let mut rep = base_reports[0].clone();
            rep.group = format!("Extra Group {}", i);
            rep.bucket_id = format!("extra-{}", i);
            reports.push(rep);
        }
        assert_eq!(reports.len(), 6);

        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let text = render_plain(&reports, now, 120, 40, ClockFormat::H24);
        assert!(text.contains("Gemini Models · Weekly"));
        assert!(text.contains("Gemini Models · 5h"));
        assert!(text.contains("Claude and GPT models · Weekly"));
        assert!(text.contains("Claude and GPT models · 5h"));
        assert!(text.contains("Extra Group 1 · Weekly"));
        assert!(text.contains("Extra Group 2 · Weekly"));
    }

    #[test]
    fn empty_state() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let text = render_plain(&[], now, 120, 40, ClockFormat::H24);
        assert!(text.contains("install-timer"));
    }

    #[test]
    fn focus_shows_one() {
        let reports = make_test_reports();
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = ViewState {
            focus: Some(1),
            message: None,
            is_once: false,
            ..Default::default()
        };
        terminal.draw(|f| draw(f, &reports, &state, now)).unwrap();

        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..40 {
            for x in 0..120 {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }

        assert!(text.contains("Gemini Models · 5h"));
        assert!(!text.contains("Gemini Models · Weekly"));
        assert!(!text.contains("Claude and GPT models · Weekly"));
        assert!(!text.contains("Claude and GPT models · 5h"));
    }

    #[test]
    fn disabled_bucket_chart_and_focus_rendering() {
        let mut reports = make_test_reports();
        reports[1].prognosis.status = Status::Disabled;
        reports[1].prognosis.reset_time = None;

        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();

        // 1. In grid view:
        let text_grid = render_plain(&reports, now, 80, 24, ClockFormat::H24);
        assert!(text_grid.contains("Gemini Models · 5h  disabled"));
        assert!(text_grid.contains("Disabled (weekly limit reached)"));
        assert!(text_grid.contains("🚫 disabled"));

        // 2. In focus view on the disabled bucket:
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = ViewState {
            focus: Some(1),
            message: None,
            is_once: false,
            ..Default::default()
        };
        terminal.draw(|f| draw(f, &reports, &state, now)).unwrap();

        let buffer = terminal.backend().buffer();
        let mut text_focus = String::new();
        for y in 0..24 {
            for x in 0..80 {
                text_focus.push_str(buffer[(x, y)].symbol());
            }
            text_focus.push('\n');
        }
        assert!(text_focus.contains("Gemini Models · 5h  disabled"));
        assert!(text_focus.contains("Disabled (weekly limit reached)"));
    }

    #[test]
    fn render_plain_text() {
        let reports = make_test_reports();
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let text = render_plain(&reports, now, 120, 40, ClockFormat::H24);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 40);
        assert!(text.contains("Gemini Models · Weekly"));
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn render_ansi_text() {
        let reports = make_test_reports();
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let text = render_ansi(&reports, now, 120, 40, ClockFormat::H24);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 41); // 40 lines + trailing newline
        assert!(text.contains("Gemini Models · Weekly"));
        assert!(text.contains('\x1b'));
    }

    #[test]
    fn focus_badges_rendered_in_chart_and_table() {
        let reports = make_test_reports();
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let text = render_plain(&reports, now, 120, 40, ClockFormat::H24);

        // Chart titles have [1], [2], [3], [4]
        assert!(text.contains("[1] Gemini Models · Weekly"));
        assert!(text.contains("[2] Gemini Models · 5h"));

        // Table rows have [1] Gemini Models, [2] Gemini Models
        let weekly_row = text
            .lines()
            .find(|l| l.contains("Weekly") && l.starts_with('│'))
            .expect("table row with Weekly window not found");
        assert!(
            weekly_row.contains("[1] Gemini Models"),
            "table row group name was truncated in 120x40: {}",
            weekly_row
        );
        // In 120-column view, full long group name expands and is not truncated
        assert!(
            text.contains("[3] Claude and GPT models"),
            "long group name truncated in 120x40: {}",
            text
        );

        // Verify in 80x24 terminal that table row group name and status are not truncated
        let text_80 = render_plain(&reports, now, 80, 24, ClockFormat::H24);
        let table_row_80 = text_80
            .lines()
            .find(|l| l.contains("Weekly") && l.starts_with('│'))
            .expect("table row with Weekly window not found in 80x24");
        assert!(
            table_row_80.contains("[1] Gemini Models"),
            "table row group name was truncated in 80x24 terminal:\n{}",
            table_row_80
        );
        assert!(
            table_row_80.contains("in 1d 13h"),
            "table row status was truncated in 80x24 terminal:\n{}",
            table_row_80
        );

        // Verify right border alignment and exact width in 80x24 plain text
        use unicode_width::UnicodeWidthStr;
        for line in text_80.lines() {
            if line.starts_with('│') || line.starts_with('┌') || line.starts_with('└') {
                assert_eq!(
                    line.width(),
                    80,
                    "line display width must be exactly 80:\n{}",
                    line
                );
                assert!(
                    line.ends_with('│') || line.ends_with('┐') || line.ends_with('┘'),
                    "line must end with box border character:\n{}",
                    line
                );
            }
        }

        // Verify right border alignment and exact width in 80x24 ANSI text
        let ansi_80 = render_ansi(&reports, now, 80, 24, ClockFormat::H24);
        for line in ansi_80.lines() {
            let stripped = strip_ansi(line);
            if stripped.starts_with('│') || stripped.starts_with('┌') || stripped.starts_with('└')
            {
                assert_eq!(
                    stripped.width(),
                    80,
                    "stripped ANSI line display width must be exactly 80:\n{}",
                    stripped
                );
                assert!(
                    stripped.ends_with('│') || stripped.ends_with('┐') || stripped.ends_with('┘'),
                    "stripped ANSI line must end with box border character:\n{}",
                    stripped
                );
            }
        }
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut in_escape = false;
        for c in s.chars() {
            if c == '\x1b' {
                in_escape = true;
            } else if in_escape {
                if c.is_ascii_alphabetic() {
                    in_escape = false;
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn one_shot_footer_renders_timestamp_instead_of_interactive_hints() {
        let reports = make_test_reports();
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let text = render_plain(&reports, now, 120, 40, ClockFormat::H24);

        let local = now.with_timezone(&Local);
        assert!(text.contains(&format!(
            "agy-usage · {}",
            local.format("%Y-%m-%d %H:%M:%S")
        )));
        assert!(!text.contains("UTC"));
        assert!(!text.contains("Esc: Grid  1-9: Focus  r: Refresh  q: Quit"));
    }

    #[test]
    fn one_shot_footer_follows_12h_clock() {
        let reports = make_test_reports();
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let text = render_plain(&reports, now, 120, 40, ClockFormat::H12);

        let local = now.with_timezone(&Local);
        assert!(text.contains(&format!(
            "agy-usage · {}",
            local.format("%Y-%m-%d %I:%M:%S %p")
        )));
    }

    #[test]
    fn interactive_footer_renders_key_hints() {
        let reports = make_test_reports();
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = ViewState {
            focus: None,
            message: None,
            is_once: false,
            ..Default::default()
        };
        terminal.draw(|f| draw(f, &reports, &state, now)).unwrap();

        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..40 {
            for x in 0..120 {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }

        assert!(text.contains("Esc: Grid  1-9: Focus  r: Refresh  q: Quit"));
    }

    #[test]
    fn test_shutdown_worker_guard_cancels_and_joins_on_drop() {
        let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child_pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

        let cancel_clone = cancel_flag.clone();
        let handle = std::thread::spawn(move || {
            while !cancel_clone.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });

        {
            let _guard = ShutdownWorkerGuard {
                cancel_flag: cancel_flag.clone(),
                child_pid: child_pid.clone(),
                handle: Some(handle),
            };
            // Guard dropped here
        }

        assert!(cancel_flag.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    #[cfg(unix)]
    fn test_shutdown_worker_guard_kills_child_process_on_drop() {
        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("10");
        cmd.process_group(0);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();

        let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child_pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(pid));

        let cancel_clone = cancel_flag.clone();
        let handle = std::thread::spawn(move || {
            while !cancel_clone.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });

        {
            let _guard = ShutdownWorkerGuard {
                cancel_flag: cancel_flag.clone(),
                child_pid: child_pid.clone(),
                handle: Some(handle),
            };
            // Guard dropped here - kills process group and child
        }

        // Child process should have been killed by SIGKILL
        let status = child.wait().unwrap();
        assert!(!status.success());
    }

    #[test]
    fn test_reload_incremental_behaviour() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");

        let mut cursor = crate::store::Cursor::default();
        let mut acc = Vec::new();

        // 1. Initial load from non-existent file
        let reports = reload_incremental(&path, &mut cursor, &mut acc, 0.1);
        assert!(reports.is_none());
        assert!(acc.is_empty());

        // 2. Append sample
        let s1 = Sample {
            ts: Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap(),
            group: "Gemini".to_string(),
            bucket_id: "b1".to_string(),
            window: "5h".to_string(),
            remaining: 0.9,
            reset_time: Some(Utc.with_ymd_and_hms(2026, 10, 11, 17, 0, 0).unwrap()),
            disabled: false,
        };
        crate::store::append(&path, std::slice::from_ref(&s1)).unwrap();

        let reports = reload_incremental(&path, &mut cursor, &mut acc, 0.1);
        assert!(reports.is_some());
        assert_eq!(acc, vec![s1.clone()]);

        // 3. No changes: returns None, keeps acc
        let reports_none = reload_incremental(&path, &mut cursor, &mut acc, 0.1);
        assert!(reports_none.is_none());
        assert_eq!(acc, vec![s1.clone()]);

        // 4. Append another sample
        let s2 = Sample {
            ts: Utc.with_ymd_and_hms(2026, 10, 11, 12, 5, 0).unwrap(),
            group: "Gemini".to_string(),
            bucket_id: "b1".to_string(),
            window: "5h".to_string(),
            remaining: 0.85,
            reset_time: Some(Utc.with_ymd_and_hms(2026, 10, 11, 17, 0, 0).unwrap()),
            disabled: false,
        };
        crate::store::append(&path, std::slice::from_ref(&s2)).unwrap();

        let reports = reload_incremental(&path, &mut cursor, &mut acc, 0.1);
        assert!(reports.is_some());
        assert_eq!(acc, vec![s1.clone(), s2.clone()]);

        // 5. Prune old sample
        crate::store::prune(&path, Utc.with_ymd_and_hms(2026, 10, 11, 12, 1, 0).unwrap()).unwrap();

        let reports = reload_incremental(&path, &mut cursor, &mut acc, 0.1);
        assert!(reports.is_some());
        assert_eq!(acc, vec![s2]);
    }

    #[test]
    fn all_status_variants_render_at_exact_terminal_width() {
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let base = &make_test_reports()[0];

        let statuses = [
            Status::OnTrack,
            Status::Tight,
            Status::WillRunOut,
            Status::Collecting,
            Status::Disabled,
        ];
        let reports: Vec<BucketReport> = statuses
            .into_iter()
            .enumerate()
            .map(|(i, status)| {
                let mut rep = base.clone();
                rep.group = format!("Group {}", i + 1);
                rep.prognosis.status = status;
                if status == Status::WillRunOut {
                    rep.prognosis.empty_at = Some(now + chrono::Duration::hours(2));
                }
                rep
            })
            .collect();

        terminal
            .draw(|f| {
                draw_table(f, &reports, f.area(), now, None);
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        use unicode_width::UnicodeWidthStr;

        let rendered_lines: Vec<String> = (0..10)
            .map(|y| {
                let mut line = String::new();
                for cell in visible_cells(buffer, y, 80) {
                    line.push_str(cell.symbol());
                }
                line
            })
            .collect();

        // 1. Verify line widths and border endings for all box lines
        for (y, line) in rendered_lines.iter().enumerate() {
            if line.starts_with('│') || line.starts_with('┌') || line.starts_with('└') {
                assert_eq!(
                    line.as_str().width(),
                    80,
                    "line at y={y} display width must be exactly 80: {line}",
                );
                assert!(
                    line.ends_with('│') || line.ends_with('┐') || line.ends_with('┘'),
                    "line at y={y} must end with box border character: {line}",
                );
            }
        }

        // 2. Directly assert that the expected status strings appear in their respective rows
        assert!(rendered_lines[2].contains("✅ on track"));
        assert!(rendered_lines[3].contains("⚠  tight"));
        assert!(rendered_lines[4].contains("❌ in 2h"));
        assert!(rendered_lines[5].contains("⏳ collecting"));
        assert!(rendered_lines[6].contains("🚫 disabled"));

        // 3. Inspect the status cells directly for Tight at y=3:
        // Column 57 has '⚠', which must have display width 1 (not wide 2),
        // followed by two single-space cells at columns 58 and 59,
        // and 'tight' starting at column 60.
        let tight_icon_cell = &buffer[(57, 3)];
        assert_eq!(tight_icon_cell.symbol(), "⚠");
        assert_eq!(
            tight_icon_cell.symbol().width(),
            1,
            "warning sign must be 1 column wide"
        );
        assert_eq!(buffer[(58, 3)].symbol(), " ");
        assert_eq!(buffer[(59, 3)].symbol(), " ");
        assert_eq!(buffer[(60, 3)].symbol(), "t");
        assert_eq!(buffer[(61, 3)].symbol(), "i");
        assert_eq!(buffer[(62, 3)].symbol(), "g");
        assert_eq!(buffer[(63, 3)].symbol(), "h");
        assert_eq!(buffer[(64, 3)].symbol(), "t");

        // 4. Compare with 2-column emoji in OnTrack (y=2) and WillRunOut (y=4):
        // '✅' at (57, 2) has display width 2, followed by ratatui reset placeholder at (58, 2),
        // space at (59, 2), and text starting at (60, 2).
        let check_cell = &buffer[(57, 2)];
        assert_eq!(check_cell.symbol(), "✅");
        assert_eq!(check_cell.symbol().width(), 2);
        assert_eq!(buffer[(59, 2)].symbol(), " ");
        assert_eq!(buffer[(60, 2)].symbol(), "o");

        let cross_cell = &buffer[(57, 4)];
        assert_eq!(cross_cell.symbol(), "❌");
        assert_eq!(cross_cell.symbol().width(), 2);
        assert_eq!(buffer[(59, 4)].symbol(), " ");
        assert_eq!(buffer[(60, 4)].symbol(), "i");
    }
}
