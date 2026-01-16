use super::PatchEntry;
use crate::util::prompt::prompt_text_with_placeholder_disappear_skippable;
use anyhow::Result;

pub fn parse_interactive(service_id: &str, service_name: &str) -> Result<Vec<PatchEntry>> {
    let Some(start_command) = prompt_text_with_placeholder_disappear_skippable(
        &format!("Start command for {service_name}? <esc to skip>"),
        "npm start",
    )?
    else {
        return Ok(vec![]);
    };

    if start_command.is_empty() {
        return Ok(vec![]);
    }

    Ok(vec![(
        format!("services.{}.deploy.startCommand", service_id),
        serde_json::json!(start_command),
    )])
}
