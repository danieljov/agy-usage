use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{Sample, lookback, window_length};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Collecting,
    WillRunOut,
    Tight,
    OnTrack,
    Disabled,
}

impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Status::Collecting => "⏳ collecting",
            Status::WillRunOut => "❌ will run out",
            Status::Tight => "⚠  tight",
            Status::OnTrack => "✅ on track",
            Status::Disabled => "🚫 disabled",
        }
    }

    pub fn label_with_time(&self, empty_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
        match self {
            Status::WillRunOut => {
                if let Some(t) = empty_at {
                    let diff = (t - now).num_seconds();
                    if diff <= 0 {
                        "❌ now".to_string()
                    } else if diff < 60 {
                        "❌ in <1m".to_string()
                    } else if diff < 3600 {
                        format!("❌ in {}m", diff / 60)
                    } else if diff < 86400 {
                        let h = diff / 3600;
                        let m = (diff % 3600) / 60;
                        if m > 0 {
                            format!("❌ in {}h {}m", h, m)
                        } else {
                            format!("❌ in {}h", h)
                        }
                    } else {
                        let d = diff / 86400;
                        let h = (diff % 86400) / 3600;
                        if h > 0 {
                            format!("❌ in {}d {}h", d, h)
                        } else {
                            format!("❌ in {}d", d)
                        }
                    }
                } else {
                    self.label().to_string()
                }
            }
            Status::Disabled => "🚫 disabled".to_string(),
            s => s.label().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prognosis {
    pub remaining_now: f64,
    pub reset_time: Option<DateTime<Utc>>,
    pub last_sample: DateTime<Utc>,
    pub budget_now: Option<f64>,
    pub rate_per_h: Option<f64>,
    pub safe_rate_per_h: Option<f64>,
    pub at_reset: Option<f64>,
    pub empty_at: Option<DateTime<Utc>>,
    pub status: Status,
}

impl Prognosis {
    pub fn status_label(&self, now: DateTime<Utc>) -> String {
        self.status.label_with_time(self.empty_at, now)
    }
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
        let large_quota_jump = curr.remaining > prev.remaining + 0.10;
        let reset_moved = match (curr.reset_time, prev.reset_time) {
            (Some(c), Some(p)) => c > p + Duration::seconds(60),
            _ => false,
        };
        let reset_expired = match prev.reset_time {
            Some(p) => p <= curr.ts,
            None => false,
        };
        let disabled_changed = prev.disabled != curr.disabled;
        if (reset_moved && (reset_expired || quota_increased))
            || large_quota_jump
            || disabled_changed
        {
            boundaries.push(i);
        }
    }
    boundaries
}

pub fn prognose(current: &[Sample], tight: f64) -> Prognosis {
    let last = current.last().expect("current samples must not be empty");
    let t0 = last.ts;
    let r0 = last.remaining;
    if last.disabled {
        return Prognosis {
            remaining_now: r0,
            reset_time: last.reset_time,
            last_sample: t0,
            budget_now: None,
            rate_per_h: None,
            safe_rate_per_h: None,
            at_reset: None,
            empty_at: None,
            status: Status::Disabled,
        };
    }

    let end = match last.reset_time {
        Some(rt) => rt,
        None => {
            return Prognosis {
                remaining_now: r0,
                reset_time: None,
                last_sample: t0,
                budget_now: None,
                rate_per_h: None,
                safe_rate_per_h: None,
                at_reset: None,
                empty_at: None,
                status: if last.disabled {
                    Status::Disabled
                } else {
                    Status::Collecting
                },
            };
        }
    };
    let l_dur = window_length(&last.window);
    let lb_dur = lookback(&last.window);

    let hours_left = (end - t0).num_milliseconds() as f64 / 3_600_000.0;
    if hours_left <= 0.0 {
        return Prognosis {
            remaining_now: r0,
            reset_time: Some(end),
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
    let safe_rate_per_h = Some(r0.max(0.0) / hours_left);

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
        let (status, empty_at) = if r0 <= 0.0 {
            (Status::WillRunOut, Some(t0))
        } else {
            (Status::Collecting, None)
        };
        return Prognosis {
            remaining_now: r0,
            reset_time: Some(end),
            last_sample: t0,
            budget_now,
            rate_per_h: None,
            safe_rate_per_h,
            at_reset: None,
            empty_at,
            status,
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
    // Treat |slope| < 1e-6 as zero slope to prevent floating-point noise artifacts
    let rate = if slope.abs() < 1e-6 {
        0.0
    } else {
        (-slope).max(0.0)
    };
    let rate_per_h = Some(rate);
    let at_reset_val = (r0 - rate * hours_left).max(0.0);
    let at_reset = Some(at_reset_val);

    let empty_at = if r0 <= 0.0 {
        Some(t0)
    } else if rate > 0.0 {
        let hours_to_empty = r0 / rate;
        if hours_to_empty < hours_left {
            let ms_f = (hours_to_empty * 3_600_000.0).round();
            if ms_f.is_finite() && ms_f >= 0.0 && ms_f <= i64::MAX as f64 {
                let ms = ms_f as i64;
                Duration::try_milliseconds(ms)
                    .and_then(|dur| t0.checked_add_signed(dur))
                    .filter(|&empty_time| empty_time < end)
            } else {
                None
            }
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
        reset_time: Some(end),
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

        // Detect if a 5h bucket was skipped in collection because weekly limit was reached.
        let is_orphaned_by_weekly_limit =
            if last.window == "5h" && !last.disabled && last.ts < latest_ts {
                samples.iter().any(|s| {
                    s.group == group
                        && s.window == "weekly"
                        && s.ts == latest_ts
                        && s.remaining <= 0.005
                })
            } else {
                false
            };

        let l_dur = window_length(&last.window);
        let window_start = if last.disabled || is_orphaned_by_weekly_limit {
            None
        } else {
            last.reset_time.and_then(|rt| l_dur.map(|dur| rt - dur))
        };

        let mut current: Vec<Sample> = if let Some(ws) = window_start {
            candidate.iter().filter(|s| s.ts >= ws).cloned().collect()
        } else {
            candidate.to_vec()
        };
        if current.is_empty() {
            current.push(last.clone());
        }

        let prognosis = if is_orphaned_by_weekly_limit {
            let mut orphaned_last = last.clone();
            orphaned_last.disabled = true;
            orphaned_last.ts = latest_ts;
            orphaned_last.reset_time = None;
            prognose(&[orphaned_last], tight)
        } else {
            prognose(&current, tight)
        };

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
        let reset_time = reset.or_else(|| Some(base_time() + Duration::hours(5)));
        Sample {
            ts,
            group: "Gemini Models".to_string(),
            bucket_id: "gemini-5h".to_string(),
            window: "5h".to_string(),
            remaining,
            reset_time,
            disabled: false,
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
    fn disabled_bucket_prognosis() {
        let sample = Sample {
            ts: base_time(),
            group: "Claude and GPT models".to_string(),
            bucket_id: "3p-5h".to_string(),
            window: "5h".to_string(),
            remaining: 1.0,
            reset_time: None,
            disabled: true,
        };
        let p = prognose(&[sample], 0.10);
        assert_eq!(p.status, Status::Disabled);
        assert_eq!(p.status.label(), "🚫 disabled");
        assert_eq!(p.budget_now, None);
        assert_eq!(p.rate_per_h, None);
        assert_eq!(p.safe_rate_per_h, None);
        assert_eq!(p.at_reset, None);
        assert_eq!(p.empty_at, None);
        assert_eq!(p.reset_time, None);
    }

    #[test]
    fn analyze_explicit_disabled_bucket() {
        let t1 = base_time();
        let reset_w = t1 + Duration::days(4);
        let samples = vec![
            Sample {
                ts: t1,
                group: "Claude and GPT models".to_string(),
                bucket_id: "3p-weekly".to_string(),
                window: "weekly".to_string(),
                remaining: 0.0,
                reset_time: Some(reset_w),
                disabled: false,
            },
            Sample {
                ts: t1,
                group: "Claude and GPT models".to_string(),
                bucket_id: "3p-5h".to_string(),
                window: "5h".to_string(),
                remaining: 1.0,
                reset_time: None,
                disabled: true,
            },
        ];
        let reports = analyze(&samples, 0.10);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].bucket_id, "3p-weekly");
        assert_eq!(reports[0].prognosis.status, Status::WillRunOut);

        assert_eq!(reports[1].bucket_id, "3p-5h");
        assert_eq!(reports[1].prognosis.status, Status::Disabled);
        assert_eq!(reports[1].window_start, None);
        assert_eq!(reports[1].prognosis.reset_time, None);
    }

    #[test]
    fn analyze_orphaned_5h_when_weekly_limit_exhausted() {
        let t_old = base_time() - Duration::hours(8);
        let t_latest = base_time();
        let reset_w = t_latest + Duration::days(4);
        let reset_5h_old = t_old + Duration::hours(5);

        let samples = vec![
            Sample {
                ts: t_old,
                group: "Claude and GPT models".to_string(),
                bucket_id: "3p-5h".to_string(),
                window: "5h".to_string(),
                remaining: 0.83,
                reset_time: Some(reset_5h_old),
                disabled: false,
            },
            Sample {
                ts: t_latest,
                group: "Claude and GPT models".to_string(),
                bucket_id: "3p-weekly".to_string(),
                window: "weekly".to_string(),
                remaining: 0.0,
                reset_time: Some(reset_w),
                disabled: false,
            },
        ];
        let reports = analyze(&samples, 0.10);
        assert_eq!(reports.len(), 2);
        let rep_5h = reports.iter().find(|r| r.bucket_id == "3p-5h").unwrap();
        assert_eq!(rep_5h.prognosis.status, Status::Disabled);
        assert_eq!(rep_5h.window_start, None);
        assert_eq!(rep_5h.prognosis.last_sample, t_latest);
    }

    #[test]
    fn near_zero_burn_rate_no_panic() {
        // Very small drop: 0.50 -> 0.4999999999999999 over 60 min
        let samples = vec![s(0, 0.50, None), s(60, 0.50 - 1e-15, None)];
        let p = prognose(&samples, 0.10);
        assert_eq!(p.empty_at, None);
        assert_eq!(p.status, Status::OnTrack);
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
    fn mid_window_reconciliation_not_boundary_and_preserves_current() {
        let t0 = base_time();
        let reset_w = t0 + Duration::days(6);
        let mk_weekly = |min_offset: i64, remaining: f64| Sample {
            ts: t0 + Duration::minutes(min_offset),
            group: "Gemini Models".to_string(),
            bucket_id: "gemini-weekly".to_string(),
            window: "weekly".to_string(),
            remaining,
            reset_time: Some(reset_w),
            disabled: false,
        };

        // Steady burn with mid-window backend quota reconciliations (+0.6%, +0.71%, +1.85%)
        // while reset_time remains identical.
        let samples = vec![
            mk_weekly(0, 0.9200),
            mk_weekly(60, 0.9071),
            mk_weekly(65, 0.9132), // +0.61% reconciliation
            mk_weekly(120, 0.8562),
            mk_weekly(125, 0.8619), // +0.57% reconciliation
            mk_weekly(150, 0.8458),
            mk_weekly(155, 0.8530), // +0.72% reconciliation
            mk_weekly(180, 0.8300),
            mk_weekly(185, 0.8485), // +1.85% reconciliation
            mk_weekly(240, 0.8200),
        ];

        assert!(window_boundaries(&samples).is_empty());

        let reports = analyze(&samples, 0.10);
        assert_eq!(reports.len(), 1);
        assert!(reports[0].boundaries.is_empty());
        assert_eq!(reports[0].current.len(), samples.len());
        assert!(reports[0].prognosis.rate_per_h.unwrap() > 0.02);
    }

    #[test]
    fn early_reset_with_advanced_reset_time_is_boundary() {
        let reset_initial = base_time() + Duration::hours(5);
        let reset_advanced = reset_initial + Duration::minutes(38);
        let samples = vec![
            s(0, 0.98, Some(reset_initial)),
            s(15, 0.9698, Some(reset_initial)),
            // Early reset before reset_initial expires: remaining increases by +2.98%
            // and reset_time advances by >60s.
            s(20, 0.9996, Some(reset_advanced)),
            s(35, 0.9850, Some(reset_advanced)),
        ];
        let bounds = window_boundaries(&samples);
        assert_eq!(bounds, vec![2]);
    }

    #[test]
    fn large_quota_jump_with_unchanged_or_none_reset_time_is_boundary() {
        let reset = base_time() + Duration::hours(5);
        let unchanged_some = vec![
            s(0, 0.45, Some(reset)),
            s(15, 0.40, Some(reset)),
            s(30, 1.00, Some(reset)),
        ];
        assert_eq!(window_boundaries(&unchanged_some), vec![2]);

        let none_reset = vec![
            Sample {
                ts: base_time(),
                group: "Custom".to_string(),
                bucket_id: "custom-1".to_string(),
                window: "5h".to_string(),
                remaining: 0.40,
                reset_time: None,
                disabled: false,
            },
            Sample {
                ts: base_time() + Duration::minutes(15),
                group: "Custom".to_string(),
                bucket_id: "custom-1".to_string(),
                window: "5h".to_string(),
                remaining: 0.41, // +1.0% minor reconciliation with None reset_time -> not a boundary
                reset_time: None,
                disabled: false,
            },
            Sample {
                ts: base_time() + Duration::minutes(30),
                group: "Custom".to_string(),
                bucket_id: "custom-1".to_string(),
                window: "5h".to_string(),
                remaining: 1.00, // +59% refill jump with None reset_time -> boundary
                reset_time: None,
                disabled: false,
            },
        ];
        assert_eq!(window_boundaries(&none_reset), vec![2]);
    }

    #[test]
    fn window_boundary_threshold_invariants_and_disabled_transitions() {
        let reset = base_time() + Duration::hours(5);

        // 1. Clock jitter <= 60s with +2% reconciliation is NOT a boundary,
        //    whereas > 60s (+61s) with +2% IS a boundary.
        let jitter_60s = vec![
            s(0, 0.80, Some(reset)),
            s(5, 0.82, Some(reset + Duration::seconds(60))),
        ];
        assert!(window_boundaries(&jitter_60s).is_empty());

        let advance_61s = vec![
            s(0, 0.80, Some(reset)),
            s(5, 0.82, Some(reset + Duration::seconds(61))),
        ];
        assert_eq!(window_boundaries(&advance_61s), vec![1]);

        // 2. With reset_moved = true and not expired, <= +0.5% (+0.4%) is NOT a boundary,
        //    whereas > +0.5% (+0.6%) IS a boundary.
        let moved_reset = reset + Duration::minutes(30);
        let sub_half_pct = vec![s(0, 0.800, Some(reset)), s(5, 0.804, Some(moved_reset))];
        assert!(window_boundaries(&sub_half_pct).is_empty());

        let above_half_pct = vec![s(0, 0.800, Some(reset)), s(5, 0.806, Some(moved_reset))];
        assert_eq!(window_boundaries(&above_half_pct), vec![1]);

        // 3. With reset_moved = false, <= +10% (+9.5%) is NOT a boundary,
        //    whereas > +10% (+10.5%) IS a boundary.
        let jump_9_5_pct = vec![s(0, 0.400, Some(reset)), s(5, 0.495, Some(reset))];
        assert!(window_boundaries(&jump_9_5_pct).is_empty());

        let jump_10_5_pct = vec![s(0, 0.400, Some(reset)), s(5, 0.505, Some(reset))];
        assert_eq!(window_boundaries(&jump_10_5_pct), vec![1]);

        // 4. Disabled state transitions (false -> true -> false) are boundaries
        //    even without quota increase or reset_time movement.
        let mut s_disabled = s(5, 0.50, Some(reset));
        s_disabled.disabled = true;
        let disabled_flip = vec![
            s(0, 0.50, Some(reset)),
            s_disabled,
            s(10, 0.50, Some(reset)),
        ];
        assert_eq!(window_boundaries(&disabled_flip), vec![1, 2]);
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
                reset_time: Some(reset),
                disabled: false,
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
                reset_time: Some(reset_1700),
                disabled: false,
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
            reset_time: Some(reset_1700),
            disabled: false,
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
                reset_time: Some(reset),
                disabled: false,
            },
            Sample {
                ts: ts2,
                group: "Custom".to_string(),
                bucket_id: "custom-daily".to_string(),
                window: "daily".to_string(),
                remaining: 0.6,
                reset_time: Some(reset),
                disabled: false,
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
            reset_time: Some(r_time),
            disabled: false,
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

    #[test]
    fn exhausted_bucket_zero_or_negative_r0() {
        let t0 = base_time();
        let samples_zero = vec![s(0, 0.0, None), s(30, 0.0, None)];
        let p_zero = prognose(&samples_zero, 0.10);
        assert_eq!(p_zero.status, Status::WillRunOut);
        assert_eq!(p_zero.empty_at, Some(t0 + Duration::minutes(30)));

        let samples_neg = vec![s(0, -0.05, None), s(30, -0.05, None)];
        let p_neg = prognose(&samples_neg, 0.10);
        assert_eq!(p_neg.status, Status::WillRunOut);
        assert_eq!(p_neg.empty_at, Some(t0 + Duration::minutes(30)));
    }

    #[test]
    fn slope_epsilon_noise_clamped_to_zero() {
        // Samples with a tiny drift less than 1e-6 / hour
        let samples = vec![s(0, 0.5000000001, None), s(60, 0.50, None)];
        let p = prognose(&samples, 0.10);
        assert_eq!(p.rate_per_h, Some(0.0));
        assert_eq!(p.empty_at, None);
        assert_eq!(p.status, Status::OnTrack);
    }

    #[test]
    fn chrono_saturation_negative_quota_no_panic() {
        let mut samples = Vec::new();
        // 20 samples with negative quota: IEEE-754 least squares can produce ~1e-33 slope
        for i in 0..20 {
            samples.push(s(i * 5, -0.001, None));
        }
        let p = prognose(&samples, 0.10);
        assert_eq!(p.status, Status::WillRunOut);
        assert_eq!(p.empty_at, Some(samples.last().unwrap().ts));
    }

    #[test]
    fn status_label_formatting() {
        let now = Utc::now();

        // OnTrack, Tight, Collecting
        assert_eq!(Status::OnTrack.label_with_time(None, now), "✅ on track");
        assert_eq!(Status::Tight.label_with_time(None, now), "⚠  tight");
        assert_eq!(
            Status::Collecting.label_with_time(None, now),
            "⏳ collecting"
        );

        // WillRunOut with no empty_at
        assert_eq!(
            Status::WillRunOut.label_with_time(None, now),
            "❌ will run out"
        );

        // WillRunOut in the past or now
        assert_eq!(Status::WillRunOut.label_with_time(Some(now), now), "❌ now");
        assert_eq!(
            Status::WillRunOut.label_with_time(Some(now - Duration::seconds(10)), now),
            "❌ now"
        );

        // WillRunOut < 60s
        assert_eq!(
            Status::WillRunOut.label_with_time(Some(now + Duration::seconds(45)), now),
            "❌ in <1m"
        );

        // WillRunOut minutes
        assert_eq!(
            Status::WillRunOut.label_with_time(Some(now + Duration::minutes(15)), now),
            "❌ in 15m"
        );

        // WillRunOut hours and minutes
        assert_eq!(
            Status::WillRunOut.label_with_time(Some(now + Duration::minutes(135)), now),
            "❌ in 2h 15m"
        );

        // WillRunOut exact hours
        assert_eq!(
            Status::WillRunOut.label_with_time(Some(now + Duration::hours(3)), now),
            "❌ in 3h"
        );

        // WillRunOut days and hours
        assert_eq!(
            Status::WillRunOut.label_with_time(Some(now + Duration::hours(27)), now),
            "❌ in 1d 3h"
        );

        // WillRunOut exact days
        assert_eq!(
            Status::WillRunOut.label_with_time(Some(now + Duration::days(2)), now),
            "❌ in 2d"
        );

        // Via Prognosis::status_label
        let p = Prognosis {
            remaining_now: 0.5,
            reset_time: Some(now + Duration::hours(10)),
            last_sample: now,
            budget_now: None,
            rate_per_h: None,
            safe_rate_per_h: None,
            at_reset: None,
            empty_at: Some(now + Duration::minutes(45)),
            status: Status::WillRunOut,
        };
        assert_eq!(p.status_label(now), "❌ in 45m");
    }
}
