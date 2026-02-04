use super::PatchEntry;
use crate::controllers::config::environment::ServiceInstance;
use crate::util::prompt::{
    prompt_confirm_with_default_with_cancel, prompt_options_skippable_with_default,
    prompt_text_with_placeholder_disappear_skippable,
};
use anyhow::Result;
use colored::Colorize;
use strum::{Display, EnumIter, IntoEnumIterator};

#[derive(Clone, Copy, Display, EnumIter, PartialEq)]
pub enum SourceType {
    #[strum(serialize = "Docker image")]
    Docker,
    #[strum(serialize = "GitHub repo")]
    GitHub,
}

pub fn parse_interactive(
    service_id: &str,
    service_name: &str,
    existing: Option<&ServiceInstance>,
) -> Result<Vec<PatchEntry>> {
    // Extract existing source info for placeholders
    let existing_source = existing.and_then(|e| e.source.as_ref());
    let existing_image = existing_source.and_then(|s| s.image.as_deref());
    let existing_repo = existing_source.and_then(|s| s.repo.as_deref());
    let existing_branch = existing_source.and_then(|s| s.branch.as_deref());
    let existing_root_dir = existing_source.and_then(|s| s.root_directory.as_deref());
    let existing_check_suites = existing_source.and_then(|s| s.check_suites);

    // Determine default source type based on existing config
    let default_source_index = if existing_image.is_some() {
        0 // Docker
    } else if existing_repo.is_some() {
        1 // GitHub
    } else {
        0 // Default to Docker
    };

    let Some(source_type) = prompt_options_skippable_with_default(
        &format!("What type of source for {service_name}? <esc to skip>"),
        SourceType::iter().collect(),
        default_source_index,
    )?
    else {
        return Ok(vec![]);
    };

    let base_path = format!("services.{service_id}.source");

    let mut entries: Vec<PatchEntry> = Vec::new();

    match source_type {
        SourceType::Docker => {
            // Docker image (required)
            let image_placeholder = existing_image.unwrap_or("<image:tag>");
            let image = loop {
                let Some(image) = prompt_text_with_placeholder_disappear_skippable(
                    "Enter docker image <esc to skip>",
                    image_placeholder,
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

            entries.push((format!("{base_path}.image"), serde_json::json!(image)));
        }
        SourceType::GitHub => {
            // GitHub repo (required)
            let repo_placeholder = match (existing_repo, existing_branch) {
                (Some(repo), Some(branch)) => format!("{repo}/{branch}"),
                _ => "<owner/repo/branch>".to_string(),
            };
            let (repo, branch) = loop {
                let Some(repo) = prompt_text_with_placeholder_disappear_skippable(
                    "Enter repo <esc to skip>",
                    &repo_placeholder,
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

            entries.push((format!("{base_path}.repo"), serde_json::json!(repo)));
            entries.push((format!("{base_path}.branch"), serde_json::json!(branch)));

            // Root directory (monorepos)
            let root_dir_placeholder = existing_root_dir.unwrap_or("/packages/backend");
            if let Some(root_dir) = prompt_text_with_placeholder_disappear_skippable(
                "Root directory <esc to skip>",
                root_dir_placeholder,
            )? {
                if !root_dir.is_empty() {
                    entries.push((
                        format!("{base_path}.rootDirectory"),
                        serde_json::json!(root_dir),
                    ));
                }
            }

            // Check suites
            let check_suites_default = existing_check_suites.unwrap_or(false);
            if let Some(check_suites) = prompt_confirm_with_default_with_cancel(
                "Wait for GitHub check suites before deploying? <esc to skip>",
                check_suites_default,
            )? {
                entries.push((
                    format!("{base_path}.checkSuites"),
                    serde_json::json!(check_suites),
                ));
            }
        }
    }

    Ok(entries)
}
