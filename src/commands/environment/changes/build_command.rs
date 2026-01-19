use super::PatchEntry;
use crate::controllers::config::environment::ServiceInstance;
use crate::util::prompt::prompt_text_with_placeholder_disappear_skippable;
use anyhow::Result;

pub fn parse_interactive(
    service_id: &str,
    service_name: &str,
    existing: Option<&ServiceInstance>,
) -> Result<Vec<PatchEntry>> {
    let existing_build_command = existing
        .and_then(|e| e.build.as_ref())
        .and_then(|b| b.build_command.as_deref());
    let placeholder = existing_build_command.unwrap_or("None");

    let Some(build_command) = prompt_text_with_placeholder_disappear_skippable(
        &format!("Build command for {service_name}? <esc to skip>"),
        placeholder,
    )?
    else {
        return Ok(vec![]);
    };

    if build_command.is_empty() {
        return Ok(vec![]);
    }

    Ok(vec![(
        format!("services.{}.build.buildCommand", service_id),
        serde_json::json!(build_command),
    )])
}
