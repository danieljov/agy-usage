use std::io::IsTerminal;
use std::path::Path;
use std::time::{Duration as StdDuration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Borders, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table,
};

use crate::forecast::{BucketReport, Status};

#[derive(Default, Debug, Clone)]
pub struct ViewState {
    pub focus: Option<usize>,
    pub message: Option<String>,
}

fn format_window(win: &str) -> &'static str {
    match win {
        "weekly" => "Weekly",
        "5h" => "5h",
        _ => "Custom",
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

        draw_chart(frame, &reports[focus_idx], chunks[0]);
        draw_table(frame, &reports[focus_idx..focus_idx + 1], chunks[1], now);
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
                draw_chart(frame, &reports[report_idx], *c_chunk);
            }
        }
    }

    draw_table(frame, reports, chunks[1], now);
    draw_status_bar(frame, reports, state, chunks[2], now);
}

fn draw_chart(frame: &mut Frame, rep: &BucketReport, area: Rect) {
    if area.width < 4 || area.height < 3 {
        return;
    }

    let title = format!(
        "{} · {}  {:.0}% left",
        rep.group,
        format_window(&rep.window),
        (rep.prognosis.remaining_now * 100.0).round()
    );

    let actual_pts: Vec<(f64, f64)> = rep
        .current
        .iter()
        .map(|s| (s.ts.timestamp() as f64, s.remaining * 100.0))
        .collect();

    let budget_pts: Vec<(f64, f64)> = if let Some(ws) = rep.window_start {
        vec![
            (ws.timestamp() as f64, 100.0),
            (rep.prognosis.reset_time.timestamp() as f64, 0.0),
        ]
    } else {
        Vec::new()
    };

    let mut proj_pts: Vec<(f64, f64)> = Vec::new();
    if let Some(_rate) = rep.prognosis.rate_per_h.filter(|&r| r > 0.0) {
        let start_x = rep.prognosis.last_sample.timestamp() as f64;
        let start_y = rep.prognosis.remaining_now * 100.0;
        let end_x = rep
            .prognosis
            .empty_at
            .unwrap_or(rep.prognosis.reset_time)
            .timestamp() as f64;
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
        (rep.prognosis.reset_time.timestamp() as f64, 0.0),
        (rep.prognosis.reset_time.timestamp() as f64, 100.0),
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
    let mut x_max = rep.prognosis.reset_time.timestamp() as f64;
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

fn draw_table(frame: &mut Frame, reports: &[BucketReport], area: Rect, now: DateTime<Utc>) {
    if area.width < 4 || area.height < 2 {
        return;
    }

    let header = Row::new(vec![
        Cell::from("Group"),
        Cell::from("Window"),
        Cell::from("Left"),
        Cell::from("Rate/h"),
        Cell::from("Budget/h"),
        Cell::from("At reset"),
        Cell::from("Status"),
        Cell::from("Last sample"),
    ])
    .style(
        Style::default()
            .add_modifier(Modifier::BOLD)
            .fg(Color::Cyan),
    );

    let rows: Vec<Row> = reports
        .iter()
        .map(|rep| {
            let p = &rep.prognosis;
            let left_str = format!("{:.1}%", p.remaining_now * 100.0);
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
            let status_str = p.status.label();
            let age_str = format_time_ago(p.last_sample, now);

            Row::new(vec![
                Cell::from(rep.group.clone()),
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
        Constraint::Percentage(22),
        Constraint::Percentage(10),
        Constraint::Percentage(9),
        Constraint::Percentage(10),
        Constraint::Percentage(11),
        Constraint::Percentage(10),
        Constraint::Percentage(14),
        Constraint::Percentage(14),
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

    let left_text = if let Some(msg) = &state.message {
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

pub fn render_plain(
    reports: &[BucketReport],
    now: DateTime<Utc>,
    width: u16,
    height: u16,
) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let state = ViewState::default();
    terminal
        .draw(|f| draw(f, reports, &state, now))
        .expect("draw failed");
    let buffer = terminal.backend().buffer();
    let mut lines = Vec::with_capacity(height as usize);
    for y in 0..height {
        let mut line = String::with_capacity(width as usize);
        for x in 0..width {
            let cell = &buffer[(x, y)];
            line.push_str(cell.symbol());
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

pub fn print_once(reports: &[BucketReport], now: DateTime<Utc>) -> Result<()> {
    if std::io::stdout().is_terminal() {
        let height = ratatui::crossterm::terminal::size()
            .map(|(_, h)| h)
            .unwrap_or(40);
        let h = height.clamp(5, 40);
        let options = ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Inline(h),
        };
        let mut terminal = ratatui::init_with_options(options);
        let state = ViewState::default();
        terminal.draw(|f| draw(f, reports, &state, now))?;
        ratatui::restore();
        println!();
    } else {
        println!("{}", render_plain(reports, now, 120, 40));
    }
    Ok(())
}

pub fn run(data_dir: &Path, tight: f64, agy_bin: &str) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        let samples_path = data_dir.join("samples.jsonl");
        let mut last_reload = Instant::now() - StdDuration::from_secs(60);
        let mut reports: Vec<BucketReport> = Vec::new();
        let mut state = ViewState::default();

        loop {
            let now_instant = Instant::now();
            if now_instant.duration_since(last_reload) >= StdDuration::from_secs(30) {
                if let Ok(loaded) = crate::store::load(&samples_path) {
                    reports = crate::forecast::analyze(&loaded.samples, tight);
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
                    KeyCode::Char('r') => match crate::source::fetch(agy_bin) {
                        Ok(json) => match crate::source::parse_usage(&json, Utc::now()) {
                            Ok(samples) => {
                                if let Err(e) = crate::store::append(&samples_path, &samples) {
                                    state.message = Some(format!("Error appending samples: {}", e));
                                } else {
                                    state.message = Some("Refreshed successfully".to_string());
                                    if let Ok(loaded) = crate::store::load(&samples_path) {
                                        reports = crate::forecast::analyze(&loaded.samples, tight);
                                    }
                                    last_reload = Instant::now();
                                }
                            }
                            Err(e) => {
                                state.message = format!("Error parsing: {}", e).into();
                            }
                        },
                        Err(e) => {
                            state.message = format!("Error fetching: {}", e).into();
                        }
                    },
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
        let text = render_plain(&reports, now, 120, 40);

        assert!(text.contains("Gemini Models · Weekly"));
        assert!(text.contains("Claude and GPT models · 5h"));
        assert!(
            text.contains("on track")
                || text.contains("will run out")
                || text.contains("collecting")
        );
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
        let text = render_plain(&reports, now, 120, 40);
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
        let text = render_plain(&[], now, 120, 40);
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
    fn render_plain_text() {
        let reports = make_test_reports();
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let text = render_plain(&reports, now, 120, 40);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 40);
        assert!(text.contains("Gemini Models · Weekly"));
        assert!(!text.contains('\x1b'));
    }
}
