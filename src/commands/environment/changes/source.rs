use super::PatchEntry;
use crate::util::prompt::{
    prompt_confirm_with_default_with_cancel, prompt_options_skippable,
    prompt_text_with_placeholder_disappear_skippable,
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

#[derive(Clone, Copy, Display, EnumIter)]
pub enum AutoUpdateType {
    #[strum(serialize = "Disabled")]
    Disabled,
    #[strum(serialize = "Patch versions only")]
    Patch,
    #[strum(serialize = "Minor versions")]
    Minor,
}

impl AutoUpdateType {
    fn to_api_value(self) -> &'static str {
        match self {
            AutoUpdateType::Disabled => "disabled",
            AutoUpdateType::Patch => "patch",
            AutoUpdateType::Minor => "minor",
        }
    }
}

pub fn parse_interactive(service_id: &str, service_name: &str) -> Result<Vec<PatchEntry>> {
    let Some(source_type) = prompt_options_skippable(
        &format!("What type of source for {service_name}? <esc to skip>"),
        SourceType::iter().collect(),
    )?
    else {
        return Ok(vec![]);
    };

    let base_path = format!("services.{}.source", service_id);

    let mut entries: Vec<PatchEntry> = Vec::new();

    match source_type {
        SourceType::Docker => {
            // Docker image (required)
            let image = loop {
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

                break image;
            };

            entries.push((format!("{}.image", base_path), serde_json::json!(image)));

            // Auto-updates (Docker only)
            if let Some(auto_update) = prompt_options_skippable(
                "Auto-update policy <esc to skip>",
                AutoUpdateType::iter().collect(),
            )? {
                entries.push((
                    format!("{}.autoUpdates.type", base_path),
                    serde_json::json!(auto_update.to_api_value()),
                ));
            }
        }
        SourceType::GitHub => {
            // GitHub repo (required)
            let (repo, branch) = loop {
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

                break (format!("{}/{}", parts[0], parts[1]), parts[2].to_string());
            };

            entries.push((format!("{}.repo", base_path), serde_json::json!(repo)));
            entries.push((format!("{}.branch", base_path), serde_json::json!(branch)));

            // Root directory (monorepos)
            if let Some(root_dir) = prompt_text_with_placeholder_disappear_skippable(
                "Root directory <esc to skip>",
                "/packages/backend",
            )? {
                if !root_dir.is_empty() {
                    entries.push((
                        format!("{}.rootDirectory", base_path),
                        serde_json::json!(root_dir),
                    ));
                }
            }

            // Check suites
            if let Some(check_suites) = prompt_confirm_with_default_with_cancel(
                "Wait for GitHub check suites before deploying? <esc to skip>",
                false,
            )? {
                entries.push((
                    format!("{}.checkSuites", base_path),
                    serde_json::json!(check_suites),
                ));
            }
        }
    }

    Ok(entries)
}
