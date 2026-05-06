use crate::{
    commands::queries::{RailwayProject, project::ProjectProjectEnvironmentsEdgesNode},
    errors::RailwayError,
};
use anyhow::{Result, bail};

pub fn get_matched_environment(
    project: &RailwayProject,
    environment: String,
) -> Result<ProjectProjectEnvironmentsEdgesNode> {
    let environment = project
        .environments
        .edges
        .iter()
        .find(|env| env.node.name == environment || env.node.id == environment)
        .ok_or_else(|| RailwayError::EnvironmentNotFound(environment))?;

    if !environment.node.can_access {
        bail!(
            "Environment \"{}\" is restricted. Ask a workspace admin for access, or choose an unrestricted environment.",
            environment.node.name
        );
    }

    Ok(environment.node.clone())
}
