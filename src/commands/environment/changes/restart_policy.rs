use super::PatchEntry;
use crate::controllers::config::environment::ServiceInstance;
use crate::util::prompt::{
    prompt_options_skippable_with_default, prompt_text_with_placeholder_disappear_skippable,
};
use anyhow::Result;
use colored::Colorize;
use strum::{Display, EnumIter, IntoEnumIterator};

#[derive(Clone, Copy, Display, EnumIter, PartialEq)]
#[strum(serialize_all = "title_case")]
pub enum RestartPolicyType {
    Never,
    Always,
    OnFailure,
}

impl RestartPolicyType {
    fn from_api_value(value: &str) -> Option<Self> {
        match value {
            "NEVER" => Some(Self::Never),
            "ALWAYS" => Some(Self::Always),
            "ON_FAILURE" => Some(Self::OnFailure),
            _ => None,
        }
    }
}

pub fn parse_interactive(
    service_id: &str,
    service_name: &str,
    existing: Option<&ServiceInstance>,
) -> Result<Vec<PatchEntry>> {
    let existing_deploy = existing.and_then(|e| e.deploy.as_ref());
    let existing_policy_type = existing_deploy
        .and_then(|d| d.restart_policy_type.as_deref())
        .and_then(RestartPolicyType::from_api_value);
    let existing_max_retries = existing_deploy.and_then(|d| d.restart_policy_max_retries);

    let options: Vec<RestartPolicyType> = RestartPolicyType::iter().collect();
    let default_index = existing_policy_type
        .and_then(|p| options.iter().position(|o| *o == p))
        .unwrap_or(0);

    let Some(policy_type) = prompt_options_skippable_with_default(
        &format!("What restart policy for {service_name}? <esc to skip>"),
        options,
        default_index,
    )?
    else {
        return Ok(vec![]);
    };

    let base_path = format!("services.{service_id}.deploy");

    let result = match policy_type {
        RestartPolicyType::Never => vec![(
            format!("{base_path}.restartPolicyType"),
            serde_json::json!("NEVER"),
        )],
        RestartPolicyType::Always => vec![(
            format!("{base_path}.restartPolicyType"),
            serde_json::json!("ALWAYS"),
        )],
        RestartPolicyType::OnFailure => {
            let retries_placeholder = existing_max_retries
                .map(|r| r.to_string())
                .unwrap_or_else(|| "<number>".to_string());
            loop {
                let Some(input) = prompt_text_with_placeholder_disappear_skippable(
                    "Enter max retries <esc to skip>",
                    &retries_placeholder,
                )?
                else {
                    return Ok(vec![]);
                };

                if input.is_empty() {
                    eprintln!("{} Max retries cannot be empty", "Warn".yellow());
                    continue;
                }

                match input.parse::<u16>() {
                    Ok(max_retries) => {
                        break vec![
                            (
                                format!("{base_path}.restartPolicyType"),
                                serde_json::json!("ON_FAILURE"),
                            ),
                            (
                                format!("{base_path}.restartPolicyMaxRetries"),
                                serde_json::json!(max_retries),
                            ),
                        ];
                    }
                    Err(_) => {
                        eprintln!("{} Invalid number for max retries", "Warn".yellow());
                        continue;
                    }
                }
            }
        }
    };

    Ok(result)
}
