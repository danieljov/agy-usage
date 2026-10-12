//! Shared synthetic quota dataset used by the demo examples
//! (`generate_screenshots` and `generate_demo`).
//!
//! Included via `#[path = "common/demo_data.rs"] mod demo_data;` so that Cargo
//! does not auto-discover it as a standalone example.

use agy_usage::model::Sample;
use chrono::{DateTime, Duration, Utc};

/// Generates realistic synthetic quota samples (2 hours of 5-minute intervals)
/// starting at 100% for four buckets, ending at `now`.
pub fn generate_synthetic_samples(now: DateTime<Utc>) -> Vec<Sample> {
    let mut samples = Vec::new();
    let num_samples = 25; // 2 hours of 5-min intervals

    let reset_gemini_weekly = now + Duration::days(5);
    let reset_gemini_5h = now + Duration::hours(2) + Duration::minutes(45);
    let reset_3p_weekly = now + Duration::days(4);
    let reset_3p_5h = now + Duration::hours(3) + Duration::minutes(15);

    for i in 0..num_samples {
        let minutes_ago = ((num_samples - 1 - i) * 5) as i64;
        let ts = now - Duration::minutes(minutes_ago);

        // 1. Gemini Weekly: starts at 100%, gentle usage down to 98.8%
        let gemini_weekly_rem = 1.000 - (i as f64 * 0.0005);
        samples.push(Sample {
            ts,
            group: "Gemini Models".into(),
            bucket_id: "gemini-weekly".into(),
            window: "weekly".into(),
            remaining: gemini_weekly_rem,
            reset_time: reset_gemini_weekly,
        });

        // 2. Gemini 5h: starts at 100%, steady moderate usage down to 90.4%
        let gemini_5h_rem = 1.000 - (i as f64 * 0.0040);
        samples.push(Sample {
            ts,
            group: "Gemini Models".into(),
            bucket_id: "gemini-5h".into(),
            window: "5h".into(),
            remaining: gemini_5h_rem,
            reset_time: reset_gemini_5h,
        });

        // 3. Claude/GPT Weekly: starts at 100%, slow burn down to 98.0%
        let p3_weekly_rem = 1.000 - (i as f64 * 0.0008);
        samples.push(Sample {
            ts,
            group: "Claude and GPT models".into(),
            bucket_id: "3p-weekly".into(),
            window: "weekly".into(),
            remaining: p3_weekly_rem,
            reset_time: reset_3p_weekly,
        });

        // 4. Claude/GPT 5h: starts at 100%, rapid burn over last 1.5h down to 42.4%
        let p3_5h_rem = if i < 6 {
            1.000 - (i as f64 * 0.003)
        } else {
            0.982 - ((i - 6) as f64 * 0.031)
        };
        samples.push(Sample {
            ts,
            group: "Claude and GPT models".into(),
            bucket_id: "3p-5h".into(),
            window: "5h".into(),
            remaining: p3_5h_rem.max(0.05),
            reset_time: reset_3p_5h,
        });
    }

    samples
}
