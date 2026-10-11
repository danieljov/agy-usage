use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{Sample, lookback, window_length};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Collecting,
    WillRunOut,
    Tight,
    OnTrack,
}

impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Status::Collecting => "⏳ collecting",
            Status::WillRunOut => "❌ will run out",
            Status::Tight => "⚠️ tight",
            Status::OnTrack => "✅ on track",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prognosis {
    pub remaining_now: f64,
    pub reset_time: DateTime<Utc>,
    pub last_sample: DateTime<Utc>,
    pub budget_now: Option<f64>,
    pub rate_per_h: Option<f64>,
    pub safe_rate_per_h: Option<f64>,
    pub at_reset: Option<f64>,
    pub empty_at: Option<DateTime<Utc>>,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BucketReport {
    pub group: String,
    pub bucket_id: String,
    pub window: String,
    pub history: Vec<Sample>,
    pub current: Vec<Sample>,
    pub window_start: Option<DateTime<Utc>>,
    pub boundaries: Vec<DateTime<Utc>>,
    pub prognosis: Prognosis,
}

pub fn window_boundaries(samples: &[Sample]) -> Vec<usize> {
    let mut boundaries = Vec::new();
    for i in 1..samples.len() {
        let prev = &samples[i - 1];
        let curr = &samples[i];
        let quota_increased = curr.remaining > prev.remaining + 0.005;
        let reset_moved = curr.reset_time > prev.reset_time + Duration::seconds(60);
        let reset_expired = prev.reset_time <= curr.ts;
        if quota_increased || (reset_moved && reset_expired) {
            boundaries.push(i);
        }
    }
    boundaries
}

pub fn prognose(current: &[Sample], tight: f64) -> Prognosis {
    let last = current.last().expect("current samples must not be empty");
    let t0 = last.ts;
    let r0 = last.remaining;
    let end = last.reset_time;
    let l_dur = window_length(&last.window);
    let lb_dur = lookback(&last.window);

    let hours_left = (end - t0).num_milliseconds() as f64 / 3_600_000.0;
    if hours_left <= 0.0 {
        return Prognosis {
            remaining_now: r0,
            reset_time: end,
            last_sample: t0,
            budget_now: None,
            rate_per_h: None,
            safe_rate_per_h: None,
            at_reset: None,
            empty_at: None,
            status: Status::Collecting,
        };
    }

    let budget_now = l_dur.map(|dur| {
        let l_hours = dur.num_milliseconds() as f64 / 3_600_000.0;
        hours_left / l_hours
    });
    let safe_rate_per_h = Some(r0 / hours_left);

    let rate_samples: Vec<&Sample> = match lb_dur {
        Some(lb) => {
            let cutoff = t0 - lb;
            current.iter().filter(|s| s.ts >= cutoff).collect()
        }
        None => current.iter().collect(),
    };

    let has_min_data = rate_samples.len() >= 2
        && (rate_samples.last().unwrap().ts - rate_samples.first().unwrap().ts)
            >= Duration::minutes(15);

    if !has_min_data {
        return Prognosis {
            remaining_now: r0,
            reset_time: end,
            last_sample: t0,
            budget_now,
            rate_per_h: None,
            safe_rate_per_h,
            at_reset: None,
            empty_at: None,
            status: Status::Collecting,
        };
    }

    // Least-squares slope with x = hours relative to t0
    let n = rate_samples.len() as f64;
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let points: Vec<(f64, f64)> = rate_samples
        .iter()
        .map(|s| {
            let x = (s.ts - t0).num_milliseconds() as f64 / 3_600_000.0;
            let y = s.remaining;
            sum_x += x;
            sum_y += y;
            (x, y)
        })
        .collect();

    let mean_x = sum_x / n;
    let mean_y = sum_y / n;
    let mut numerator = 0.0;
    let mut denominator = 0.0;
    for (x, y) in points {
        let dx = x - mean_x;
        let dy = y - mean_y;
        numerator += dx * dy;
        denominator += dx * dx;
    }

    let slope = if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    };
    let rate = (-slope).max(0.0);
    let rate_per_h = Some(rate);
    let at_reset_val = (r0 - rate * hours_left).max(0.0);
    let at_reset = Some(at_reset_val);

    let empty_at = if rate > 0.0 {
        let hours_to_empty = r0 / rate;
        let empty_time = t0 + Duration::milliseconds((hours_to_empty * 3_600_000.0).round() as i64);
        if empty_time < end {
            Some(empty_time)
        } else {
            None
        }
    } else {
        None
    };

    let status = if empty_at.is_some() {
        Status::WillRunOut
    } else if at_reset_val < tight {
        Status::Tight
    } else {
        Status::OnTrack
    };

    Prognosis {
        remaining_now: r0,
        reset_time: end,
        last_sample: t0,
        budget_now,
        rate_per_h,
        safe_rate_per_h,
        at_reset,
        empty_at,
        status,
    }
}

pub fn analyze(samples: &[Sample], tight: f64) -> Vec<BucketReport> {
    if samples.is_empty() {
        return Vec::new();
    }

    let latest_ts = samples.iter().map(|s| s.ts).max().unwrap();
    let mut group_order = Vec::new();

    // Groups in latest ts batch in order of appearance
    for s in samples.iter().filter(|s| s.ts == latest_ts) {
        if !group_order.contains(&s.group) {
            group_order.push(s.group.clone());
        }
    }

    // Older groups
    for s in samples {
        if !group_order.contains(&s.group) {
            group_order.push(s.group.clone());
        }
    }

    // Group samples by (group, bucket_id)
    use std::collections::BTreeMap;
    let mut by_bucket: BTreeMap<(String, String), Vec<Sample>> = BTreeMap::new();
    for s in samples {
        by_bucket
            .entry((s.group.clone(), s.bucket_id.clone()))
            .or_default()
            .push(s.clone());
    }

    let mut reports = Vec::new();

    for ((group, bucket_id), mut history) in by_bucket {
        history.sort_by_key(|s| s.ts);
        let boundary_indices = window_boundaries(&history);
        let boundaries: Vec<DateTime<Utc>> =
            boundary_indices.iter().map(|&i| history[i].ts).collect();

        let last_boundary = boundary_indices.last().copied().unwrap_or(0);
        let candidate = &history[last_boundary..];
        let last = history.last().unwrap();
        let l_dur = window_length(&last.window);
        let window_start = l_dur.map(|dur| last.reset_time - dur);

        let mut current: Vec<Sample> = if let Some(ws) = window_start {
            candidate.iter().filter(|s| s.ts >= ws).cloned().collect()
        } else {
            candidate.to_vec()
        };
        if current.is_empty() {
            current.push(last.clone());
        }

        let prognosis = prognose(&current, tight);

        reports.push(BucketReport {
            group,
            bucket_id,
            window: last.window.clone(),
            history,
            current,
            window_start,
            boundaries,
            prognosis,
        });
    }

    // Sort reports by group order, then within group weekly, 5h, then alphabetical
    reports.sort_by(|a, b| {
        let g_idx_a = group_order
            .iter()
            .position(|g| g == &a.group)
            .unwrap_or(usize::MAX);
        let g_idx_b = group_order
            .iter()
            .position(|g| g == &b.group)
            .unwrap_or(usize::MAX);
        if g_idx_a != g_idx_b {
            return g_idx_a.cmp(&g_idx_b);
        }

        let win_rank = |w: &str| match w {
            "weekly" => 0,
            "5h" => 1,
            _ => 2,
        };
        let w_a = win_rank(&a.window);
        let w_b = win_rank(&b.window);
        if w_a != w_b {
            return w_a.cmp(&w_b);
        }

        (&a.window, &a.bucket_id).cmp(&(&b.window, &b.bucket_id))
    });

    reports
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    fn base_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap()
    }

    fn s(min_offset: i64, remaining: f64, reset: Option<DateTime<Utc>>) -> Sample {
        let ts = base_time() + Duration::minutes(min_offset);
        let reset_time = reset.unwrap_or_else(|| base_time() + Duration::hours(5));
        Sample {
            ts,
            group: "Gemini Models".to_string(),
            bucket_id: "gemini-5h".to_string(),
            window: "5h".to_string(),
            remaining,
            reset_time,
        }
    }

    #[test]
    fn constant_burn_on_track() {
        let samples = vec![
            s(0, 0.90, None),
            s(15, 0.8875, None),
            s(30, 0.875, None),
            s(45, 0.8625, None),
            s(60, 0.85, None),
        ];
        let p = prognose(&samples, 0.10);
        assert!((p.rate_per_h.unwrap() - 0.05).abs() < 1e-5);
        assert!((p.budget_now.unwrap() - 0.8).abs() < 1e-5);
        assert!((p.safe_rate_per_h.unwrap() - 0.2125).abs() < 1e-5);
        assert!((p.at_reset.unwrap() - 0.65).abs() < 1e-5);
        assert_eq!(p.empty_at, None);
        assert_eq!(p.status, Status::OnTrack);
    }

    #[test]
    fn runs_out_before_reset() {
        let samples = vec![
            s(0, 0.50, None),
            s(15, 0.45, None),
            s(30, 0.40, None),
            s(45, 0.35, None),
            s(60, 0.30, None),
        ];
        let p = prognose(&samples, 0.10);
        assert!((p.rate_per_h.unwrap() - 0.2).abs() < 1e-5);
        assert!((p.at_reset.unwrap() - 0.0).abs() < 1e-5);
        assert_eq!(p.status, Status::WillRunOut);
        let expected_empty = base_time() + Duration::minutes(60) + Duration::minutes(90);
        assert_eq!(p.empty_at, Some(expected_empty));
    }

    #[test]
    fn tight_boundary() {
        let samples_on_track = vec![s(0, 0.50 + 0.09975, None), s(60, 0.50, None)];
        let p_track = prognose(&samples_on_track, 0.10);
        assert!((p_track.at_reset.unwrap() - 0.101).abs() < 1e-4);
        assert_eq!(p_track.status, Status::OnTrack);

        let samples_tight = vec![s(0, 0.50 + 0.10025, None), s(60, 0.50, None)];
        let p_tight = prognose(&samples_tight, 0.10);
        assert!((p_tight.at_reset.unwrap() - 0.099).abs() < 1e-4);
        assert_eq!(p_tight.status, Status::Tight);
    }

    #[test]
    fn idle_zero_rate() {
        let samples = vec![s(0, 0.70, None), s(30, 0.70, None), s(60, 0.70, None)];
        let p = prognose(&samples, 0.10);
        assert!((p.rate_per_h.unwrap() - 0.0).abs() < 1e-6);
        assert!((p.at_reset.unwrap() - 0.70).abs() < 1e-6);
        assert_eq!(p.status, Status::OnTrack);
    }

    #[test]
    fn single_sample_collecting() {
        let samples = vec![s(0, 0.80, None)];
        let p = prognose(&samples, 0.10);
        assert_eq!(p.status, Status::Collecting);
        assert_eq!(p.rate_per_h, None);
        assert!(p.budget_now.is_some());
    }

    #[test]
    fn short_span_collecting() {
        let samples = vec![s(0, 0.80, None), s(10, 0.79, None)];
        let p = prognose(&samples, 0.10);
        assert_eq!(p.status, Status::Collecting);
        assert_eq!(p.rate_per_h, None);
        assert!(p.budget_now.is_some());
    }

    #[test]
    fn lookback_ignores_old() {
        let samples = vec![
            s(0, 0.90, None),
            s(30, 0.60, None),
            s(60, 0.50, None),
            s(90, 0.50, None),
            s(120, 0.50, None),
        ];
        let p = prognose(&samples, 0.10);
        assert!((p.rate_per_h.unwrap() - 0.0).abs() < 1e-6);
    }

    #[test]
    fn quota_increase_is_boundary() {
        let samples = vec![s(0, 0.40, None), s(15, 0.38, None), s(30, 1.00, None)];
        let bounds = window_boundaries(&samples);
        assert_eq!(bounds, vec![2]);
    }

    #[test]
    fn reset_after_expiry_is_boundary() {
        let reset_1700 = base_time() + Duration::hours(5);
        let reset_2200 = base_time() + Duration::hours(10);
        let samples = vec![
            s(290, 0.50, Some(reset_1700)),
            s(295, 0.50, Some(reset_1700)),
            s(305, 0.45, Some(reset_2200)),
        ];
        let bounds = window_boundaries(&samples);
        assert_eq!(bounds, vec![2]);
    }

    #[test]
    fn idle_sliding_reset_no_boundary() {
        let mut samples = Vec::new();
        for i in 0..12 {
            let ts = base_time() + Duration::minutes(i * 5);
            let reset = ts + Duration::hours(5);
            samples.push(s(i * 5, 1.0, Some(reset)));
        }
        let bounds = window_boundaries(&samples);
        assert!(bounds.is_empty());
    }

    #[test]
    fn idle_then_usage() {
        let mut samples = Vec::new();
        let start_10 = base_time() - Duration::hours(2);
        for i in 0..24 {
            let ts = start_10 + Duration::minutes(i * 5);
            let reset = ts + Duration::hours(5);
            samples.push(Sample {
                ts,
                group: "Gemini Models".to_string(),
                bucket_id: "gemini-5h".to_string(),
                window: "5h".to_string(),
                remaining: 1.0,
                reset_time: reset,
            });
        }
        let reset_1700 = base_time() + Duration::hours(5);
        let usage_points = vec![
            (20, 0.98),
            (35, 0.9675),
            (50, 0.955),
            (65, 0.9425),
            (80, 0.93),
        ];
        for (offset, rem) in usage_points {
            let ts = base_time() + Duration::minutes(offset);
            samples.push(Sample {
                ts,
                group: "Gemini Models".to_string(),
                bucket_id: "gemini-5h".to_string(),
                window: "5h".to_string(),
                remaining: rem,
                reset_time: reset_1700,
            });
        }

        let reports = analyze(&samples, 0.10);
        assert_eq!(reports.len(), 1);
        let rep = &reports[0];
        assert!(rep.boundaries.is_empty());
        for cs in &rep.current {
            assert!(cs.ts >= base_time());
        }
        assert!((rep.prognosis.rate_per_h.unwrap() - 0.05).abs() < 1e-4);
    }

    #[test]
    fn stale_data() {
        let reset_1700 = base_time() + Duration::hours(5);
        let ts_1800 = base_time() + Duration::hours(6);
        let samples = vec![Sample {
            ts: ts_1800,
            group: "Gemini Models".to_string(),
            bucket_id: "gemini-5h".to_string(),
            window: "5h".to_string(),
            remaining: 0.50,
            reset_time: reset_1700,
        }];
        let p = prognose(&samples, 0.10);
        assert_eq!(p.status, Status::Collecting);
        assert_eq!(p.safe_rate_per_h, None);
        assert_eq!(p.at_reset, None);
        assert_eq!(p.rate_per_h, None);
    }

    #[test]
    fn unknown_window() {
        let ts1 = base_time();
        let ts2 = base_time() + Duration::hours(2);
        let reset = base_time() + Duration::hours(10);
        let samples = vec![
            Sample {
                ts: ts1,
                group: "Custom".to_string(),
                bucket_id: "custom-daily".to_string(),
                window: "daily".to_string(),
                remaining: 0.8,
                reset_time: reset,
            },
            Sample {
                ts: ts2,
                group: "Custom".to_string(),
                bucket_id: "custom-daily".to_string(),
                window: "daily".to_string(),
                remaining: 0.6,
                reset_time: reset,
            },
        ];
        let reports = analyze(&samples, 0.10);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].window_start, None);
        assert_eq!(reports[0].prognosis.budget_now, None);
        assert!((reports[0].prognosis.rate_per_h.unwrap() - 0.10).abs() < 1e-4);
    }

    #[test]
    fn analyze_orders_and_splits() {
        let t1 = base_time();
        let reset_w = t1 + Duration::days(7);
        let reset_5h = t1 + Duration::hours(5);

        let make_b = |grp: &str, id: &str, win: &str, rem: f64, r_time: DateTime<Utc>| Sample {
            ts: t1,
            group: grp.to_string(),
            bucket_id: id.to_string(),
            window: win.to_string(),
            remaining: rem,
            reset_time: r_time,
        };

        let samples = vec![
            make_b("Claude and GPT models", "3p-5h", "5h", 0.9, reset_5h),
            make_b("Claude and GPT models", "3p-weekly", "weekly", 0.8, reset_w),
            make_b("Gemini Models", "gemini-5h", "5h", 0.95, reset_5h),
            make_b("Gemini Models", "gemini-weekly", "weekly", 0.85, reset_w),
            make_b("Custom Group", "custom-1", "daily", 0.5, reset_w),
        ];

        let reports = analyze(&samples, 0.10);
        assert_eq!(reports.len(), 5);

        assert_eq!(reports[0].bucket_id, "3p-weekly");
        assert_eq!(reports[1].bucket_id, "3p-5h");
        assert_eq!(reports[2].bucket_id, "gemini-weekly");
        assert_eq!(reports[3].bucket_id, "gemini-5h");
        assert_eq!(reports[4].bucket_id, "custom-1");
    }
}
