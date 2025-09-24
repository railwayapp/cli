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

// Generic function to format logs from any type implementing LogLike
pub fn format_attr_log<T: LogLike>(log: &T) {
    let timestamp = log.timestamp();
    let message = log.message();
    let attributes = log.attributes();
    // we love inconsistencies!
    if attributes.is_empty() || (attributes.len() == 1 && attributes[0].0 == "level") {
        println!("{}", message);
        return;
    }

    let mut level: Option<String> = None;
    let mut others = Vec::new();
    // get attributes using a match
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
    // get the level and colour it
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
    println!(
        "{} {} {} {}",
        timestamp.replace('"', "").normal(),
        level,
        message,
        others.join(" ")
    );
}

// Helper function to print any log type
pub fn print_log<T>(log: T, json: bool, use_formatted: bool)
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

        let json_string = serde_json::to_string(&map).unwrap();
        println!("{json_string}");
    } else if use_formatted {
        // For formatted non-JSON output
        format_attr_log(&log);
    } else {
        // Simple output (just the message)
        println!("{}", log.message());
    }
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
