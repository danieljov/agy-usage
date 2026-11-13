use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::clock::ClockFormat;
use crate::forecast::{BucketReport, Prognosis};

const TEMPLATE: &str = include_str!("../assets/dashboard.html");
const CHART_JS: &str = include_str!("../assets/vendor/chart.umd.min.js");
const DATE_FNS_ADAPTER: &str =
    include_str!("../assets/vendor/chartjs-adapter-date-fns.bundle.min.js");
const PLATEAU_HEARTBEAT_MS: i64 = 900_000;

#[derive(Serialize)]
struct CompactReport<'a> {
    group: &'a str,
    bucket_id: &'a str,
    window: &'a str,
    label: String,
    window_secs: Option<i64>,
    window_start: Option<DateTime<Utc>>,
    reset_time: Option<DateTime<Utc>>,
    boundaries: Vec<DateTime<Utc>>,
    prognosis: &'a Prognosis,
    series: Vec<(i64, f64)>,
}

#[derive(Serialize)]
struct DashboardData<'a> {
    generated_at: DateTime<Utc>,
    clock: ClockFormat,
    reports: Vec<CompactReport<'a>>,
}

/// Compresses a chronological `(timestamp_ms, remaining)` series for HTML
/// embedding by rounding values to 4 decimal places and collapsing interior
/// flat plateau points while preserving series endpoints, plateau boundary
/// points before/after transitions, and a heartbeat point at least every 15
/// minutes (`900_000` ms) along long plateaus.
pub fn compress_series(points: &[(i64, f64)]) -> Vec<(i64, f64)> {
    debug_assert!(points.is_sorted_by_key(|p| p.0));
    if points.is_empty() {
        return Vec::new();
    }

    let rounded: Vec<(i64, f64)> = points
        .iter()
        .map(|&(ts, val)| (ts, crate::model::round4(val)))
        .collect();
    if rounded.len() <= 2 {
        return rounded;
    }

    let mut out = Vec::with_capacity(rounded.len());
    out.push(rounded[0]);
    let mut last_emitted_ts = rounded[0].0;

    for i in 1..rounded.len() - 1 {
        let (ts, val) = rounded[i];
        let prev_val = rounded[i - 1].1;
        let (next_ts, next_val) = rounded[i + 1];

        let is_plateau_edge = val != prev_val || val != next_val;
        let needs_heartbeat = next_ts.saturating_sub(last_emitted_ts) > PLATEAU_HEARTBEAT_MS;

        if is_plateau_edge || needs_heartbeat {
            out.push((ts, val));
            last_emitted_ts = ts;
        }
    }

    out.push(rounded[rounded.len() - 1]);
    out
}

/// Atomically writes `html` to `path` by creating parent directories if needed,
/// writing to a sibling temporary file in the same directory, and renaming it
/// into place. Any temporary file is removed automatically if writing fails.
pub fn write_atomic(path: &Path, html: &str) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let existing_perms = std::fs::metadata(path).ok().map(|m| m.permissions());
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(html.as_bytes())?;
    temp.flush()?;
    if let Some(perms) = existing_perms {
        temp.as_file().set_permissions(perms)?;
    }
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn render(
    reports: &[BucketReport],
    generated_at: DateTime<Utc>,
    history_days: u32,
    clock: ClockFormat,
) -> String {
    let cutoff = Duration::try_days(i64::from(history_days))
        .and_then(|d| generated_at.checked_sub_signed(d))
        .unwrap_or(DateTime::<Utc>::MIN_UTC);
    let compact_reports: Vec<CompactReport> = reports
        .iter()
        .map(|r| {
            let raw_series: Vec<(i64, f64)> = r
                .history
                .iter()
                .filter(|s| s.ts >= cutoff)
                .map(|s| (s.ts.timestamp_millis(), s.remaining))
                .collect();
            let series = compress_series(&raw_series);
            CompactReport {
                group: &r.group,
                bucket_id: &r.bucket_id,
                window: &r.window,
                label: r.label(reports),
                window_secs: crate::model::window_length(&r.window).map(|d| d.num_seconds()),
                window_start: r.window_start,
                reset_time: r.prognosis.reset_time,
                // Like `series`, limited to the displayed history so the output does
                // not depend on how much older history was loaded.
                boundaries: r
                    .boundaries
                    .iter()
                    .copied()
                    .filter(|&b| b >= cutoff)
                    .collect(),
                prognosis: &r.prognosis,
                series,
            }
        })
        .collect();

    let data = DashboardData {
        generated_at,
        clock,
        reports: compact_reports,
    };

    let json = serde_json::to_string(&data).expect("failed to serialize dashboard data");
    let safe_json = json.replace('<', "\\u003c");
    fill_template(
        TEMPLATE,
        &[
            ("/*__CHART_JS__*/", CHART_JS),
            ("/*__DATE_FNS_ADAPTER__*/", DATE_FNS_ADAPTER),
            ("/*__AGY_DATA__*/null", &safe_json),
        ],
    )
}

/// Replaces every occurrence of each placeholder in `template` in a single pass.
///
/// Only the template itself is searched: substituted values (e.g. the vendored
/// Chart.js bundle or the data JSON) are copied verbatim and never re-scanned, so a
/// placeholder-like string inside a value is not substituted.
fn fill_template(template: &str, replacements: &[(&str, &str)]) -> String {
    let mut matches: Vec<(usize, &str, &str)> = replacements
        .iter()
        .flat_map(|&(placeholder, value)| {
            template
                .match_indices(placeholder)
                .map(move |(pos, _)| (pos, placeholder, value))
        })
        .collect();
    matches.sort_by_key(|&(pos, _, _)| pos);

    let extra: usize = matches.iter().map(|&(_, _, value)| value.len()).sum();
    let mut out = String::with_capacity(template.len() + extra);
    let mut cursor = 0;
    for (pos, placeholder, value) in matches {
        // Skip a match overlapping an already-substituted placeholder.
        if pos < cursor {
            continue;
        }
        out.push_str(&template[cursor..pos]);
        out.push_str(value);
        cursor = pos + placeholder.len();
    }
    out.push_str(&template[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forecast::{Prognosis, Status};
    use crate::model::Sample;
    use chrono::{Duration, TimeZone, Utc};

    fn make_report(
        group: &str,
        bucket_id: &str,
        window: &str,
        samples: Vec<Sample>,
    ) -> BucketReport {
        let t0 = samples.last().map(|s| s.ts).unwrap_or_else(Utc::now);
        let reset = t0 + Duration::hours(5);
        BucketReport {
            group: group.to_string(),
            bucket_id: bucket_id.to_string(),
            window: window.to_string(),
            history: samples.clone(),
            current: samples,
            window_start: Some(t0),
            boundaries: vec![],
            prognosis: Prognosis {
                remaining_now: 0.75,
                reset_time: Some(reset),
                last_sample: t0,
                budget_now: Some(0.8),
                rate_per_h: Some(0.05),
                safe_rate_per_h: Some(0.15),
                at_reset: Some(0.55),
                empty_at: None,
                status: Status::OnTrack,
            },
        }
    }

    #[test]
    fn embeds_reports() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let s = Sample {
            ts: now,
            group: "3p Models".to_string(),
            bucket_id: "3p-5h".to_string(),
            window: "5h".to_string(),
            remaining: 0.8,
            reset_time: Some(now + Duration::hours(5)),
            disabled: false,
        };
        let rep = make_report("3p Models", "3p-5h", "5h", vec![s]);
        let output = render(&[rep], now, 30, ClockFormat::H24);
        assert!(output.contains("\"bucket_id\":\"3p-5h\""));
        assert!(output.contains("\"label\":\"3p Models · 5h\""));
        assert!(output.contains("\"series\":[["));
        assert!(!output.contains("/*__AGY_DATA__*/"));
    }

    #[test]
    fn disambiguates_colliding_bucket_labels_in_dashboard_data() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let s1 = Sample {
            ts: now,
            group: "Gemini Models".to_string(),
            bucket_id: "gemini-weekly".to_string(),
            window: "weekly".to_string(),
            remaining: 0.8,
            reset_time: Some(now + Duration::days(6)),
            disabled: false,
        };
        let s2 = Sample {
            bucket_id: "gemini-argon-weekly".to_string(),
            remaining: 0.9,
            ..s1.clone()
        };
        let s3 = Sample {
            bucket_id: "gemini-5h".to_string(),
            window: "5h".to_string(),
            reset_time: Some(now + Duration::hours(4)),
            ..s1.clone()
        };
        let reports = vec![
            make_report("Gemini Models", "gemini-weekly", "weekly", vec![s1]),
            make_report("Gemini Models", "gemini-argon-weekly", "weekly", vec![s2]),
            make_report("Gemini Models", "gemini-5h", "5h", vec![s3]),
        ];
        let output = render(&reports, now, 30, ClockFormat::H24);
        let val: serde_json::Value = serde_json::from_str(&extract_json(&output)).unwrap();
        let reps = val["reports"].as_array().unwrap();
        assert_eq!(reps[0]["label"], "Gemini Models · Weekly (gemini-weekly)");
        assert_eq!(
            reps[1]["label"],
            "Gemini Models · Weekly (gemini-argon-weekly)"
        );
        // Negative boundary: non-colliding 5h bucket in the same group is not suffixed
        assert_eq!(reps[2]["label"], "Gemini Models · 5h");
        assert!(output.contains("const title = rep.label;"));
    }

    #[test]
    fn history_capped() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let old_sample = Sample {
            ts: now - Duration::days(40),
            group: "Gemini".to_string(),
            bucket_id: "gemini-weekly".to_string(),
            window: "weekly".to_string(),
            remaining: 0.9,
            reset_time: Some(now - Duration::days(33)),
            disabled: false,
        };
        let recent_sample = Sample {
            ts: now - Duration::days(1),
            group: "Gemini".to_string(),
            bucket_id: "gemini-weekly".to_string(),
            window: "weekly".to_string(),
            remaining: 0.85,
            reset_time: Some(now + Duration::days(6)),
            disabled: false,
        };
        let rep = make_report(
            "Gemini",
            "gemini-weekly",
            "weekly",
            vec![old_sample, recent_sample],
        );
        let output = render(&[rep], now, 30, ClockFormat::H24);
        let val: serde_json::Value = serde_json::from_str(&extract_json(&output)).unwrap();
        let series = val["reports"][0]["series"].as_array().unwrap();
        assert_eq!(series.len(), 1);
    }

    #[test]
    fn history_days_max_no_panic() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let s = Sample {
            ts: now,
            group: "Gemini".to_string(),
            bucket_id: "gemini-weekly".to_string(),
            window: "weekly".to_string(),
            remaining: 0.85,
            reset_time: Some(now + Duration::days(6)),
            disabled: false,
        };
        let rep = make_report("Gemini", "gemini-weekly", "weekly", vec![s]);
        let output = render(&[rep], now, u32::MAX, ClockFormat::H24);
        assert!(output.contains("\"bucket_id\":\"gemini-weekly\""));
    }

    #[test]
    fn escapes_script_close() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let s = Sample {
            ts: now,
            group: "x</script><b>".to_string(),
            bucket_id: "b1".to_string(),
            window: "5h".to_string(),
            remaining: 0.8,
            reset_time: Some(now + Duration::hours(5)),
            disabled: false,
        };
        let rep = make_report("x</script><b>", "b1", "5h", vec![s]);
        let output = render(&[rep], now, 30, ClockFormat::H24);
        assert!(output.contains(r#"\u003c/script>"#));
        let count = output.matches("</script>").count();
        let empty_output = render(&[], now, 30, ClockFormat::H24);
        assert_eq!(count, empty_output.matches("</script>").count());
    }

    #[test]
    fn escapes_dom_injection() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let output = render(&[], now, 30, ClockFormat::H24);
        assert!(output.contains("function escapeHtml("));
        assert!(output.contains("escapeHtml(title)"));
    }

    #[test]
    fn empty_state() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let output = render(&[], now, 30, ClockFormat::H24);
        assert!(output.contains("\"reports\":[]"));
        assert!(output.contains(
            "No samples yet — run \"agy-usage install-timer\" or \"agy-usage collect\"."
        ));
    }

    #[test]
    fn offline_chart_guard_present() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let output = render(&[], now, 30, ClockFormat::H24);
        assert!(output.contains("typeof Chart !== 'undefined'"));
        assert!(output.contains("Charts unavailable offline (Chart.js CDN unreached)"));
        assert!(output.contains("rep.reset_time"));
    }

    #[test]
    fn demo_html() {
        let fixture = include_str!("../tests/fixtures/usage.json");
        let ts = Utc.with_ymd_and_hms(2026, 10, 11, 17, 20, 0).unwrap();
        let samples = crate::source::parse_usage(fixture, ts).unwrap();
        let reports = crate::forecast::analyze(&samples, 0.10);
        let html = render(&reports, ts, 30, ClockFormat::H24);
        std::fs::create_dir_all("target").unwrap();
        std::fs::write("target/demo.html", html).unwrap();
    }

    #[test]
    fn chartjs_tooltips_and_decimation_config() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let output = render(&[], now, 30, ClockFormat::H24);
        assert!(output.contains("parsing: false"));
        assert!(output.contains("mode: 'nearest'"));
        assert!(output.contains("axis: 'x'"));
        assert!(output.contains("tension: 0"));
        assert!(output.contains("borderJoinStyle: 'round'"));
        assert!(output.contains("borderCapStyle: 'round'"));
        assert!(output.contains("decimation:"));
        assert!(output.contains("algorithm: 'min-max'"));
        assert!(output.contains("toFixed(1)"));
        assert!(output.contains("const actualPointRadius = pts => (pts.length === 1 ? 2 : 0);"));
        assert!(output.contains("pointRadius: actualPointRadius(initialActualPts)"));
        assert!(output.contains("pointHoverRadius: 4"));
        assert!(output.contains("emptyAt"));
        assert!(output.contains("❌ now"));
        assert!(output.contains("updateRelativeTimes"));
        assert!(output.contains("setInterval(updateRelativeTimes, 30000)"));
        assert!(output.contains("statusLabel(p.status, p.empty_at, Date.now())"));
    }

    #[test]
    fn range_toggle_and_window_aware_defaults() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let s_5h = Sample {
            ts: now,
            group: "3p Models".to_string(),
            bucket_id: "3p-5h".to_string(),
            window: "5h".to_string(),
            remaining: 0.8,
            reset_time: Some(now + Duration::hours(5)),
            disabled: false,
        };
        let rep_5h = make_report("3p Models", "3p-5h", "5h", vec![s_5h]);
        let s_daily = Sample {
            ts: now,
            group: "Gemini".to_string(),
            bucket_id: "gemini-daily".to_string(),
            window: "daily".to_string(),
            remaining: 0.9,
            reset_time: Some(now + Duration::hours(20)),
            disabled: false,
        };
        let rep_daily = make_report("Gemini", "gemini-daily", "daily", vec![s_daily]);
        let s_weekly = Sample {
            ts: now,
            group: "Gemini".to_string(),
            bucket_id: "gemini-weekly".to_string(),
            window: "weekly".to_string(),
            remaining: 0.85,
            reset_time: Some(now + Duration::days(6)),
            disabled: false,
        };
        let rep_weekly = make_report("Gemini", "gemini-weekly", "weekly", vec![s_weekly]);
        let output = render(&[rep_5h, rep_daily, rep_weekly], now, 30, ClockFormat::H24);
        let val: serde_json::Value = serde_json::from_str(&extract_json(&output)).unwrap();
        let reps = val["reports"].as_array().unwrap();
        assert_eq!(reps[0]["window_secs"], 18_000);
        assert_eq!(reps[1]["window_secs"], 86_400);
        assert_eq!(reps[1]["label"], "Gemini · Daily");
        assert_eq!(reps[2]["window_secs"], 604_800);

        // UI markup & styling
        assert!(output.contains(".range-toggle"));
        assert!(output.contains(".chart-card:last-child:nth-child(odd)"));
        assert!(output.contains("grid-column: 1 / -1"));
        assert!(output.contains("min-width: 0;"));
        assert!(output.contains(".range-btn"));
        assert!(output.contains(".range-btn:focus-visible"));
        assert!(output.contains("class=\"range-toggle\""));

        // Accessibility attributes
        assert!(output.contains("role=\"group\""));
        assert!(output.contains("aria-label=\"Time range\""));
        assert!(output.contains("aria-pressed=\"true\""));
        assert!(output.contains("aria-pressed=\"false\""));
        assert!(output.contains("b.setAttribute('aria-pressed', isActive ? 'true' : 'false')"));

        // Negative boundary: disabled buckets (and offline fallback without Chart.js) suppress range toggle controls
        assert!(output.contains("const rangeHtml = (!hasCharts || isDis) ? '' :"));

        // Boundary plugin checks display property and chartArea existence
        assert!(output.contains("boundaries?.display === false"));
        assert!(output.contains("!chart.chartArea || !chart.scales?.x"));

        // Consistent 5h / fixed-range viewport bounds with lastSampleMs clamp and shared applyCardRange helper
        assert!(output.contains("function applyCardRange(cardIdx, range)"));
        assert!(output.contains("'5h': { spanMs: 5 * 3600 * 1000, boundaries: () => true }"));
        assert!(output.contains("'24h': { spanMs: 24 * 3600 * 1000, boundaries: () => true }"));
        assert!(
            output.contains("'7d': { spanMs: 7 * 86400 * 1000, boundaries: isShort => !isShort }")
        );
        assert!(
            output
                .contains("'30d': { spanMs: 30 * 86400 * 1000, boundaries: isShort => !isShort }")
        );

        // Boundary restoration on 7d/30d for non-short cards
        assert!(
            output.contains("const isShort = rep.window_secs != null && rep.window_secs < 86400;")
        );
        assert!(output.contains("function computeRangeBounds(state, range)"));
        assert!(
            output.contains("chart.options.plugins.boundaries.display = bounds.boundariesDisplay;")
        );

        // Window-aware defaults in chart initialization: initial viewport uses computeRangeBounds (matching 5h/7d toggle)
        assert!(output.contains("\"window_secs\":18000"));
        assert!(output.contains("\"window_secs\":86400"));
        assert!(output.contains("\"window_secs\":604800"));
        assert!(output.contains("const initialBounds = computeRangeBounds({"));
        assert!(output.contains("}, initialRange);"));
        assert!(!output.contains("const winMs ="));
        assert!(output.contains("const initialMin = initialBounds.minX;"));
        assert!(output.contains("const initialMax = initialBounds.maxX;"));
        assert!(
            output.contains("const initialActualPts = sliceSeriesFrom(allActualPts, initialMin);")
        );
        assert!(output.contains("const visiblePts = sliceSeriesFrom(allActualPts, minX);"));
        assert!(output.contains("updateDatasetPoints(actualDataset, visiblePts);"));
        assert!(output.contains("actualDataset.pointRadius = actualPointRadius(visiblePts);"));
        assert!(output.contains("min: initialMin"));
        assert!(output.contains("max: initialMax"));
        assert!(output.contains("chart.options.scales.x.min"));
        assert!(output.contains("chart.options.scales.x.max"));
        assert!(output.contains("chart.update('none')"));

        // Unified card range buttons on every card with window-aware active defaults (5h for short, 7d for long/weekly)
        let (_, range_section) = output
            .split_once("const rangeHtml = (!hasCharts || isDis) ? '' : (isShort ? `")
            .expect("rangeHtml section found");
        let (h_5h, h_weekly) = range_section
            .split_once("` : `")
            .expect("both template branches present");
        let (h_weekly, _) = h_weekly
            .split_once("`);")
            .expect("end of rangeHtml template");
        for range in ["window", "5h", "24h", "7d", "30d", "all"] {
            let attr = format!("data-range=\"{range}\"");
            assert!(h_5h.contains(&attr), "short card missing unified {attr}");
            assert!(
                h_weekly.contains(&attr),
                "weekly card missing unified {attr}"
            );
        }
        assert!(h_5h.contains("class=\"range-btn active\" data-range=\"5h\""));
        for other in ["window", "24h", "7d", "30d", "all"] {
            let active_attr = format!("class=\"range-btn active\" data-range=\"{other}\"");
            assert!(
                !h_5h.contains(&active_attr),
                "short card unexpectedly activated {other}"
            );
        }
        assert!(h_weekly.contains("class=\"range-btn active\" data-range=\"7d\""));
        for other in ["window", "5h", "24h", "30d", "all"] {
            let active_attr = format!("class=\"range-btn active\" data-range=\"{other}\"");
            assert!(
                !h_weekly.contains(&active_attr),
                "weekly card unexpectedly activated {other}"
            );
        }
    }

    #[test]
    fn serializes_will_run_out_prognosis_with_empty_at() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let empty = now + Duration::hours(3);
        let mut rep = make_report("Gemini", "gemini-weekly", "weekly", vec![]);
        rep.prognosis.status = Status::WillRunOut;
        rep.prognosis.empty_at = Some(empty);
        let output = render(&[rep], now, 30, ClockFormat::H24);

        assert!(output.contains("\"status\":\"WillRunOut\""));
        assert!(output.contains("\"empty_at\":\"2026-10-11T15:00:00Z\""));
    }

    #[test]
    fn renders_with_inlined_scripts_zero_external_cdns() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let rep = make_report("G", "b1", "weekly", vec![]);
        let html = render(&[rep], now, 30, ClockFormat::H24);

        assert!(!html.contains("cdn.jsdelivr.net"));
        assert!(!html.contains("<script src="));
        assert!(html.contains("Chart.register"));
        assert!(html.contains(CHART_JS));
        assert!(html.contains(DATE_FNS_ADAPTER));
    }

    #[test]
    fn vendored_scripts_contain_no_script_close_tag() {
        assert!(!CHART_JS.to_lowercase().contains("</script"));
        assert!(!DATE_FNS_ADAPTER.to_lowercase().contains("</script"));
    }

    #[test]
    fn data_containing_placeholder_does_not_expand_recursively() {
        let s = Sample {
            ts: Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap(),
            group: "/*__CHART_JS__*/".to_string(),
            bucket_id: "b1".to_string(),
            window: "weekly".to_string(),
            remaining: 0.9,
            reset_time: Some(Utc.with_ymd_and_hms(2026, 10, 16, 12, 0, 0).unwrap()),
            disabled: false,
        };
        let rep = make_report("/*__CHART_JS__*/", "b1", "weekly", vec![s]);
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 17, 5, 0).unwrap();
        let html = render(&[rep], now, 30, ClockFormat::H24);

        let first_idx = html.find(CHART_JS).expect("CHART_JS should be present");
        let second_idx = html[first_idx + CHART_JS.len()..].find(CHART_JS);
        assert!(second_idx.is_none(), "CHART_JS was expanded multiple times");
    }

    #[test]
    fn renders_disabled_bucket() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let s = Sample {
            ts: now,
            group: "Claude and GPT models".to_string(),
            bucket_id: "3p-5h".to_string(),
            window: "5h".to_string(),
            remaining: 1.0,
            reset_time: None,
            disabled: true,
        };
        let mut rep = make_report("Claude and GPT models", "3p-5h", "5h", vec![s]);
        rep.prognosis.status = Status::Disabled;
        rep.prognosis.reset_time = None;
        rep.window_start = None;

        let output = render(&[rep], now, 30, ClockFormat::H24);
        assert!(output.contains("\"status\":\"Disabled\""));
        assert!(output.contains("Weekly limit reached"));
        assert!(output.contains("badge-disabled"));
    }

    #[test]
    fn embeds_detected_clock_format() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let h12 = render(&[], now, 30, ClockFormat::H12);
        let v12: serde_json::Value = serde_json::from_str(&extract_json(&h12)).unwrap();
        assert_eq!(v12["clock"], "12h");
        let h24 = render(&[], now, 30, ClockFormat::H24);
        let v24: serde_json::Value = serde_json::from_str(&extract_json(&h24)).unwrap();
        assert_eq!(v24["clock"], "24h");
    }

    #[test]
    fn dashboard_time_displays_follow_clock_format() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let html = render(&[], now, 30, ClockFormat::H24);
        // Browser-locale formatting must be pinned to the detected hour cycle,
        // otherwise e.g. an en-US browser shows AM/PM on a 24h system.
        assert!(html.contains("hourCycle: USE_12H ? 'h12' : 'h23'"));
        assert!(html.contains("genDate.toLocaleString(undefined, TIME_OPTS)"));
        assert!(html.contains("lastDate.toLocaleTimeString(undefined, TIME_OPTS)"));
        assert!(!TEMPLATE.contains("toLocaleString()"));
        assert!(!TEMPLATE.contains("toLocaleTimeString()"));
        // Chart axis + tooltip must override every date-fns time-of-day default
        // (which are 12h "h:mm aaaa").
        assert!(html.contains("time: chartTimeOptions()"));
        for fmt in [
            "millisecond: 'HH:mm:ss.SSS'",
            "second: 'HH:mm:ss'",
            "minute: 'HH:mm'",
            "hour: 'HH:mm'",
            "tooltipFormat: 'MMM d, yyyy, HH:mm:ss'",
            "millisecond: 'h:mm:ss.SSS a'",
            "second: 'h:mm:ss a'",
            "minute: 'h:mm a'",
            "hour: 'h a'",
            "tooltipFormat: 'MMM d, yyyy, h:mm:ss a'",
        ] {
            assert!(TEMPLATE.contains(fmt), "missing chart time format {fmt}");
        }
    }

    #[test]
    fn includes_zero_rate_projections_and_guards_degenerate_segments() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let s1 = Sample {
            ts: now - Duration::minutes(30),
            group: "Claude and GPT models".to_string(),
            bucket_id: "3p-weekly".to_string(),
            window: "weekly".to_string(),
            remaining: 0.70,
            reset_time: Some(now + Duration::days(5)),
            disabled: false,
        };
        let s2 = Sample {
            ts: now,
            remaining: 0.70,
            ..s1.clone()
        };
        let reports = crate::forecast::analyze(&[s1, s2], 0.10);
        assert_eq!(reports[0].prognosis.rate_per_h, Some(0.0));
        assert_eq!(reports[0].prognosis.at_reset, Some(0.70));

        let output = render(&reports, now, 30, ClockFormat::H24);
        let val: serde_json::Value = serde_json::from_str(&extract_json(&output)).unwrap();
        let prog = &val["reports"][0]["prognosis"];
        assert_eq!(prog["rate_per_h"], 0.0);
        assert_eq!(prog["at_reset"], 0.7);
        assert!(prog["empty_at"].is_null());
        assert_eq!(prog["status"], "OnTrack");

        // Verify dashboard JS includes zero-rate projections (rate_per_h >= 0) and guards against 0-length segments
        assert!(output.contains("if (p.rate_per_h != null && p.rate_per_h >= 0)"));
        assert!(output.contains("if (endX > startX)"));
        assert!(!output.contains("p.rate_per_h > 0"));
    }

    #[test]
    fn dashboard_renders_and_refreshes_reset_countdown_and_clock_time() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let output = render(&[], now, 30, ClockFormat::H24);

        // Table header includes Resets column between At reset and Status
        assert!(
            output.contains("<th>At reset</th>\n                        <th>Resets</th>\n                        <th>Status</th>"),
            "missing Resets table header column"
        );

        // Helpers for relative countdown and clock-format-aware reset cell
        assert!(output.contains("function formatTimeUntil("));
        assert!(output.contains("function formatResetCell("));
        assert!(output.contains("rDate.toLocaleTimeString(undefined, TIME_OPTS)"));

        // Negative boundary: disabled or missing reset_time returns '—'
        assert!(output.contains("if (isDisabled || !resetTime) return '—';"));

        // Initial row rendering & periodic refresh in updateRelativeTimes()
        assert!(
            output
                .contains("const resetsStr = formatResetCell(rep.reset_time, isDis, Date.now());")
        );
        assert!(output.contains("tr.cells[5].textContent = formatResetCell(rep.reset_time, p.status === 'Disabled', now);"));
        assert!(
            output.contains("tr.cells[6].textContent = statusLabel(p.status, p.empty_at, now);")
        );
        assert!(output.contains("tr.cells[7].textContent = `${lDate.toLocaleTimeString(undefined, TIME_OPTS)} (${formatTimeAgo(lDate)})`;"));
    }

    fn extract_json(html: &str) -> String {
        let prefix = "const data = ";
        let start = html.find(prefix).expect("prefix not found") + prefix.len();
        let end = html[start..].find(";\n").expect("semicolon not found") + start;
        html[start..end].to_string()
    }

    #[test]
    fn fill_template_matches_sequential_replace_for_dashboard() {
        let data = r#"{"generated_at":"x","reports":[]}"#;
        let expected = TEMPLATE
            .replace("/*__CHART_JS__*/", CHART_JS)
            .replace("/*__DATE_FNS_ADAPTER__*/", DATE_FNS_ADAPTER)
            .replace("/*__AGY_DATA__*/null", data);
        let actual = fill_template(
            TEMPLATE,
            &[
                ("/*__CHART_JS__*/", CHART_JS),
                ("/*__DATE_FNS_ADAPTER__*/", DATE_FNS_ADAPTER),
                ("/*__AGY_DATA__*/null", data),
            ],
        );
        assert_eq!(actual, expected);
        assert!(!actual.contains("/*__AGY_DATA__*/"));
    }

    #[test]
    fn fill_template_substitutes_all_occurrences_without_rescanning_values() {
        let out = fill_template(
            "a{X}b{Y}c{X}d",
            &[("{X}", "<{Y}>"), ("{Y}", "y"), ("{absent}", "nope")],
        );
        // `{Y}` inside the value of `{X}` is copied verbatim, not substituted.
        assert_eq!(out, "a<{Y}>byc<{Y}>d");
        assert!(!out.contains("nope"));
        assert_eq!(fill_template("plain", &[("{X}", "v")]), "plain");
        assert_eq!(fill_template("", &[("{X}", "v")]), "");
    }

    #[test]
    fn fill_template_skips_overlapping_placeholders() {
        // "{AB}" and "B}" overlap; the earlier match wins and the overlapped one is
        // ignored instead of producing out-of-bounds or duplicated output.
        let out = fill_template("x{AB}y", &[("{AB}", "1"), ("B}", "2")]);
        assert_eq!(out, "x1y");
    }

    #[test]
    fn boundaries_are_limited_to_displayed_history() {
        let now = Utc.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap();
        let cutoff = now - Duration::days(7);
        let mut rep = make_report("G", "b1", "weekly", vec![]);
        // Deliberately unsorted to show no ordering is assumed.
        rep.boundaries = vec![
            now - Duration::days(1),
            cutoff - Duration::seconds(1),
            cutoff,
            now - Duration::days(21),
        ];
        let html = render(&[rep], now, 7, ClockFormat::H24);
        let data: serde_json::Value = serde_json::from_str(&extract_json(&html)).unwrap();
        let got: Vec<DateTime<Utc>> =
            serde_json::from_value(data["reports"][0]["boundaries"].clone()).unwrap();
        assert_eq!(got, vec![now - Duration::days(1), cutoff]);
    }

    #[test]
    fn crosshair_sync_plugin_and_cleanup_on_range_toggle() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let output = render(&[], now, 30, ClockFormat::H24);

        // Plugin registration and binary search helper
        assert!(output.contains("const crosshairSyncPlugin = {"));
        assert!(output.contains("id: 'crosshairSync'"));
        assert!(output.contains("Chart.register(boundaryPlugin, crosshairSyncPlugin)"));
        assert!(output.contains("function findNearestActualIndex(pts, targetMs)"));
        assert!(output.contains("let isSyncingCrosshair = false;"));
        assert!(output.contains("const chartInstances = [];"));
        assert!(output.contains("chartInstances.push(chart);"));

        // Source chart preserves native tooltip and only marks args.changed = true
        assert!(output.contains("chart.$crosshairTimeMs = targetMs;"));
        assert!(output.contains("args.changed = true;"));

        // Sibling chart checks resolved scale bounds, decimated meta element coordinates, and snap tolerance
        assert!(output.contains("targetMs < xScale.min || targetMs > xScale.max"));
        assert!(output.contains("Math.min(15 * 60 * 1000, (xScale.max - xScale.min) * 0.05)"));
        assert!(output.contains("sibling.getDatasetMeta(actualIdx)"));
        assert!(output.contains("sibling.setActiveElements(activeElements)"));
        assert!(output.contains("sibling.tooltip?.setActiveElements(activeElements"));

        // Range toggle clears any active synced crosshair
        assert!(output.contains("clearSyncedCrosshair(null);"));
    }

    #[test]
    fn compress_series_handles_empty_single_and_rounding() {
        assert_eq!(compress_series(&[]), Vec::<(i64, f64)>::new());

        let single = compress_series(&[(1_000, -0.00001)]);
        assert_eq!(single, vec![(1_000, 0.0)]);
        assert!(single[0].1.is_sign_positive());

        let two = compress_series(&[(1_000, 0.12344), (2_000, 0.12345)]);
        assert_eq!(two, vec![(1_000, 0.1234), (2_000, 0.1235)]);
    }

    #[test]
    fn compress_series_preserves_plateau_edges_and_15m_heartbeats() {
        // 45 minutes of 1-minute samples at 1.0 (with tiny rounding noise), followed by a ramp down.
        let mut pts = Vec::new();
        for m in 0..=45 {
            let noise = if m % 2 == 0 { 0.00002 } else { -0.00002 };
            pts.push((m * 60_000, 1.0 + noise));
        }
        pts.push((46 * 60_000, 0.85));
        pts.push((47 * 60_000, 0.85));
        pts.push((48 * 60_000, 0.85));
        pts.push((49 * 60_000, 0.70));

        let compressed = compress_series(&pts);
        // Must preserve:
        // - 0m (series start & plateau start)
        // - 15m, 30m (15m heartbeats along long plateau)
        // - 45m (final point of 1.0 plateau right before drop at 46m!)
        // - 46m (drop & start of 0.85 plateau)
        // - 48m (final point of 0.85 plateau right before drop at 49m!)
        // - 49m (series end)
        assert_eq!(
            compressed,
            vec![
                (0, 1.0),
                (15 * 60_000, 1.0),
                (30 * 60_000, 1.0),
                (45 * 60_000, 1.0),
                (46 * 60_000, 0.85),
                (48 * 60_000, 0.85),
                (49 * 60_000, 0.7),
            ]
        );
    }

    #[test]
    fn compress_series_keeps_gap_within_15m_for_4m_cadence() {
        // 4-minute sampling cadence over 40 minutes on a flat plateau:
        // heartbeat must ensure consecutive points stay <= 900_000 ms apart.
        let pts: Vec<(i64, f64)> = (0..=10).map(|i| (i * 4 * 60_000, 0.9)).collect();
        let compressed = compress_series(&pts);
        assert_eq!(compressed.first().copied(), Some((0, 0.9)));
        assert_eq!(compressed.last().copied(), Some((40 * 60_000, 0.9)));
        assert!(compressed.len() < pts.len());
        for w in compressed.windows(2) {
            assert!(
                w[1].0 - w[0].0 <= 900_000,
                "gap {} exceeded 15m heartbeat",
                w[1].0 - w[0].0
            );
        }
    }

    #[test]
    fn render_embeds_compressed_series() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let start = now - Duration::minutes(40);
        let samples: Vec<Sample> = (0..=40)
            .map(|m| Sample {
                ts: start + Duration::minutes(m),
                group: "Gemini".to_string(),
                bucket_id: "gemini-5h".to_string(),
                window: "5h".to_string(),
                remaining: 1.0,
                reset_time: Some(now + Duration::hours(4)),
                disabled: false,
            })
            .collect();
        let rep = make_report("Gemini", "gemini-5h", "5h", samples);
        let html = render(&[rep], now, 30, ClockFormat::H24);
        let val: serde_json::Value = serde_json::from_str(&extract_json(&html)).unwrap();
        let series = val["reports"][0]["series"].as_array().unwrap();
        assert_eq!(series.len(), 4); // 0m, 15m, 30m, 40m
    }

    #[test]
    fn write_atomic_creates_parents_overwrites_and_cleans_up_on_error() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("nested").join("dir").join("dashboard.html");

        write_atomic(&nested, "<html>v1</html>").unwrap();
        assert_eq!(std::fs::read_to_string(&nested).unwrap(), "<html>v1</html>");
        assert_eq!(
            std::fs::read_dir(nested.parent().unwrap()).unwrap().count(),
            1
        );

        // Clean overwrite
        write_atomic(&nested, "<html>v2</html>").unwrap();
        assert_eq!(std::fs::read_to_string(&nested).unwrap(), "<html>v2</html>");
        assert_eq!(
            std::fs::read_dir(nested.parent().unwrap()).unwrap().count(),
            1
        );

        // Negative boundary: persisting over an existing directory fails and removes temp file
        let err_dir = tmp.path().join("existing_dir");
        std::fs::create_dir(&err_dir).unwrap();
        assert!(write_atomic(&err_dir, "<html>fail</html>").is_err());
        assert_eq!(std::fs::read_dir(&err_dir).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 2); // nested + existing_dir only

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o644)).unwrap();
            write_atomic(&nested, "<html>v3</html>").unwrap();
            let mode = std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o644,
                "write_atomic should preserve existing file mode"
            );
        }
    }

    #[test]
    fn dashboard_includes_global_range_auto_reload_theme_and_slice_helpers() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let output = render(&[], now, 30, ClockFormat::H24);

        // Demo/screenshot template invariants must stay intact
        assert_eq!(
            output.matches("<title>agy-usage dashboard</title>").count(),
            1
        );
        assert_eq!(output.matches("<body>").count(), 1);
        assert!(output.contains("const data = "));
        assert!(output.contains("maintainAspectRatio: false,"));
        assert!(output.contains("transition: color 0.15s ease, background 0.15s ease;"));

        // Global range selector bar in header
        assert!(output.contains("id=\"global-range-toggle\""));
        assert!(output.contains("aria-label=\"Global time range\""));
        assert!(output.contains("function syncGlobalRangeButtons()"));

        // Left-edge interpolation & decimation cleanup
        assert!(output.contains("function sliceSeriesFrom(pts, cutoffMs)"));
        assert!(output.contains("function updateDatasetPoints(ds, nextPts)"));
        assert!(output.contains("delete ds._decimated;"));
        assert!(output.contains("delete ds._data;"));

        // Opt-in Auto 60s reload control with visibilityState & try/catch localStorage persistence
        assert!(output.contains("id=\"auto-reload-btn\""));
        assert!(output.contains("Auto 60s"));
        assert!(output.contains("document.visibilityState === 'visible'"));
        assert!(output.contains("visibilitychange"));
        assert!(output.contains("window.localStorage.getItem"));
        assert!(output.contains("window.localStorage.setItem"));
        assert!(output.contains("AUTO_RELOAD_MS = 60000"));

        // Dark-default + light-mode theme toggle on documentElement + dynamic Chart.js colors
        assert!(output.contains(":root {"));
        assert!(output.contains("[data-theme=\"light\"]"));
        assert!(output.contains("document.documentElement.setAttribute('data-theme', 'light')"));
        assert!(output.contains("document.documentElement.removeAttribute('data-theme')"));
        assert!(output.contains("id=\"theme-toggle-btn\""));
        assert!(output.contains("function getThemeColors()"));
        assert!(output.contains("getThemeColors().boundary"));
        assert!(output.contains("getThemeColors().crosshair"));
        assert!(output.contains("backgroundColor: themeColors.tooltipBg"));
    }
}
