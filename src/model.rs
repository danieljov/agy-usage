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
    match window {
        "weekly" => Some(Duration::days(7)),
        "5h" => Some(Duration::hours(5)),
        _ => None,
    }
}

pub fn lookback(window: &str) -> Option<Duration> {
    match window {
        "weekly" => Some(Duration::hours(24)),
        "5h" => Some(Duration::hours(1)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_length() {
        assert_eq!(window_length("weekly"), Some(Duration::days(7)));
        assert_eq!(window_length("5h"), Some(Duration::hours(5)));
        assert_eq!(window_length("x"), None);
    }

    #[test]
    fn test_lookback() {
        assert_eq!(lookback("weekly"), Some(Duration::hours(24)));
        assert_eq!(lookback("5h"), Some(Duration::hours(1)));
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
