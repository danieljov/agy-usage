//! Shared synthetic quota dataset used by the demo examples
//! (`generate_screenshots` and `generate_demo`).
//!
//! Placed in a subdirectory without a `main.rs` so Cargo does not auto-discover
//! it as a standalone example binary, and included via
//! `#[path = "common/demo_data.rs"] mod demo_data;`.

use agy_usage::model::Sample;
use chrono::{DateTime, Duration, Utc};

const WEEKLY_MINS: i64 = 7 * 24 * 60; // 10,080 minutes (168h)
const FIVE_H_MINS: i64 = 5 * 60; // 300 minutes (5h)

/// Computes `(win_idx, mins_in_win, win_reset)` for timestamp `ts` relative to a
/// bucket's active window starting at `active_ws` with duration `win_mins`.
///
/// - `win_idx == 0` is the active window (`[active_ws, active_ws + win_mins)`).
/// - `win_idx < 0` are prior historical windows.
/// - `mins_in_win` is in `0..win_mins`, equal to `0` at the exact window start.
fn window_position(
    ts: DateTime<Utc>,
    active_ws: DateTime<Utc>,
    win_mins: i64,
) -> (i64, i64, DateTime<Utc>) {
    let delta_mins = (ts - active_ws).num_minutes();
    let win_idx = delta_mins.div_euclid(win_mins);
    let mins_in_win = delta_mins.rem_euclid(win_mins);
    let win_reset = active_ws + Duration::minutes((win_idx + 1) * win_mins);
    (win_idx, mins_in_win, win_reset)
}

/// Generates 30 days of deterministic synthetic quota samples ending at `now`:
/// - 15-minute intervals across `now - 30d .. now - 24h`
/// - 5-minute intervals across `now - 24h ..= now`
///
/// Every active and historical `Weekly` and `5h` window starts at `100%` on an
/// exact sample timestamp (`mins_in_win == 0`), producing realistic multi-window
/// reset boundaries across all range toggles (`5h`, `24h`, `7d`, `30d`, `All`)
/// and covering `OnTrack`, `Tight`, and `WillRunOut` prognosis states.
pub fn generate_synthetic_samples(now: DateTime<Utc>) -> Vec<Sample> {
    let ws_gemini_weekly = now - Duration::days(2); // reset = now + 5d
    let ws_gemini_5h = now - Duration::minutes(135); // reset = now + 2h 45m
    let ws_3p_weekly = now - Duration::days(3); // reset = now + 4d
    let ws_3p_5h = now - Duration::minutes(105); // reset = now + 3h 15m

    let mins_ago_iter = (97..=2880)
        .rev()
        .map(|k| k * 15)
        .chain((0..=288).rev().map(|k| k * 5));

    let mut samples = Vec::with_capacity((2784 + 289) * 4);

    for mins_ago in mins_ago_iter {
        let ts = now - Duration::minutes(mins_ago);

        // 1. Gemini Weekly: active window starts at 100% at `now - 2d`, steady 0.45%/h burn
        //    down to 78.4% at `now` (24.4% left at reset -> Status::OnTrack).
        let (gw_idx, gw_mins, gw_reset) = window_position(ts, ws_gemini_weekly, WEEKLY_MINS);
        let gw_elapsed_h = gw_mins as f64 / 60.0;
        let gemini_weekly_rem = if gw_idx == 0 {
            1.0 - 0.0045 * gw_elapsed_h
        } else {
            1.0 - 0.0044 * gw_elapsed_h
        };
        samples.push(Sample {
            ts,
            group: "Gemini Models".into(),
            bucket_id: "gemini-weekly".into(),
            window: "weekly".into(),
            remaining: gemini_weekly_rem.clamp(0.0, 1.0),
            reset_time: Some(gw_reset),
            disabled: false,
        });

        // 2. Gemini 5h: active window starts at 100% at `now - 2h 15m`, steady 8.00%/h burn
        //    down to 82.0% at `now` (60.0% left at reset -> Status::OnTrack).
        let (g5_idx, g5_mins, g5_reset) = window_position(ts, ws_gemini_5h, FIVE_H_MINS);
        let g5_elapsed_h = g5_mins as f64 / 60.0;
        let gemini_5h_rem = if g5_idx == 0 {
            1.0 - 0.08 * g5_elapsed_h
        } else {
            1.0 - 0.095 * g5_elapsed_h
        };
        samples.push(Sample {
            ts,
            group: "Gemini Models".into(),
            bucket_id: "gemini-5h".into(),
            window: "5h".into(),
            remaining: gemini_5h_rem.clamp(0.0, 1.0),
            reset_time: Some(g5_reset),
            disabled: false,
        });

        // 3. Claude/GPT Weekly: active window starts at 100% at `now - 3d`, 0.56%/h burn
        //    down to 59.68% at `now` (5.9% left at reset < 10% -> Status::Tight).
        let (cw_idx, cw_mins, cw_reset) = window_position(ts, ws_3p_weekly, WEEKLY_MINS);
        let cw_elapsed_h = cw_mins as f64 / 60.0;
        let p3_weekly_rem = if cw_idx == 0 {
            1.0 - 0.0056 * cw_elapsed_h
        } else {
            1.0 - 0.0052 * cw_elapsed_h
        };
        samples.push(Sample {
            ts,
            group: "Claude and GPT models".into(),
            bucket_id: "3p-weekly".into(),
            window: "weekly".into(),
            remaining: p3_weekly_rem.clamp(0.0, 1.0),
            reset_time: Some(cw_reset),
            disabled: false,
        });

        // 4. Claude/GPT 5h: active window starts at 100% at `now - 1h 45m`, moderate burn
        //    over first 45m to 79.6%, then 37.20%/h over last 60m down to 42.4% at `now`
        //    (runs out in 1h 8m -> Status::WillRunOut).
        let (c5_idx, c5_mins, c5_reset) = window_position(ts, ws_3p_5h, FIVE_H_MINS);
        let p3_5h_rem = if c5_idx == 0 {
            if c5_mins <= 45 {
                1.0 - 0.204 * (c5_mins as f64 / 45.0)
            } else {
                0.796 - 0.372 * ((c5_mins - 45) as f64 / 60.0)
            }
        } else {
            1.0 - 0.14 * (c5_mins as f64 / 60.0)
        };
        samples.push(Sample {
            ts,
            group: "Claude and GPT models".into(),
            bucket_id: "3p-5h".into(),
            window: "5h".into(),
            remaining: p3_5h_rem.clamp(0.0, 1.0),
            reset_time: Some(c5_reset),
            disabled: false,
        });
    }

    samples
}
