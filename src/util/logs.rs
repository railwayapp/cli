use crate::{queries, subscriptions};
use colored::Colorize;
use serde_json::Value;
use std::collections::HashMap;

// Trait for common fields on log types
pub trait LogLike {
    fn message(&self) -> &str;
    fn timestamp(&self) -> &str;
    fn attributes(&self) -> Vec<(&str, &str)>;
}

/// Format log line with attributes into a colored string for display to a string
pub fn format_attr_log_string<T: LogLike>(log: &T, show_all_attributes: bool) -> String {
    let timestamp = log.timestamp();
    let message = log.message();
    let attributes = log.attributes();

    // For some reason, we choose to only format the log if there are attributes
    // other than level (which is always present). This is likely because we
    // don't want to complicate the log for users who are just console logging
    // in their app without taking advantage of our structured logging.
    if attributes.is_empty() || (attributes.len() == 1 && attributes[0].0 == "level") {
        return message.to_string();
    }

    let mut level: Option<String> = None;
    let mut others = Vec::new();
    // format attributes other than level
    for (key, value) in attributes {
        match key.to_lowercase().as_str() {
            "level" | "lvl" | "severity" => level = Some(value.to_string()),
            _ => {
                if show_all_attributes {
                    others.push(format!(
                        "{}{}{}",
                        key.magenta(),
                        "=",
                        value
                            .normal()
                            .replace('"', "\"".dimmed().to_string().as_str())
                    ));
                }
            }
        }
    }

    // If we have a level, format with level indicator
    if let Some(level) = level {
        let level_str = match level.replace('"', "").to_lowercase().as_str() {
            "info" => "[INFO]".blue(),
            "error" | "err" => "[ERRO]".red(),
            "warn" => "[WARN]".yellow(),
            "debug" => "[DBUG]".dimmed(),
            _ => format!("[{level}]").normal(),
        }
        .bold();

        if others.is_empty() {
            format!("{} {}", level_str, message)
        } else {
            format!(
                "{} {} {} {}",
                timestamp.replace('"', "").normal(),
                level_str,
                message,
                others.join(" ")
            )
        }
    } else {
        // No level attribute, just return the message
        message.to_string()
    }
}

/// Formatting mode for log output
#[derive(Clone, Copy)]
pub enum LogFormat {
    /// Just the raw message, no formatting
    Simple,
    /// Level indicator only (e.g. [ERRO]), no other attributes - good for build logs
    LevelOnly,
    /// Full formatting with all attributes - good for deploy logs
    Full,
}

/// Format a log entry as a string based
pub fn format_log_string<T>(log: T, json: bool, format: LogFormat) -> String
where
    T: LogLike + serde::Serialize,
{
    if json {
        // For JSON output, handle attributes specially
        let mut map: HashMap<String, Value> = HashMap::new();

        map.insert(
            "message".to_string(),
            serde_json::to_value(log.message()).unwrap(),
        );
        map.insert(
            "timestamp".to_string(),
            serde_json::to_value(log.timestamp()).unwrap(),
        );

        // Insert dynamic attributes
        for (key, value) in log.attributes() {
            let parsed_value = match value.trim_matches('"').parse::<Value>() {
                Ok(v) => v,
                Err(_) => serde_json::to_value(value.trim_matches('"')).unwrap(),
            };
            map.insert(key.to_string(), parsed_value);
        }

        serde_json::to_string(&map).unwrap()
    } else {
        match format {
            LogFormat::Simple => log.message().to_string(),
            LogFormat::LevelOnly => format_attr_log_string(&log, false),
            LogFormat::Full => format_attr_log_string(&log, true),
        }
    }
}

/// Format a log entry as a string based and print it
pub fn print_log<T>(log: T, json: bool, format: LogFormat)
where
    T: LogLike + serde::Serialize,
{
    println!("{}", format_log_string(log, json, format));
}

pub trait HttpLogLike {
    fn timestamp(&self) -> &str;
    fn method(&self) -> &str;
    fn path(&self) -> &str;
    fn http_status(&self) -> i64;
    fn total_duration(&self) -> i64;
    fn request_id(&self) -> &str;
}

pub fn format_http_log_string<T: HttpLogLike>(log: &T, json: bool) -> String {
    if json {
        serde_json::json!({
            "timestamp": log.timestamp(),
            "method": log.method(),
            "path": log.path(),
            "httpStatus": log.http_status(),
            "totalDuration": log.total_duration(),
            "requestId": log.request_id(),
        })
        .to_string()
    } else {
        let status = log.http_status();
        let status = match status {
            200..=299 => status.to_string().green(),
            300..=399 => status.to_string().cyan(),
            400..=499 => status.to_string().yellow(),
            500..=599 => status.to_string().red(),
            _ => status.to_string().normal(),
        };

        format!(
            "{} {} {} {} {} {}",
            log.timestamp().dimmed(),
            log.method().bold(),
            log.path(),
            status.bold(),
            format!("{}ms", log.total_duration()).dimmed(),
            log.request_id().dimmed()
        )
    }
}

pub fn print_http_log<T: HttpLogLike>(log: T, json: bool) {
    println!("{}", format_http_log_string(&log, json));
}

// Implementations for all the generated GraphQL log types
impl LogLike for subscriptions::deployment_logs::LogFields {
    fn message(&self) -> &str {
        &self.message
    }
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn attributes(&self) -> Vec<(&str, &str)> {
        self.attributes
            .iter()
            .map(|a| (a.key.as_str(), a.value.as_str()))
            .collect()
    }
}

impl LogLike for queries::deployment_logs::LogFields {
    fn message(&self) -> &str {
        &self.message
    }
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn attributes(&self) -> Vec<(&str, &str)> {
        self.attributes
            .iter()
            .map(|a| (a.key.as_str(), a.value.as_str()))
            .collect()
    }
}

impl LogLike for subscriptions::build_logs::LogFields {
    fn message(&self) -> &str {
        &self.message
    }
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn attributes(&self) -> Vec<(&str, &str)> {
        self.attributes
            .iter()
            .map(|a| (a.key.as_str(), a.value.as_str()))
            .collect()
    }
}

impl LogLike for queries::build_logs::LogFields {
    fn message(&self) -> &str {
        &self.message
    }
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn attributes(&self) -> Vec<(&str, &str)> {
        self.attributes
            .iter()
            .map(|a| (a.key.as_str(), a.value.as_str()))
            .collect()
    }
}

impl HttpLogLike for queries::http_logs::HttpLogFields {
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn method(&self) -> &str {
        &self.method
    }
    fn path(&self) -> &str {
        &self.path
    }
    fn http_status(&self) -> i64 {
        i64::from(self.http_status)
    }
    fn total_duration(&self) -> i64 {
        i64::from(self.total_duration)
    }
    fn request_id(&self) -> &str {
        &self.request_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test struct that implements LogLike for testing
    #[derive(serde::Serialize)]
    struct TestLog {
        message: String,
        timestamp: String,
        attributes: Vec<(String, String)>,
    }

    impl LogLike for TestLog {
        fn message(&self) -> &str {
            &self.message
        }
        fn timestamp(&self) -> &str {
            &self.timestamp
        }
        fn attributes(&self) -> Vec<(&str, &str)> {
            self.attributes
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect()
        }
    }

    #[test]
    fn test_format_attr_log_no_attributes() {
        let log = TestLog {
            message: "Test message".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            attributes: vec![],
        };

        // Should only return the message
        let output = format_attr_log_string(&log, false);
        assert_eq!(output, "Test message");
    }

    #[test]
    fn test_format_attr_log_only_level() {
        let log = TestLog {
            message: "Test message".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            attributes: vec![("level".to_string(), "info".to_string())],
        };

        // Should only return message when only attribute is level
        let output = format_attr_log_string(&log, false);
        assert_eq!(output, "Test message");
    }

    #[test]
    fn test_format_attr_log_with_attributes_level_only() {
        let log = TestLog {
            message: "Test message".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            attributes: vec![
                ("level".to_string(), "error".to_string()),
                ("service".to_string(), "api".to_string()),
                ("replica".to_string(), "xyz123".to_string()),
            ],
        };

        // With show_all_attributes=false, should only show level + message
        let output = format_attr_log_string(&log, false);
        assert!(output.contains("Test message"));
        // Should NOT contain the extra attributes
        assert!(!output.contains("service"));
        assert!(!output.contains("api"));
    }

    #[test]
    fn test_format_attr_log_with_attributes_full() {
        let log = TestLog {
            message: "Test message".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            attributes: vec![
                ("level".to_string(), "error".to_string()),
                ("service".to_string(), "api".to_string()),
                ("replica".to_string(), "xyz123".to_string()),
            ],
        };

        // With show_all_attributes=true, should format with all attributes
        let output = format_attr_log_string(&log, true);
        assert!(output.contains("Test message"));
        assert!(output.contains("2025-01-01T00:00:00Z"));
        assert!(output.contains("service"));
        assert!(output.contains("api"));
        assert!(output.contains("replica"));
        assert!(output.contains("xyz123"));
    }

    #[test]
    fn test_print_log_json_mode() {
        let log = TestLog {
            message: "Test message".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            attributes: vec![
                ("level".to_string(), "warn".to_string()),
                ("count".to_string(), "42".to_string()),
            ],
        };

        // Test JSON output mode
        let output = format_log_string(log, true, LogFormat::Simple);
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["message"], "Test message");
        assert_eq!(json["timestamp"], "2025-01-01T00:00:00Z");
        assert_eq!(json["level"], "warn");
        assert_eq!(json["count"], 42); // This parses as a number
    }

    #[test]
    fn test_print_log_simple_mode() {
        let log = TestLog {
            message: "Test message".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            attributes: vec![("level".to_string(), "info".to_string())],
        };

        // Test simple output mode
        let output = format_log_string(log, false, LogFormat::Simple);
        assert_eq!(output, "Test message");
    }

    #[derive(serde::Serialize)]
    struct TestHttpLog {
        timestamp: String,
        method: String,
        path: String,
        http_status: i64,
        total_duration: i64,
        request_id: String,
    }

    impl HttpLogLike for TestHttpLog {
        fn timestamp(&self) -> &str {
            &self.timestamp
        }
        fn method(&self) -> &str {
            &self.method
        }
        fn path(&self) -> &str {
            &self.path
        }
        fn http_status(&self) -> i64 {
            self.http_status
        }
        fn total_duration(&self) -> i64 {
            self.total_duration
        }
        fn request_id(&self) -> &str {
            &self.request_id
        }
    }

    #[test]
    fn test_format_http_log_plain_text() {
        let log = TestHttpLog {
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            method: "GET".to_string(),
            path: "/healthz".to_string(),
            http_status: 200,
            total_duration: 5,
            request_id: "req-123".to_string(),
        };

        let output = format_http_log_string(&log, false);
        assert!(output.contains("2025-01-01T00:00:00Z"));
        assert!(output.contains("GET"));
        assert!(output.contains("/healthz"));
        assert!(output.contains("200"));
        assert!(output.contains("5ms"));
        assert!(output.contains("req-123"));
    }

    #[test]
    fn test_format_http_log_json() {
        let log = TestHttpLog {
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            method: "POST".to_string(),
            path: "/form/verify".to_string(),
            http_status: 200,
            total_duration: 4,
            request_id: "req-456".to_string(),
        };

        let output = format_http_log_string(&log, true);
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["timestamp"], "2025-01-01T00:00:00Z");
        assert_eq!(json["method"], "POST");
        assert_eq!(json["path"], "/form/verify");
        assert_eq!(json["httpStatus"], 200);
        assert_eq!(json["totalDuration"], 4);
        assert_eq!(json["requestId"], "req-456");
    }
}
