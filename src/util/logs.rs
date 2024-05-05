use crate::subscriptions;
use colored::Colorize;

pub fn format_attr_log(log: subscriptions::deployment_logs::LogFields) {
    // we love inconsistencies!
    if log.attributes.is_empty()
        || (log.attributes.len() == 1
            && log
                .attributes
                .first()
                .is_some_and(|attr| attr.key == "level"))
    {
        println!("{}", log.message);
        return;
    }

    let mut level: Option<String> = None;
    let message = log.message;
    let mut others = Vec::new();
    // get attributes using a match
    for attr in &log.attributes {
        match attr.key.to_lowercase().as_str() {
            "level" | "lvl" | "severity" => level = Some(attr.value.clone()),
            _ => others.push(format!(
                "{}{}{}",
                attr.key.clone().magenta(),
                "=",
                attr.value
                    .clone()
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
                _ => format!("[{}]", level).normal(),
            }
            .bold()
        })
        .unwrap();
    println!(
        "{} {} {} {}",
        log.timestamp.replace('"', "").normal(),
        level,
        message,
        others.join(" ")
    );
}
