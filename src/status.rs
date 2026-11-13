use std::io::Write;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Local, Utc};
use serde::Serialize;

use crate::clock::ClockFormat;
use crate::forecast::{BucketReport, Status};
use crate::model::{STALE_THRESHOLD_MINS, round_decimals, round4};
use crate::sanitize_single_line;

pub const STATUS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFormat {
    Oneline,
    Waybar,
    Tmux,
    Json,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BucketStatus {
    pub label: String,
    pub group: String,
    pub bucket_id: String,
    pub window: String,
    pub status: Status,
    pub status_label: String,
    pub remaining: f64,
    pub remaining_pct: f64,
    pub budget_now: Option<f64>,
    pub rate_per_h: Option<f64>,
    pub safe_rate_per_h: Option<f64>,
    pub at_reset: Option<f64>,
    pub reset_time: Option<DateTime<Utc>>,
    pub empty_at: Option<DateTime<Utc>>,
    pub last_sample: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusOutput {
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub clock: ClockFormat,
    pub worst_status: Status,
    pub stale: bool,
    pub buckets: Vec<BucketStatus>,
}

#[derive(Debug, Serialize)]
struct WaybarPayload {
    text: String,
    tooltip: String,
    class: Vec<&'static str>,
    percentage: u8,
}

fn round1(value: f64) -> f64 {
    round_decimals(value, 10.0)
}

fn status_severity(status: Status) -> u8 {
    match status {
        Status::WillRunOut => 4,
        Status::Tight => 3,
        Status::Collecting => 2,
        Status::OnTrack => 1,
        Status::Disabled => 0,
    }
}

fn status_icon(status: Status) -> &'static str {
    match status {
        Status::WillRunOut => "❌",
        Status::Tight => "⚠",
        Status::Collecting => "⏳",
        Status::OnTrack => "✅",
        Status::Disabled => "🚫",
    }
}

fn waybar_class(status: Status) -> &'static str {
    match status {
        Status::WillRunOut => "critical",
        Status::Tight => "warning",
        Status::OnTrack => "ok",
        Status::Collecting => "collecting",
        Status::Disabled => "disabled",
    }
}

fn escape_pango(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

fn rounded_pct(pct: f64) -> u8 {
    pct.round().clamp(0.0, 100.0) as u8
}

fn bucket_priority_cmp(a: &BucketStatus, b: &BucketStatus) -> std::cmp::Ordering {
    status_severity(a.status)
        .cmp(&status_severity(b.status))
        .then_with(|| b.remaining.total_cmp(&a.remaining))
}

/// Builds a structured status snapshot using the default 15-minute staleness threshold.
pub fn build_status(
    reports: &[BucketReport],
    now: DateTime<Utc>,
    clock: ClockFormat,
) -> StatusOutput {
    build_status_with_stale_threshold(reports, now, clock, Duration::minutes(STALE_THRESHOLD_MINS))
}

/// Builds a structured status snapshot using a custom staleness threshold (e.g., `max(interval * 2, 15m)`).
pub fn build_status_with_stale_threshold(
    reports: &[BucketReport],
    now: DateTime<Utc>,
    clock: ClockFormat,
    stale_threshold: Duration,
) -> StatusOutput {
    let buckets: Vec<BucketStatus> = reports
        .iter()
        .map(|rep| BucketStatus {
            label: rep.label(reports),
            group: rep.group.clone(),
            bucket_id: rep.bucket_id.clone(),
            window: rep.window.clone(),
            status: rep.prognosis.status,
            status_label: rep.prognosis.status_label(now),
            remaining: round4(rep.prognosis.remaining_now),
            remaining_pct: round1((rep.prognosis.remaining_now * 100.0).clamp(0.0, 100.0)),
            budget_now: rep.prognosis.budget_now.map(round4),
            rate_per_h: rep.prognosis.rate_per_h.map(round4),
            safe_rate_per_h: rep.prognosis.safe_rate_per_h.map(round4),
            at_reset: rep.prognosis.at_reset.map(round4),
            reset_time: rep.prognosis.reset_time,
            empty_at: rep.prognosis.empty_at,
            last_sample: rep.prognosis.last_sample,
        })
        .collect();

    let worst_status = buckets
        .iter()
        .filter(|b| b.status != Status::Disabled)
        .max_by(|a, b| bucket_priority_cmp(a, b))
        .or_else(|| buckets.first())
        .map(|b| b.status)
        .unwrap_or(Status::Collecting);

    let newest_sample = buckets
        .iter()
        .filter(|b| b.status != Status::Disabled)
        .map(|b| b.last_sample)
        .max()
        .or_else(|| buckets.iter().map(|b| b.last_sample).max());
    let stale = match newest_sample {
        Some(ts) => now.signed_duration_since(ts) > stale_threshold,
        None => true,
    };

    StatusOutput {
        schema_version: STATUS_SCHEMA_VERSION,
        generated_at: now,
        clock,
        worst_status,
        stale,
        buckets,
    }
}

fn active_buckets(out: &StatusOutput) -> Vec<&BucketStatus> {
    let active: Vec<&BucketStatus> = out
        .buckets
        .iter()
        .filter(|b| b.status != Status::Disabled)
        .collect();
    if active.is_empty() {
        out.buckets.iter().collect()
    } else {
        active
    }
}

fn primary_bucket(out: &StatusOutput) -> Option<&BucketStatus> {
    active_buckets(out)
        .into_iter()
        .max_by(|a, b| bucket_priority_cmp(a, b))
}

fn format_oneline(out: &StatusOutput) -> String {
    if out.buckets.is_empty() {
        return "agy: no samples".to_string();
    }
    let parts: Vec<String> = active_buckets(out)
        .into_iter()
        .map(|b| {
            let label = sanitize_single_line(&b.label);
            let tag = match b.status {
                Status::WillRunOut => sanitize_single_line(&b.status_label),
                _ => status_icon(b.status).to_string(),
            };
            format!("{label}: {}% ({tag})", rounded_pct(b.remaining_pct))
        })
        .collect();
    let mut line = parts.join(" | ");
    if out.stale {
        line.push_str(" [stale]");
    }
    line
}

fn format_tmux(out: &StatusOutput) -> String {
    if out.buckets.is_empty() {
        return "agy: --".to_string();
    }
    // Strip `#` so untrusted bucket labels cannot accidentally inject tmux format/command directives.
    let sanitize_tmux = |s: &str| sanitize_single_line(s).replace('#', "");
    let parts: Vec<String> = active_buckets(out)
        .into_iter()
        .map(|b| {
            let icon = status_icon(b.status);
            let label = sanitize_tmux(&b.label);
            format!("{label} {}% {icon}", rounded_pct(b.remaining_pct))
        })
        .collect();
    let mut line = parts.join(" · ");
    if out.stale {
        line.push_str(" (stale)");
    }
    line
}

fn format_waybar(out: &StatusOutput) -> Result<String> {
    let mut classes = vec![waybar_class(out.worst_status)];
    if out.stale {
        classes.push("stale");
    }

    let (text, percentage, tooltip) = match primary_bucket(out) {
        None => (
            "agy: --".to_string(),
            0,
            "No samples recorded yet.\nRun `agy-usage collect` first.".to_string(),
        ),
        Some(primary) => {
            let icon = status_icon(primary.status);
            let pct_u8 = rounded_pct(primary.remaining_pct);
            let text = if out.stale {
                format!("{icon} {pct_u8}% !")
            } else {
                format!("{icon} {pct_u8}%")
            };

            let mut lines: Vec<String> = out
                .buckets
                .iter()
                .map(|b| {
                    let reset_str = match b.reset_time {
                        Some(dt) => {
                            let local = dt.with_timezone(&Local);
                            format!(" | resets {}", out.clock.format_datetime(&local))
                        }
                        None => String::new(),
                    };
                    format!(
                        "{}: {:.1}% ({}){}",
                        escape_pango(&sanitize_single_line(&b.label)),
                        b.remaining_pct,
                        escape_pango(&sanitize_single_line(&b.status_label)),
                        reset_str
                    )
                })
                .collect();
            if let Some(newest) = out.buckets.iter().map(|b| b.last_sample).max() {
                let local = newest.with_timezone(&Local);
                let stale_note = if out.stale { " [stale]" } else { "" };
                lines.push(format!(
                    "Last sample: {}{}",
                    out.clock.format_datetime(&local),
                    stale_note
                ));
            }
            (text, pct_u8, lines.join("\n"))
        }
    };

    serde_json::to_string(&WaybarPayload {
        text,
        tooltip,
        class: classes,
        percentage,
    })
    .context("failed to serialize waybar payload")
}

/// Formats `StatusOutput` in the requested output representation.
pub fn format_status(out: &StatusOutput, format: StatusFormat) -> Result<String> {
    match format {
        StatusFormat::Oneline => Ok(format_oneline(out)),
        StatusFormat::Tmux => Ok(format_tmux(out)),
        StatusFormat::Waybar => format_waybar(out),
        StatusFormat::Json => {
            serde_json::to_string_pretty(out).context("failed to serialize status JSON")
        }
    }
}

/// Writes formatted status output, tolerating `BrokenPipe` when downstream closes early.
pub fn write_status(
    out: &StatusOutput,
    format: StatusFormat,
    writer: &mut impl Write,
) -> Result<()> {
    let rendered = format_status(out, format)?;
    crate::write_line_broken_pipe_safe(&rendered, "failed to write status output", writer)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    #[test]
    fn sanitize_single_line_strips_controls_and_unicode_separators_and_rounds_uniformly() {
        let dirty = "bad\x1b[31m\0\x07line\u{0085}next\u{2028}para\u{2029}end\n\r\t!";
        assert_eq!(
            super::sanitize_single_line(dirty),
            "bad[31mline next para end   !"
        );

        // Halfway & near-100 percentages must round uniformly across all text formats.
        assert_eq!(super::rounded_pct(12.5), 13);
        assert_eq!(super::rounded_pct(99.6), 100);

        let now = chrono::Utc
            .with_ymd_and_hms(2026, 10, 12, 12, 0, 0)
            .unwrap();
        let mut out = super::build_status(&[], now, crate::clock::ClockFormat::H24);
        out.buckets.push(super::BucketStatus {
            label: "5h (\x1b[31mEvil\nLabel\u{2028}1)".to_string(),
            group: "G".to_string(),
            bucket_id: "b1".to_string(),
            window: "5h".to_string(),
            status: crate::forecast::Status::Tight,
            status_label: "tight\x07\u{2029}warn".to_string(),
            remaining: 0.125,
            remaining_pct: 12.5,
            budget_now: None,
            rate_per_h: None,
            safe_rate_per_h: None,
            at_reset: None,
            reset_time: None,
            empty_at: None,
            last_sample: now,
        });
        out.stale = false;
        let oneline = super::format_oneline(&out);
        assert!(
            oneline.contains("5h ([31mEvil Label 1): 13%"),
            "got: {oneline}"
        );
        assert!(!oneline.contains('\x1b') && !oneline.contains('\n'));

        let tmux = super::format_tmux(&out);
        assert!(tmux.contains("13%"), "got: {tmux}");

        let waybar: serde_json::Value =
            serde_json::from_str(&super::format_waybar(&out).unwrap()).unwrap();
        assert_eq!(waybar["percentage"], 13);
        let tip = waybar["tooltip"].as_str().unwrap();
        assert!(
            tip.contains("5h ([31mEvil Label 1): 12.5% (tight warn)"),
            "got: {tip}"
        );
        assert!(!tip.contains('\x1b') && !tip.contains('\x07'));

        // Custom stale threshold (e.g. 60m timer => 120m threshold) overrides 15m default.
        let sample_ts = now - chrono::Duration::minutes(40);
        let s = crate::model::Sample {
            ts: sample_ts,
            group: "Gemini".to_string(),
            bucket_id: "g-5h".to_string(),
            window: "5h".to_string(),
            remaining: 0.8,
            reset_time: Some(now + chrono::Duration::hours(4)),
            disabled: false,
        };
        let reps = crate::forecast::analyze(&[s], 0.10);
        assert!(super::build_status(&reps, now, crate::clock::ClockFormat::H24).stale);
        assert!(
            !super::build_status_with_stale_threshold(
                &reps,
                now,
                crate::clock::ClockFormat::H24,
                chrono::Duration::minutes(120)
            )
            .stale
        );
    }

    use super::*;
    use crate::forecast::Prognosis;

    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn report(
        group: &str,
        id: &str,
        window: &str,
        remaining: f64,
        status: Status,
        last_sample: DateTime<Utc>,
        empty_at: Option<DateTime<Utc>>,
    ) -> BucketReport {
        BucketReport {
            group: group.to_string(),
            bucket_id: id.to_string(),
            window: window.to_string(),
            history: vec![],
            current: vec![],
            window_start: None,
            boundaries: vec![],
            prognosis: Prognosis {
                remaining_now: remaining,
                reset_time: Some(last_sample + Duration::hours(4)),
                last_sample,
                budget_now: Some(0.5),
                rate_per_h: Some(0.1),
                safe_rate_per_h: Some(0.08),
                at_reset: Some(0.2),
                empty_at,
                status,
            },
        }
    }

    #[test]
    fn build_status_empty_reports_is_stale_and_collecting() {
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
        let out = build_status(&[], now, ClockFormat::H24);
        assert_eq!(out.schema_version, 1);
        assert_eq!(out.worst_status, Status::Collecting);
        assert!(out.stale);
        assert!(out.buckets.is_empty());
        assert_eq!(
            format_status(&out, StatusFormat::Oneline).unwrap(),
            "agy: no samples"
        );
        assert_eq!(format_status(&out, StatusFormat::Tmux).unwrap(), "agy: --");
        let waybar: serde_json::Value =
            serde_json::from_str(&format_status(&out, StatusFormat::Waybar).unwrap()).unwrap();
        assert_eq!(waybar["text"], "agy: --");
        assert_eq!(waybar["percentage"], 0);
        assert_eq!(waybar["class"], serde_json::json!(["collecting", "stale"]));
    }

    #[test]
    fn status_prioritizes_enabled_buckets_and_formats_all_outputs() {
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
        let fresh = now - Duration::minutes(3);
        let reports = vec![
            report(
                "Gemini Models",
                "g-5h",
                "5h",
                0.75,
                Status::OnTrack,
                fresh,
                None,
            ),
            report(
                "Claude 4.5",
                "c-5h",
                "5h",
                0.35,
                Status::WillRunOut,
                fresh,
                Some(now + Duration::minutes(68)),
            ),
            report(
                "Old Tier",
                "o-5h",
                "5h",
                0.0,
                Status::Disabled,
                now - Duration::hours(3),
                None,
            ),
        ];

        let out = build_status(&reports, now, ClockFormat::H12);
        assert_eq!(out.worst_status, Status::WillRunOut);
        assert!(!out.stale);
        assert_eq!(out.buckets.len(), 3);

        let json_str = format_status(&out, StatusFormat::Json).unwrap();
        let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["clock"], "12h");
        assert_eq!(json["worst_status"], "WillRunOut");
        assert_eq!(json["stale"], false);
        assert_eq!(json["buckets"][1]["remaining_pct"], 35.0);

        let oneline = format_status(&out, StatusFormat::Oneline).unwrap();
        assert!(oneline.contains("Gemini Models · 5h: 75%"));
        assert!(oneline.contains("Claude 4.5 · 5h: 35% (❌ in 1h 8m)"));
        assert!(!oneline.contains("Old Tier"));
        assert!(!oneline.contains('\n'));

        let tmux = format_status(&out, StatusFormat::Tmux).unwrap();
        assert!(!tmux.contains("#("));
        assert!(!tmux.contains('\n'));
        assert!(tmux.contains("35% ❌"));

        let waybar: serde_json::Value =
            serde_json::from_str(&format_status(&out, StatusFormat::Waybar).unwrap()).unwrap();
        assert_eq!(waybar["text"], "❌ 35%");
        assert_eq!(waybar["percentage"], 35);
        assert_eq!(waybar["class"], serde_json::json!(["critical"]));
        assert!(
            waybar["tooltip"]
                .as_str()
                .unwrap()
                .contains("Claude 4.5 · 5h")
        );
    }

    #[test]
    fn stale_detection_triggers_past_fifteen_minutes_and_tmux_strips_hashes() {
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
        let stale_ts = now - Duration::minutes(16);
        let reports = vec![report(
            "Group #(whoami)",
            "g1",
            "5h",
            0.5,
            Status::Tight,
            stale_ts,
            None,
        )];
        let out = build_status(&reports, now, ClockFormat::H24);
        assert!(out.stale);
        let oneline = format_status(&out, StatusFormat::Oneline).unwrap();
        assert!(oneline.ends_with("[stale]"));
        let tmux = format_status(&out, StatusFormat::Tmux).unwrap();
        assert!(!tmux.contains('#'));
        assert!(tmux.ends_with("(stale)"));
    }

    #[test]
    fn write_status_handles_broken_pipe_cleanly() {
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
        let out = build_status(&[], now, ClockFormat::H24);
        let mut writer = BrokenPipeWriter;
        assert!(write_status(&out, StatusFormat::Json, &mut writer).is_ok());
    }

    #[test]
    fn waybar_tooltip_escapes_pango_markup_while_plain_and_json_preserve_literals() {
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
        let reports = vec![report(
            "Group <script>&\"",
            "b1",
            "5h",
            0.5,
            Status::OnTrack,
            now,
            None,
        )];
        let out = build_status(&reports, now, ClockFormat::H24);

        let waybar: serde_json::Value =
            serde_json::from_str(&format_status(&out, StatusFormat::Waybar).unwrap()).unwrap();
        let tooltip = waybar["tooltip"].as_str().unwrap();
        assert!(tooltip.contains("Group &lt;script&gt;&amp;\""));
        assert!(!tooltip.contains("<script>"));

        // Negative boundary assertion: oneline, tmux, and json do not HTML/Pango-escape labels.
        let oneline = format_status(&out, StatusFormat::Oneline).unwrap();
        assert!(oneline.contains("Group <script>&\""));
        let tmux = format_status(&out, StatusFormat::Tmux).unwrap();
        assert!(tmux.contains("Group <script>&\""));
        let json: serde_json::Value =
            serde_json::from_str(&format_status(&out, StatusFormat::Json).unwrap()).unwrap();
        assert_eq!(json["buckets"][0]["group"], "Group <script>&\"");
    }
}
