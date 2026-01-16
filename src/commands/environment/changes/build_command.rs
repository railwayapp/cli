use super::PatchEntry;
use crate::util::prompt::prompt_text_with_placeholder_disappear_skippable;
use anyhow::Result;

pub fn parse_interactive(service_id: &str, service_name: &str) -> Result<Vec<PatchEntry>> {
    let Some(build_command) = prompt_text_with_placeholder_disappear_skippable(
        &format!("Build command for {service_name}? <esc to skip>"),
        "npm run build",
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
