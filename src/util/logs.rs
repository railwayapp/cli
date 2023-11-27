use crate::subscriptions;
use colored::Colorize;

pub fn format_attr_log(mut log: subscriptions::deployment_logs::LogFields) {
    if !log.attributes.is_empty() {
        let mut level: Option<String> = None;
        let message = log.message;
        let mut others = Vec::new();
        // for some reason, not all have "" around the value
        for attr in &mut log.attributes {
            if !attr.value.starts_with('"') {
                attr.value.insert(0, '"');
            };
            if !attr.value.ends_with('"') {
                attr.value.push('"');
            }
        }
        // get attributes using a match
        for attr in &log.attributes {
            match attr.key.to_lowercase().as_str() {
                "level" | "lvl" | "severity" => level = Some(attr.value.clone()),
                _ => others.push(format!(
                    "{}{}{}",
                    attr.key.clone().bright_cyan(),
                    "=",
                    attr.value
                        .clone()
                        .replace('"', "\"".dimmed().to_string().as_str())
                )),
            }
        }
        // get the level and colour it
        let level = level.map(|level| {
            // make it uppercase so we dont have to make another variable
            // for some reason, .uppercase() removes formatting
            let level = level.replace('"', "").to_uppercase();
            match level.to_lowercase().as_str() {
                "info" => level.blue(),
                "error" => level.red(),
                "warn" => level.yellow(),
                "debug" => level.magenta(),
                _ => level.normal(),
            }
            .bold()
        });
        println!(
            "{}={} {}={} {}={}{}{5} {}",
            "timestamp".bright_cyan(),
            log.timestamp.replace('"', "").purple(),
            "level".bright_cyan(),
            level.unwrap_or_default(),
            "msg".bright_cyan(),
            "\"".dimmed(),
            message,
            others.join(" ")
        );
    } else {
        println!("{}", log.message);
    }
}
