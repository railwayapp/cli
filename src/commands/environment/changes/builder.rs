use super::PatchEntry;
use crate::controllers::config::environment::ServiceInstance;
use crate::util::prompt::{
    prompt_options_skippable_with_default, prompt_text_with_placeholder_disappear_skippable,
};
use anyhow::Result;
use strum::{Display, EnumIter, IntoEnumIterator};

#[derive(Clone, Copy, Display, EnumIter, PartialEq)]
pub enum BuilderType {
    Dockerfile,
    Railpack,
}

impl BuilderType {
    fn from_api_value(value: &str) -> Option<Self> {
        match value {
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
    let existing_dockerfile_path = existing_build.and_then(|b| b.dockerfile_path.as_deref());

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

    Ok(entries)
}
