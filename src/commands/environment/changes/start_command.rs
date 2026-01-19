use super::PatchEntry;
use crate::controllers::config::environment::ServiceInstance;
use crate::util::prompt::prompt_text_with_placeholder_disappear_skippable;
use anyhow::Result;

pub fn parse_interactive(
    service_id: &str,
    service_name: &str,
    existing: Option<&ServiceInstance>,
) -> Result<Vec<PatchEntry>> {
    let existing_start_command = existing
        .and_then(|e| e.deploy.as_ref())
        .and_then(|d| d.start_command.as_deref());
    let placeholder = existing_start_command.unwrap_or("None");

    let Some(start_command) = prompt_text_with_placeholder_disappear_skippable(
        &format!("Start command for {service_name}? <esc to skip>"),
        placeholder,
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
