use super::PatchEntry;
use crate::util::prompt::prompt_text_with_placeholder_disappear_skippable;
use anyhow::Result;

pub fn parse_interactive(service_id: &str, service_name: &str) -> Result<Vec<PatchEntry>> {
    let mut entries: Vec<PatchEntry> = Vec::new();

    // Start command (deploy config)
    if let Some(start_cmd) = prompt_text_with_placeholder_disappear_skippable(
        &format!("Start command for {service_name}? <esc to skip>"),
        "npm start",
    )? {
        if !start_cmd.is_empty() {
            entries.push((
                format!("services.{}.deploy.startCommand", service_id),
                serde_json::json!(start_cmd),
            ));
        }
    }

    // Build command (build config)
    if let Some(build_cmd) = prompt_text_with_placeholder_disappear_skippable(
        &format!("Build command for {service_name}? <esc to skip>"),
        "npm run build",
    )? {
        if !build_cmd.is_empty() {
            entries.push((
                format!("services.{}.build.buildCommand", service_id),
                serde_json::json!(build_cmd),
            ));
        }
    }

    Ok(entries)
}
