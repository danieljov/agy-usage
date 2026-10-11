use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::forecast::{BucketReport, Prognosis};

const TEMPLATE: &str = include_str!("../assets/dashboard.html");

#[derive(Serialize)]
struct CompactReport<'a> {
    group: &'a str,
    bucket_id: &'a str,
    window: &'a str,
    window_start: Option<DateTime<Utc>>,
    reset_time: DateTime<Utc>,
    boundaries: &'a [DateTime<Utc>],
    prognosis: &'a Prognosis,
    series: Vec<(i64, f64)>,
}

#[derive(Serialize)]
struct DashboardData<'a> {
    generated_at: DateTime<Utc>,
    reports: Vec<CompactReport<'a>>,
}

pub fn render(reports: &[BucketReport], generated_at: DateTime<Utc>, history_days: u32) -> String {
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
        reports: compact_reports,
    };

    let json = serde_json::to_string(&data).expect("failed to serialize dashboard data");
    let safe_json = json.replace('<', "\\u003c");
    TEMPLATE.replace("/*__AGY_DATA__*/null", &safe_json)
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
                reset_time: reset,
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
            reset_time: now + Duration::hours(5),
        };
        let rep = make_report("3p Models", "3p-5h", "5h", vec![s]);
        let output = render(&[rep], now, 30);
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
            reset_time: now - Duration::days(33),
        };
        let recent_sample = Sample {
            ts: now - Duration::days(1),
            group: "Gemini".to_string(),
            bucket_id: "gemini-weekly".to_string(),
            window: "weekly".to_string(),
            remaining: 0.85,
            reset_time: now + Duration::days(6),
        };
        let rep = make_report(
            "Gemini",
            "gemini-weekly",
            "weekly",
            vec![old_sample, recent_sample],
        );
        let output = render(&[rep], now, 30);
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
            reset_time: now + Duration::days(6),
        };
        let rep = make_report("Gemini", "gemini-weekly", "weekly", vec![s]);
        let output = render(&[rep], now, u32::MAX);
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
            reset_time: now + Duration::hours(5),
        };
        let rep = make_report("x</script><b>", "b1", "5h", vec![s]);
        let output = render(&[rep], now, 30);
        assert!(output.contains(r#"\u003c/script>"#));
        let count = output.matches("</script>").count();
        let empty_output = render(&[], now, 30);
        assert_eq!(count, empty_output.matches("</script>").count());
    }

    #[test]
    fn escapes_dom_injection() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let output = render(&[], now, 30);
        assert!(output.contains("function escapeHtml("));
        assert!(output.contains("escapeHtml(title)"));
    }

    #[test]
    fn empty_state() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let output = render(&[], now, 30);
        assert!(output.contains("\"reports\":[]"));
        assert!(output.contains(
            "No samples yet — run \"agy-usage install-timer\" or \"agy-usage collect\"."
        ));
    }

    #[test]
    fn offline_chart_guard_present() {
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let output = render(&[], now, 30);
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
        let html = render(&reports, ts, 30);
        std::fs::create_dir_all("target").unwrap();
        std::fs::write("target/demo.html", html).unwrap();
    }

    fn extract_json(html: &str) -> String {
        let prefix = "const data = ";
        let start = html.find(prefix).expect("prefix not found") + prefix.len();
        let end = html[start..].find(";\n").expect("semicolon not found") + start;
        html[start..end].to_string()
    }
}
