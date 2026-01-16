use super::PatchEntry;
use crate::util::prompt::{
    prompt_options_skippable, prompt_text_with_placeholder_disappear_skippable,
};
use anyhow::Result;
use strum::{Display, EnumIter, IntoEnumIterator};

#[derive(Clone, Copy, Display, EnumIter)]
pub enum BuilderType {
    Nixpacks,
    Dockerfile,
    Railpack,
}

pub fn parse_interactive(service_id: &str, service_name: &str) -> Result<Vec<PatchEntry>> {
    let Some(builder_type) = prompt_options_skippable(
        &format!("What builder for {service_name}? <esc to skip>"),
        BuilderType::iter().collect(),
    )?
    else {
        return Ok(vec![]);
    };

    let mut entries: Vec<PatchEntry> = Vec::new();
    let base_path = format!("services.{}.build", service_id);

    match builder_type {
        BuilderType::Nixpacks => {
            entries.push((
                format!("{}.builder", base_path),
                serde_json::json!("NIXPACKS"),
            ));

            // Optionally prompt for nixpacks config path
            if let Some(config_path) = prompt_text_with_placeholder_disappear_skippable(
                "Nixpacks config path <esc to skip>",
                "nixpacks.toml",
            )? {
                if !config_path.is_empty() {
                    entries.push((
                        format!("{}.nixpacksConfigPath", base_path),
                        serde_json::json!(config_path),
                    ));
                }
            }
        }
        BuilderType::Dockerfile => {
            entries.push((
                format!("{}.builder", base_path),
                serde_json::json!("DOCKERFILE"),
            ));

            // Optionally prompt for dockerfile path
            if let Some(dockerfile_path) = prompt_text_with_placeholder_disappear_skippable(
                "Dockerfile path <esc to skip>",
                "Dockerfile",
            )? {
                if !dockerfile_path.is_empty() {
                    entries.push((
                        format!("{}.dockerfilePath", base_path),
                        serde_json::json!(dockerfile_path),
                    ));
                }
            }
        }
        BuilderType::Railpack => {
            entries.push((
                format!("{}.builder", base_path),
                serde_json::json!("RAILPACK"),
            ));
            // Railpack doesn't have a config path option
        }
    }

    // Watch patterns (applies to all builders)
    if let Some(patterns) = prompt_text_with_placeholder_disappear_skippable(
        "Watch patterns (comma-separated) <esc to skip>",
        "src/**,package.json",
    )? {
        if !patterns.is_empty() {
            let patterns_vec: Vec<String> = patterns
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            if !patterns_vec.is_empty() {
                entries.push((
                    format!("{}.watchPatterns", base_path),
                    serde_json::json!(patterns_vec),
                ));
            }
        }
    }

    Ok(entries)
}
