use super::PatchEntry;
use crate::util::prompt::prompt_variables;
use anyhow::Result;

pub fn parse_interactive(service_id: &str, service_name: &str) -> Result<Vec<PatchEntry>> {
    let variables = prompt_variables(Some(service_name))?;
    Ok(variables
        .into_iter()
        .map(|v| {
            (
                format!("services.{}.variables.{}", service_id, v.key),
                serde_json::json!({ "value": v.value }),
            )
        })
        .collect())
}
