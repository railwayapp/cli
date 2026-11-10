use crate::{
    commands::queries::{
        RailwayProject,
        project::{ProjectProjectEnvironmentsEdges, ProjectProjectEnvironmentsEdgesNode},
    },
    errors::RailwayError,
};
use anyhow::{Result, bail};

pub fn get_matched_environment(
    project: &RailwayProject,
    environment: String,
) -> Result<ProjectProjectEnvironmentsEdgesNode> {
    Ok(get_matched_environment_edge(project, environment)?
        .node
        .clone())
}

pub fn get_matched_environment_edge<'a>(
    project: &'a RailwayProject,
    environment: String,
) -> Result<&'a ProjectProjectEnvironmentsEdges> {
    let environment = project
        .environments
        .edges
        .iter()
        .find(|env| env.node.name == environment || env.node.id == environment)
        .ok_or_else(|| RailwayError::EnvironmentNotFound(environment))?;

    ensure_environment_accessible(&environment.node)?;

    Ok(environment)
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
