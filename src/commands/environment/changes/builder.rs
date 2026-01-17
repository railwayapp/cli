use super::PatchEntry;
use crate::controllers::config::environment::ServiceInstance;
use crate::util::prompt::{
    prompt_options_skippable_with_default, prompt_text_with_placeholder_disappear_skippable,
};
use anyhow::Result;
use strum::{Display, EnumIter, IntoEnumIterator};

#[derive(Clone, Copy, Display, EnumIter, PartialEq)]
pub enum BuilderType {
    Nixpacks,
    Dockerfile,
    Railpack,
}

impl BuilderType {
    fn from_api_value(value: &str) -> Option<Self> {
        match value {
            "NIXPACKS" => Some(Self::Nixpacks),
            "DOCKERFILE" => Some(Self::Dockerfile),
            "RAILPACK" => Some(Self::Railpack),
            _ => None,
        }
    }
}

pub fn parse_interactive(
    service_id: &str,
    service_name: &str,
    existing: Option<&ServiceInstance>,
) -> Result<Vec<PatchEntry>> {
    // Extract existing build config for placeholders
    let existing_build = existing.and_then(|e| e.build.as_ref());
    let existing_builder = existing_build
        .and_then(|b| b.builder.as_deref())
        .and_then(BuilderType::from_api_value);
    let existing_nixpacks_config = existing_build.and_then(|b| b.nixpacks_config_path.as_deref());
    let existing_dockerfile_path = existing_build.and_then(|b| b.dockerfile_path.as_deref());
    let existing_watch_patterns = existing_build.and_then(|b| b.watch_patterns.as_ref());

    let options: Vec<BuilderType> = BuilderType::iter().collect();
    let default_index = existing_builder
        .and_then(|b| options.iter().position(|o| *o == b))
        .unwrap_or(0);

    let Some(builder_type) = prompt_options_skippable_with_default(
        &format!("What builder for {service_name}? <esc to skip>"),
        options,
        default_index,
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
            let nixpacks_placeholder = existing_nixpacks_config.unwrap_or("nixpacks.toml");
            if let Some(config_path) = prompt_text_with_placeholder_disappear_skippable(
                "Nixpacks config path <esc to skip>",
                nixpacks_placeholder,
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
            let dockerfile_placeholder = existing_dockerfile_path.unwrap_or("Dockerfile");
            if let Some(dockerfile_path) = prompt_text_with_placeholder_disappear_skippable(
                "Dockerfile path <esc to skip>",
                dockerfile_placeholder,
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
    let watch_placeholder = existing_watch_patterns
        .map(|p| p.join(","))
        .unwrap_or_else(|| "src/**,package.json".to_string());
    if let Some(patterns) = prompt_text_with_placeholder_disappear_skippable(
        "Watch patterns (comma-separated) <esc to skip>",
        &watch_placeholder,
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
