use super::PatchEntry;
use crate::util::prompt::{
    prompt_options_skippable, prompt_text_with_placeholder_disappear_skippable,
};
use anyhow::Result;
use colored::Colorize;
use strum::{Display, EnumIter, IntoEnumIterator};

#[derive(Clone, Copy, Display, EnumIter)]
pub enum SourceType {
    #[strum(serialize = "Docker image")]
    Docker,
    #[strum(serialize = "GitHub repo")]
    GitHub,
}

pub fn parse_interactive(service_id: &str, service_name: &str) -> Result<Vec<PatchEntry>> {
    let Some(source_type) = prompt_options_skippable(
        &format!("What type of source for {service_name}? <esc to skip>"),
        SourceType::iter().collect(),
    )?
    else {
        return Ok(vec![]);
    };

    let path = format!("services.{}.source", service_id);

    let result = match source_type {
        SourceType::Docker => loop {
            let Some(image) = prompt_text_with_placeholder_disappear_skippable(
                "Enter docker image <esc to skip>",
                "<image:tag>",
            )?
            else {
                return Ok(vec![]);
            };

            if image.is_empty() {
                eprintln!("{} Docker image cannot be empty", "Warn".yellow());
                continue;
            }

            break (
                path,
                serde_json::json!({
                    "image": image,
                }),
            );
        },
        SourceType::GitHub => loop {
            let Some(repo) = prompt_text_with_placeholder_disappear_skippable(
                "Enter repo <esc to skip>",
                "<owner/repo/branch>",
            )?
            else {
                return Ok(vec![]);
            };

            if repo.is_empty() {
                eprintln!("{} Repo cannot be empty", "Warn".yellow());
                continue;
            }

            let parts: Vec<&str> = repo.splitn(3, '/').collect();
            if parts.len() != 3 {
                eprintln!(
                    "{} Malformed repo: expected owner/repo/branch",
                    "Warn".yellow()
                );
                continue;
            }

            break (
                path,
                serde_json::json!({
                    "repo": format!("{}/{}", parts[0], parts[1]),
                    "branch": parts[2],
                }),
            );
        },
    };

    Ok(vec![result])
}
