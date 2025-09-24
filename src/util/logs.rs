use crate::subscriptions;
use colored::Colorize;

// Generic function to format logs from both queries and subscriptions
pub fn format_attr_log_impl(timestamp: &str, message: &str, attributes: &[(String, String)]) {
    // we love inconsistencies!
    if attributes.is_empty()
        || (attributes.len() == 1 && attributes[0].0 == "level")
    {
        println!("{}", message);
        return;
    }

    let mut level: Option<String> = None;
    let mut others = Vec::new();
    // get attributes using a match
    for (key, value) in attributes {
        match key.to_lowercase().as_str() {
            "level" | "lvl" | "severity" => level = Some(value.clone()),
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

// Wrapper for subscription logs (still used by up.rs)
pub fn format_attr_log(log: subscriptions::deployment_logs::LogFields) {
    let attributes: Vec<(String, String)> = log.attributes.iter()
        .map(|a| (a.key.clone(), a.value.clone()))
        .collect();
    format_attr_log_impl(&log.timestamp, &log.message, &attributes);
}
