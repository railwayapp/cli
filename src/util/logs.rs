use crate::subscriptions;
use colored::Colorize;

// Trait for log types that have common fields (matches the one in commands/logs.rs)
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
