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
pub fn format_attr_log_string<T: LogLike>(log: &T) -> String {
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
            _ => others.push(format!(
                "{}{}{}",
                key.magenta(),
                "=",
                value
                    .normal()
                    .replace('"', "\"".dimmed().to_string().as_str())
            )),
        }
    }
    // format the level as a color
    let level = level
        .map(|level| {
            // make it uppercase so we dont have to make another variable
            // for some reason, .uppercase() removes formatting

            match level.replace('"', "").to_lowercase().as_str() {
                "info" => "[INFO]".blue(),
                "error" | "err" => "[ERRO]".red(),
                "warn" => "[WARN]".yellow(),
                "debug" => "[DBUG]".dimmed(),
                _ => format!("[{level}]").normal(),
            }
            .bold()
        })
        .unwrap();
    format!(
        "{} {} {} {}",
        timestamp.replace('"', "").normal(),
        level,
        message,
        others.join(" ")
    )
}

/// Format a log entry as a string based
pub fn format_log_string<T>(log: T, json: bool, use_formatted: bool) -> String
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
    } else if use_formatted {
        // For formatted non-JSON output
        format_attr_log_string(&log)
    } else {
        // Simple output (just the message)
        log.message().to_string()
    }
}

/// Format a log entry as a string based and print it
pub fn print_log<T>(log: T, json: bool, use_formatted: bool)
where
    T: LogLike + serde::Serialize,
{
    println!("{}", format_log_string(log, json, use_formatted));
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
        let output = format_attr_log_string(&log);
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
        let output = format_attr_log_string(&log);
        assert_eq!(output, "Test message");
    }

    #[test]
    fn test_format_attr_log_with_attributes() {
        let log = TestLog {
            message: "Test message".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            attributes: vec![
                ("level".to_string(), "error".to_string()),
                ("service".to_string(), "api".to_string()),
                ("replica".to_string(), "xyz123".to_string()),
            ],
        };

        // Should format with all attributes
        let output = format_attr_log_string(&log);
        // Check that output contains expected parts
        assert!(output.contains("Test message"));
        assert!(output.contains("2025-01-01T00:00:00Z"));
        // The colored output makes exact matching harder, but we can check structure
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
        let output = format_log_string(log, true, false);
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

        // Test simple output mode (json=false, use_formatted=false)
        let output = format_log_string(log, false, false);
        assert_eq!(output, "Test message");
    }
}
