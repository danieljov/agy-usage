use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::clock::ClockFormat;
use crate::forecast::{BucketReport, Prognosis};

const TEMPLATE: &str = include_str!("../assets/dashboard.html");
const CHART_JS: &str = include_str!("../assets/vendor/chart.umd.min.js");
const DATE_FNS_ADAPTER: &str =
    include_str!("../assets/vendor/chartjs-adapter-date-fns.bundle.min.js");

#[derive(Serialize)]
struct CompactReport<'a> {
    group: &'a str,
    bucket_id: &'a str,
    window: &'a str,
    window_start: Option<DateTime<Utc>>,
    reset_time: Option<DateTime<Utc>>,
    boundaries: &'a [DateTime<Utc>],
    prognosis: &'a Prognosis,
    series: Vec<(i64, f64)>,
}

#[derive(Serialize)]
struct DashboardData<'a> {
    generated_at: DateTime<Utc>,
    clock: ClockFormat,
    reports: Vec<CompactReport<'a>>,
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
            let series = r
                .history
                .iter()
                .filter(|s| s.ts >= cutoff)
                .map(|s| (s.ts.timestamp_millis(), s.remaining))
                .collect();
            CompactReport {
                group: &r.group,
                bucket_id: &r.bucket_id,
                window: &r.window,
                window_start: r.window_start,
                reset_time: r.prognosis.reset_time,
                boundaries: &r.boundaries,
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
    TEMPLATE
        .replace("/*__CHART_JS__*/", CHART_JS)
        .replace("/*__DATE_FNS_ADAPTER__*/", DATE_FNS_ADAPTER)
        .replace("/*__AGY_DATA__*/null", &safe_json)
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
        assert!(output.contains("\"series\":[["));
        assert!(!output.contains("/*__AGY_DATA__*/"));
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
        assert!(output.contains("decimation:"));
        assert!(output.contains("algorithm: 'min-max'"));
        assert!(output.contains("toFixed(1)"));
        assert!(output.contains("rep.series.length === 1 ? 2"));
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
        let output = render(&[rep_5h, rep_weekly], now, 30, ClockFormat::H24);

        // UI markup & styling
        assert!(output.contains(".range-toggle"));
        assert!(output.contains(".range-btn"));
        assert!(output.contains(".range-btn:focus-visible"));
        assert!(output.contains("class=\"range-toggle\""));

        // Accessibility attributes
        assert!(output.contains("role=\"group\""));
        assert!(output.contains("aria-label=\"Time range\""));
        assert!(output.contains("aria-pressed=\"true\""));
        assert!(output.contains("aria-pressed=\"false\""));
        assert!(output.contains("b.setAttribute('aria-pressed', isActive ? 'true' : 'false')"));

        // Negative boundary: disabled buckets suppress range toggle controls
        assert!(output.contains("const rangeHtml = isDis ? '' :"));

        // Boundary plugin checks display property and chartArea existence
        assert!(output.contains("boundaries?.display === false"));
        assert!(output.contains("!chart.chartArea || !chart.scales?.x"));

        // Consistent 5h viewport bounds
        assert!(output.contains("chart.options.scales.x.min = endMs - 5 * 3600 * 1000;"));

        // Window-aware defaults in chart initialization
        assert!(output.contains("min: initialMin"));
        assert!(output.contains("max: endMs"));
        assert!(output.contains("chart.options.scales.x.min"));
        assert!(output.contains("chart.options.scales.x.max"));
        assert!(output.contains("chart.update('none')"));

        // Template exclusivity: 5h template has 5h/24h default; weekly has 30d default
        let (_, range_section) = output
            .split_once("const rangeHtml = isDis ? '' : (is5h ? `")
            .expect("rangeHtml section found");
        let (h_5h, h_weekly) = range_section
            .split_once("` : `")
            .expect("both template branches present");
        assert!(h_5h.contains("data-range=\"5h\""));
        assert!(h_5h.contains("class=\"range-btn active\" data-range=\"24h\""));
        assert!(!h_5h.contains("data-range=\"all\""));
        assert!(h_weekly.contains("class=\"range-btn active\" data-range=\"30d\""));
        assert!(h_weekly.contains("data-range=\"all\""));
        assert!(!h_weekly.contains("data-range=\"5h\""));
        assert!(!h_weekly.contains("data-range=\"24h\""));
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

    fn extract_json(html: &str) -> String {
        let prefix = "const data = ";
        let start = html.find(prefix).expect("prefix not found") + prefix.len();
        let end = html[start..].find(";\n").expect("semicolon not found") + start;
        html[start..end].to_string()
    }
}
