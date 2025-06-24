use anyhow::bail;

use super::*;
use base64::prelude::*;
use std::path::Path;

use crate::{
    queries::project::{
        ProjectProject, ProjectProjectEnvironmentsEdges,
        ProjectProjectServicesEdgesNodeServiceInstancesEdges,
    },
    util::prompt::{fake_select, prompt_confirm_with_default},
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
