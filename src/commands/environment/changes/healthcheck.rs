use super::PatchEntry;
use crate::controllers::config::environment::ServiceInstance;
use crate::util::prompt::prompt_text_with_placeholder_disappear_skippable;
use anyhow::Result;
use colored::Colorize;

pub fn parse_interactive(
    service_id: &str,
    _service_name: &str,
    existing: Option<&ServiceInstance>,
) -> Result<Vec<PatchEntry>> {
    let existing_deploy = existing.and_then(|e| e.deploy.as_ref());
    let existing_healthcheck_path = existing_deploy.and_then(|d| d.healthcheck_path.as_deref());
    let existing_healthcheck_timeout = existing_deploy.and_then(|d| d.healthcheck_timeout);

    let mut entries: Vec<PatchEntry> = Vec::new();
    let base_path = format!("services.{}.deploy", service_id);

    // Health check path
    let path_placeholder = existing_healthcheck_path.unwrap_or("None");
    let Some(path) = prompt_text_with_placeholder_disappear_skippable(
        "Health check endpoint <esc to skip>",
        path_placeholder,
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
    let timeout_placeholder = existing_healthcheck_timeout
        .map(|t| t.to_string())
        .unwrap_or_else(|| "300".to_string());
    loop {
        let Some(timeout_str) = prompt_text_with_placeholder_disappear_skippable(
            "Health check timeout in seconds <esc to skip>",
            &timeout_placeholder,
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
