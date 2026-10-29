use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

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
}
