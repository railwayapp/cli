use super::PatchEntry;
use crate::controllers::config::environment::ServiceInstance;
use crate::util::prompt::prompt_text_with_placeholder_disappear_skippable;
use anyhow::Result;

pub fn parse_interactive(
    service_id: &str,
    service_name: &str,
    existing: Option<&ServiceInstance>,
) -> Result<Vec<PatchEntry>> {
    let existing_watch_patterns = existing
        .and_then(|e| e.build.as_ref())
        .and_then(|b| b.watch_patterns.as_ref());

    let placeholder = existing_watch_patterns
        .map(|p| p.join(","))
        .unwrap_or_else(|| "None".to_string());

    let Some(patterns) = prompt_text_with_placeholder_disappear_skippable(
        &format!("Watch patterns for {service_name}? (comma-separated) <esc to skip>"),
        &placeholder,
    )?
    else {
        return Ok(vec![]);
    };

    if patterns.is_empty() {
        return Ok(vec![]);
    }

    let patterns_vec: Vec<String> = patterns
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if patterns_vec.is_empty() {
        return Ok(vec![]);
    }

    Ok(vec![(
        format!("services.{}.build.watchPatterns", service_id),
        serde_json::json!(patterns_vec),
    )])
}
