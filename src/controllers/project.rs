use reqwest::Client;

use crate::{
    client::post_graphql,
    commands::{
        queries::{
            self,
            project::{ProjectProject, ProjectProjectServicesEdgesNode},
        },
        Configs,
    },
    errors::RailwayError,
    LinkedProject,
};
use anyhow::{bail, Result};

use super::environment::get_matched_environment;

pub async fn get_project(
    client: &Client,
    configs: &Configs,
    project_id: String,
) -> Result<queries::RailwayProject, RailwayError> {
    let vars = queries::project::Variables { id: project_id };

    let project = post_graphql::<queries::Project, _>(client, configs.get_backboard(), vars)
        .await
        .map_err(|e| {
            if let RailwayError::GraphQLError(msg) = &e {
                if msg.contains("Project not found") {
                    return RailwayError::ProjectNotFound;
                }
            }

            e
        })?
        .project;

    Ok(project)
}

pub fn get_service(
    project: &ProjectProject,
    service_name: String,
) -> Result<ProjectProjectServicesEdgesNode> {
    let service = project
        .services
        .edges
        .iter()
        .find(|edge| edge.node.name.to_lowercase() == service_name.to_lowercase());

    if let Some(service) = service {
        return Ok(service.node.clone());
    }

    bail!(RailwayError::ServiceNotFound(service_name))
}

pub async fn ensure_project_and_environment_exist(
    client: &Client,
    configs: &Configs,
    linked_project: &LinkedProject,
) -> Result<()> {
    let project = get_project(client, configs, linked_project.project.clone()).await?;

    if project.deleted_at.is_some() {
        bail!(RailwayError::ProjectDeleted);
    }

    let environment = get_matched_environment(
        &project,
        linked_project
            .environment_name
            .clone()
            .unwrap_or("Production".to_string()),
    );

    match environment {
        Ok(environment) => {
            if environment.deleted_at.is_some() {
                bail!(RailwayError::EnvironmentDeleted);
            }
        }
        Err(_) => bail!(RailwayError::EnvironmentDeleted),
    };

    Ok(())
}
