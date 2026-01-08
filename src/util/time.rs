use anyhow::{bail, Result};
use chrono::{DateTime, Duration, Local, TimeZone, Utc};

/// Parse a time string into a UTC DateTime.
///
/// Supports:
/// - Relative times: "30s", "5m", "2h", "1d", "1w" (seconds, minutes, hours, days, weeks ago)
/// - ISO 8601 with timezone: "2024-01-15T10:30:00Z" or "2024-01-15T10:30:00-05:00"
/// - ISO 8601 without timezone (assumes local): "2024-01-15T10:30:00"
pub fn parse_time(input: &str) -> Result<DateTime<Utc>> {
    let input = input.trim();

    if let Some(dt) = parse_relative_time(input) {
        return Ok(dt);
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Ok(dt.with_timezone(&Utc));
    }

    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S") {
        let local_dt = Local.from_local_datetime(&dt).single();
        if let Some(local_dt) = local_dt {
            return Ok(local_dt.with_timezone(&Utc));
        }
    }

    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M:%S") {
        let local_dt = Local.from_local_datetime(&dt).single();
        if let Some(local_dt) = local_dt {
            return Ok(local_dt.with_timezone(&Utc));
        }
    }

    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M") {
        let local_dt = Local.from_local_datetime(&dt).single();
        if let Some(local_dt) = local_dt {
            return Ok(local_dt.with_timezone(&Utc));
        }
    }

    bail!(
        "Invalid time format: '{}'. Use relative time (e.g., 30m, 2h, 1d) or ISO 8601 (e.g., 2024-01-15T10:30:00Z)",
        input
    )
}

fn parse_relative_time(input: &str) -> Option<DateTime<Utc>> {
    let input = input.to_lowercase();

    if input.len() < 2 {
        return None;
    }

    let (num_str, unit) = input.split_at(input.len() - 1);
    let num: i64 = num_str.parse().ok()?;

    if num < 0 {
        return None;
    }

    let duration = match unit {
        "s" => Duration::seconds(num),
        "m" => Duration::minutes(num),
        "h" => Duration::hours(num),
        "d" => Duration::days(num),
        "w" => Duration::weeks(num),
        _ => return None,
    };

    Some(Utc::now() - duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_relative_seconds() {
        let result = parse_time("30s").unwrap();
        let expected = Utc::now() - Duration::seconds(30);
        assert!((result - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_relative_minutes() {
        let result = parse_time("5m").unwrap();
        let expected = Utc::now() - Duration::minutes(5);
        assert!((result - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_relative_hours() {
        let result = parse_time("2h").unwrap();
        let expected = Utc::now() - Duration::hours(2);
        assert!((result - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_relative_days() {
        let result = parse_time("1d").unwrap();
        let expected = Utc::now() - Duration::days(1);
        assert!((result - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_relative_weeks() {
        let result = parse_time("2w").unwrap();
        let expected = Utc::now() - Duration::weeks(2);
        assert!((result - expected).num_seconds().abs() < 2);
    }

    #[test]
    fn test_parse_iso8601_utc() {
        let result = parse_time("2024-01-15T10:30:00Z").unwrap();
        assert_eq!(result.to_rfc3339(), "2024-01-15T10:30:00+00:00");
    }

    #[test]
    fn test_parse_iso8601_with_offset() {
        let result = parse_time("2024-01-15T10:30:00-05:00").unwrap();
        assert_eq!(result.to_rfc3339(), "2024-01-15T15:30:00+00:00");
    }

    #[test]
    fn test_parse_invalid() {
        assert!(parse_time("invalid").is_err());
        assert!(parse_time("").is_err());
        assert!(parse_time("abc123").is_err());
    }

    #[test]
    fn test_parse_whitespace_trimmed() {
        let result = parse_time("  30m  ").unwrap();
        let expected = Utc::now() - Duration::minutes(30);
        assert!((result - expected).num_seconds().abs() < 2);
    }
}
