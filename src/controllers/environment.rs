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

    ensure_environment_accessible(&environment.node)?;

    Ok(environment.node.clone())
}

pub fn ensure_environment_accessible(
    environment: &ProjectProjectEnvironmentsEdgesNode,
) -> Result<()> {
    if !environment.can_access {
        bail!(RailwayError::EnvironmentRestricted(
            environment.name.clone()
        ));
    }

    Ok(())
}
