use crate::{
    commands::queries::{project::ProjectProjectEnvironmentsEdgesNode, RailwayProject},
    errors::RailwayError,
};
use anyhow::Result;

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

    Ok(environment.node.clone())
}
