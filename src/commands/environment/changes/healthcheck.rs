use super::PatchEntry;
use crate::util::prompt::prompt_text_with_placeholder_disappear_skippable;
use anyhow::Result;
use colored::Colorize;

pub fn parse_interactive(service_id: &str, _service_name: &str) -> Result<Vec<PatchEntry>> {
    let mut entries: Vec<PatchEntry> = Vec::new();
    let base_path = format!("services.{}.deploy", service_id);

    // Health check path
    let Some(path) = prompt_text_with_placeholder_disappear_skippable(
        "Health check endpoint <esc to skip>",
        "/health",
    )?
    else {
        return Ok(vec![]);
    };

    if path.is_empty() {
        return Ok(vec![]);
    }

    entries.push((
        format!("{}.healthcheckPath", base_path),
        serde_json::json!(path),
    ));

    // Health check timeout (only prompt if path was provided)
    loop {
        let Some(timeout_str) = prompt_text_with_placeholder_disappear_skippable(
            "Health check timeout in seconds <esc to skip>",
            "300",
        )?
        else {
            // User pressed esc, exit without setting timeout
            break;
        };

        if timeout_str.is_empty() {
            // Empty input, skip timeout
            break;
        }

        match timeout_str.parse::<i64>() {
            Ok(timeout) if timeout > 0 => {
                entries.push((
                    format!("{}.healthcheckTimeout", base_path),
                    serde_json::json!(timeout),
                ));
                break;
            }
            _ => {
                eprintln!(
                    "{} Invalid timeout '{}', must be a positive number",
                    "Warn".yellow(),
                    timeout_str
                );
                continue;
            }
        }
    }

    Ok(entries)
}
