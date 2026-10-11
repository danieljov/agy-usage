use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Sample {
    pub ts: DateTime<Utc>,
    pub group: String,
    pub bucket_id: String,
    pub window: String,
    pub remaining: f64,
    pub reset_time: DateTime<Utc>,
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
}
