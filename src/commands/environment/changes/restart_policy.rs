use super::PatchEntry;
use crate::util::prompt::{
    prompt_options_skippable, prompt_text_with_placeholder_disappear_skippable,
};
use anyhow::Result;
use colored::Colorize;
use strum::{Display, EnumIter, IntoEnumIterator};

#[derive(Clone, Copy, Display, EnumIter)]
#[strum(serialize_all = "title_case")]
pub enum RestartPolicyType {
    Never,
    Always,
    OnFailure,
}

pub fn parse_interactive(service_id: &str, service_name: &str) -> Result<Vec<PatchEntry>> {
    let Some(policy_type) = prompt_options_skippable(
        &format!("What restart policy for {service_name}? <esc to skip>"),
        RestartPolicyType::iter().collect(),
    )?
    else {
        return Ok(vec![]);
    };

    let base_path = format!("services.{}.deploy", service_id);

    let result = match policy_type {
        RestartPolicyType::Never => vec![(
            format!("{}.restartPolicyType", base_path),
            serde_json::json!("NEVER"),
        )],
        RestartPolicyType::Always => vec![(
            format!("{}.restartPolicyType", base_path),
            serde_json::json!("ALWAYS"),
        )],
        RestartPolicyType::OnFailure => loop {
            let Some(input) = prompt_text_with_placeholder_disappear_skippable(
                "Enter max retries <esc to skip>",
                "<number>",
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
                            format!("{}.restartPolicyType", base_path),
                            serde_json::json!("ON_FAILURE"),
                        ),
                        (
                            format!("{}.restartPolicyMaxRetries", base_path),
                            serde_json::json!(max_retries),
                        ),
                    ];
                }
                Err(_) => {
                    eprintln!("{} Invalid number for max retries", "Warn".yellow());
                    continue;
                }
            }
        },
    };

    Ok(result)
}
