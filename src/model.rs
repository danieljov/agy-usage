use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Minutes after which the latest sample is considered stale across status, doctor, and TUI.
pub const STALE_THRESHOLD_MINS: i64 = 15;

/// Rounds `value` using scaling `factor` (`0.0` normalized cleanly for JSON/UI output).
pub fn round_decimals(value: f64, factor: f64) -> f64 {
    if !value.is_finite() || !factor.is_finite() || factor <= 0.0 {
        return 0.0;
    }
    let scaled = (value * factor).round();
    if !scaled.is_finite() {
        return value;
    }
    let rounded = scaled / factor;
    if rounded == 0.0 { 0.0 } else { rounded }
}

/// Rounds `value` to 4 decimal places (`0.0` normalized cleanly for JSON/UI output).
pub fn round4(value: f64) -> f64 {
    round_decimals(value, 10_000.0)
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Sample {
    pub ts: DateTime<Utc>,
    pub group: String,
    pub bucket_id: String,
    pub window: String,
    pub remaining: f64,
    pub reset_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub disabled: bool,
}

pub fn window_length(window: &str) -> Option<Duration> {
    let dur = match window {
        "weekly" => Duration::days(7),
        "daily" => Duration::hours(24),
        _ => {
            let (digits, unit) = window.split_at_checked(window.len().checked_sub(1)?)?;
            if digits.is_empty()
                || digits.starts_with('0')
                || !digits.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            let n: i64 = digits.parse().ok()?;
            match unit {
                "h" => Duration::try_hours(n)?,
                "d" => Duration::try_days(n)?,
                "w" => Duration::try_weeks(n)?,
                _ => return None,
            }
        }
    };
    (dur <= Duration::days(7)).then_some(dur)
}

pub fn lookback(window: &str) -> Option<Duration> {
    window_length(window).map(|len| (len / 5).clamp(Duration::minutes(15), Duration::hours(24)))
}

pub fn format_window(win: &str) -> std::borrow::Cow<'_, str> {
    match win {
        "weekly" => "Weekly".into(),
        "daily" => "Daily".into(),
        "5h" => "5h".into(),
        other => other.into(),
    }
}

/// Validates and normalizes `--tight-threshold` into a finite fraction in `0.0..=1.0`.
/// Normalizes `-0.0` to `0.0` so CLI formatting and unit files never emit `-0`.
pub fn normalize_tight_threshold(v: f64) -> Result<f64, String> {
    if !v.is_finite() || !(0.0..=1.0).contains(&v) {
        return Err(format!(
            "--tight-threshold must be a finite fraction in 0.0..=1.0 (got {v})"
        ));
    }
    Ok(if v == 0.0 { 0.0 } else { v })
}

/// Parses `--tight-threshold` from CLI input as a finite fraction in `0.0..=1.0`.
pub fn parse_tight_threshold(raw: &str) -> Result<f64, String> {
    let val = raw
        .parse::<f64>()
        .map_err(|e| format!("invalid --tight-threshold '{raw}': {e}"))?;
    normalize_tight_threshold(val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_length() {
        assert_eq!(window_length("weekly"), Some(Duration::days(7)));
        assert_eq!(window_length("daily"), Some(Duration::hours(24)));
        assert_eq!(window_length("5h"), Some(Duration::hours(5)));
        assert_eq!(window_length("1h"), Some(Duration::hours(1)));
        assert_eq!(window_length("24h"), Some(Duration::hours(24)));
        assert_eq!(window_length("168h"), Some(Duration::days(7)));
        assert_eq!(window_length("1d"), Some(Duration::days(1)));
        assert_eq!(window_length("7d"), Some(Duration::days(7)));
        assert_eq!(window_length("1w"), Some(Duration::days(7)));

        // Cap at 7 days so ANALYSIS_MARGIN_DAYS (8d) remains sound
        assert_eq!(window_length("169h"), None);
        assert_eq!(window_length("8d"), None);
        assert_eq!(window_length("2w"), None);
        assert_eq!(window_length("30d"), None);

        // Reject 0, leading zeros, signs, spaces, uppercase, multi-byte UTF-8, overflow, and unknown suffixes
        for invalid in [
            "",
            "x",
            "0h",
            "0d",
            "0w",
            "05h",
            "01d",
            "+5h",
            "-5h",
            " 5h",
            "5h ",
            "5H",
            "7D",
            "1W",
            "30m",
            "monthly",
            "5€",
            "🔥",
            "999999999999999999999h",
        ] {
            assert_eq!(
                window_length(invalid),
                None,
                "expected None for {invalid:?}"
            );
        }
    }

    #[test]
    fn test_lookback() {
        assert_eq!(lookback("weekly"), Some(Duration::hours(24)));
        assert_eq!(lookback("5h"), Some(Duration::hours(1)));
        assert_eq!(lookback("daily"), Some(Duration::minutes(288)));
        assert_eq!(lookback("24h"), Some(Duration::minutes(288)));
        assert_eq!(lookback("1h"), Some(Duration::minutes(15)));
        assert_eq!(lookback("8d"), None);
        assert_eq!(lookback("2w"), None);
        assert_eq!(lookback("x"), None);
    }

    #[test]
    fn test_sample_deserialization_legacy_backward_compatibility() {
        let json = r#"{"ts":"2026-10-12T21:10:08Z","group":"Gemini Models","bucket_id":"gemini-5h","window":"5h","remaining":0.95,"reset_time":"2026-10-13T01:57:23Z"}"#;
        let s: Sample = serde_json::from_str(json).unwrap();
        assert!(!s.disabled);
        assert_eq!(
            s.reset_time,
            Some(
                DateTime::parse_from_rfc3339("2026-10-13T01:57:23Z")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
    }

    #[test]
    fn test_sample_deserialization_disabled_bucket() {
        let json = r#"{"ts":"2026-10-12T21:10:08Z","group":"Claude and GPT models","bucket_id":"3p-5h","window":"5h","remaining":1.0,"reset_time":null,"disabled":true}"#;
        let s: Sample = serde_json::from_str(json).unwrap();
        assert!(s.disabled);
        assert_eq!(s.reset_time, None);
    }

    #[test]
    fn test_round_decimals_and_normalize_tight_threshold() {
        assert_eq!(round_decimals(1e305, 10_000.0), 1e305);
        assert_eq!(round_decimals(1.23, 0.0), 0.0);
        assert_eq!(round_decimals(1.23, -10.0), 0.0);
        assert_eq!(round_decimals(f64::NAN, 100.0), 0.0);
        assert_eq!(round_decimals(f64::INFINITY, 100.0), 0.0);
        let neg_zero_rounded = round_decimals(-0.00001, 10.0);
        assert!(!neg_zero_rounded.is_sign_negative());

        assert_eq!(normalize_tight_threshold(0.0), Ok(0.0));
        let norm_neg_zero = normalize_tight_threshold(-0.0).unwrap();
        assert!(!norm_neg_zero.is_sign_negative());
        assert_eq!(normalize_tight_threshold(0.1), Ok(0.1));
        assert_eq!(normalize_tight_threshold(1.0), Ok(1.0));
        assert!(normalize_tight_threshold(-0.1).is_err());
        assert!(normalize_tight_threshold(1.1).is_err());
        assert!(normalize_tight_threshold(f64::NAN).is_err());
        assert!(normalize_tight_threshold(f64::INFINITY).is_err());
    }
}
