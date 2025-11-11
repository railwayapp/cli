use anyhow::bail;
use is_terminal::IsTerminal;
use pathdiff::diff_paths;
use similar::{ChangeTag, TextDiff};

use super::*;
use base64::prelude::*;
use std::{path::Path, str::FromStr};

use crate::{
    queries::project::{
        ProjectProject, ProjectProjectEnvironmentsEdges,
        ProjectProjectServicesEdgesNodeServiceInstancesEdges,
    },
    util::prompt::{fake_select, prompt_confirm_with_default, prompt_path},
};

pub fn get_functions_in_environment<'a>(
    project: &'a ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
) -> Vec<&'a ProjectProjectServicesEdgesNodeServiceInstancesEdges> {
    project
        .services
        .edges
        .iter()
        .filter_map(|service| {
            service
                .node
                .service_instances
                .edges
                .iter()
                .find(|instance| instance.node.environment_id == environment.node.id)
        })
        .filter(|service_instance| is_function_service(service_instance))
        .collect()
}

pub fn link_function(path: &Path, id: &str) -> Result<()> {
    let mut c = Configs::new()?;
    c.link_function(path.to_path_buf(), id.to_owned())?;
    c.write()?;
    Ok(())
}

pub fn unlink_function(id: &str) -> Result<()> {
    let mut c = Configs::new()?;
    c.unlink_function(id.to_owned())?;
    c.write()?;
    Ok(())
}

fn is_function_service(
    service_instance: &ProjectProjectServicesEdgesNodeServiceInstancesEdges,
) -> bool {
    service_instance.node.source.clone().is_some_and(|source| {
        source
            .image
            .unwrap_or_default()
            .starts_with("ghcr.io/railwayapp/function")
    })
}

pub fn confirm(arg: Option<bool>, terminal: bool, message: &str) -> Result<bool> {
    let yes = arg.unwrap_or(false);

    if yes {
        fake_select(message, "Yes");
        Ok(true)
    } else if arg.is_some() && !yes {
        fake_select(message, "No");
        Ok(false)
    } else if terminal {
        prompt_confirm_with_default(message, false)
    } else {
        bail!(
            "The skip confirmation flag (-y,--yes) must be provided when not running in a terminal"
        )
    }
}

pub fn get_start_cmd(path: &Path) -> Result<String> {
    let content = std::fs::read(path)?;
    let cmd = format!("./run.sh {}", BASE64_STANDARD.encode(content));

    if cmd.len() >= 96 * 1024 {
        bail!("Your function is too large (must be smaller than 96kb base64)");
    }

    Ok(cmd)
}

pub fn get_function_from_path(path: Option<PathBuf>) -> Result<(String, PathBuf)> {
    let configs = Configs::new()?;
    let terminal = std::io::stdout().is_terminal();
    let closest = configs.get_closest_linked_project_directory()?;
    let p = PathBuf::from_str(closest.as_str())?;
    let functions = configs.get_functions_in_directory(p)?;
    let path = if let Some(path) = path {
        fake_select(
            "Enter the path to your function",
            &path.display().to_string(),
        );
        path
    } else if functions.len() == 1 {
        let p = functions.first().unwrap();
        let diff = diff_paths(&p.0, std::env::current_dir()?);
        let display = if let Some(diffed) = diff {
            diffed.display().to_string()
        } else {
            p.0.display().to_string()
        };
        fake_select("Enter the path to your function", &display);
        p.0.clone()
    } else if terminal {
        prompt_path("Enter the path of your function")?
    } else {
        bail!("Path must be provided when not running in a terminal");
    };
    if !path.exists() {
        bail!("The path provided must exist");
    }
    let id = match configs.get_function(path.clone())? {
        Some(id) => id,
        None => bail!(
            "The provided path ({}) hasn't been linked to any functions. Run `railway functions link` to link a function.",
            path.clone().display()
        ),
    };
    Ok((id, path.clone()))
}

pub fn has_domains(function: &ProjectProjectServicesEdgesNodeServiceInstancesEdges) -> bool {
    !function.node.domains.custom_domains.is_empty()
        || !function.node.domains.service_domains.is_empty()
}

pub fn find_service(
    project: &ProjectProject,
    environment: &ProjectProjectEnvironmentsEdges,
    id: &str,
) -> Option<queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdgesNode> {
    let c = common::get_functions_in_environment(project, environment);
    c.iter()
        .find(|f| f.node.service_id == id)
        .map(|f| f.node.clone())
}

#[derive(Debug)]
pub struct DiffStats {
    pub insertions: usize,
    pub deletions: usize,
    pub changes: usize,
}

impl std::fmt::Display for DiffStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "({} insertions, {} deletions, {} changes)",
            self.insertions, self.deletions, self.changes
        )
    }
}

pub fn calculate_diff(old_content: &str, new_content: &str) -> DiffStats {
    let diff = TextDiff::from_lines(old_content, new_content);
    let mut insertions = 0;
    let mut deletions = 0;
    let mut changes = 0;

    for group in diff.grouped_ops(0) {
        for op in &group {
            for change in diff.iter_changes(op) {
                match change.tag() {
                    ChangeTag::Delete => deletions += 1,
                    ChangeTag::Insert => insertions += 1,
                    ChangeTag::Equal => {}
                }
            }

            if op.tag() == similar::DiffTag::Replace {
                changes += 1;
            }
        }
    }

    DiffStats {
        insertions,
        deletions,
        changes,
    }
}

pub fn extract_function_content(
    function: &queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdgesNode,
) -> Result<String> {
    let cmd = function
        .start_command
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Function has no start command"))?;

    let encoded = cmd.split(' ').next_back().ok_or_else(|| {
        anyhow::anyhow!("Function no longer uses the correct start command format")
    })?;

    String::from_utf8(BASE64_STANDARD.decode(encoded)?)
        .map_err(|e| anyhow::anyhow!("Failed to decode function content: {}", e))
}
