use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use reqwest::Client;

use crate::{
    LinkedProject,
    client::{GQLClient, post_graphql},
    commands::{
        Configs,
        queries::{
            self,
            project::{
                ProjectProject, ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdgesNode,
                ProjectProjectServicesEdgesNode,
            },
        },
    },
    errors::RailwayError,
};

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

/// Get all service IDs that have instances in a given environment
pub fn get_service_ids_in_env(project: &ProjectProject, environment_id: &str) -> HashSet<String> {
    project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment_id)
        .map(|e| {
            e.node
                .service_instances
                .edges
                .iter()
                .map(|si| si.node.service_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Find a service instance within a specific environment
pub fn find_service_instance<'a>(
    project: &'a ProjectProject,
    environment_id: &str,
    service_id: &str,
) -> Option<&'a ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdgesNode> {
    project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment_id)
        .and_then(|e| {
            e.node
                .service_instances
                .edges
                .iter()
                .find(|si| si.node.service_id == service_id)
                .map(|si| &si.node)
        })
}

/// Resolved context for service operations (variables, etc.)
pub struct ServiceContext {
    pub client: Client,
    pub configs: Configs,
    pub project: ProjectProject,
    pub project_id: String,
    pub environment_id: String,
    pub service_id: String,
    pub service_name: String,
}

/// Resolves project, environment, and service from args and linked project.
/// Returns a ServiceContext with all resolved IDs.
pub async fn resolve_service_context(
    service_arg: Option<String>,
    environment_arg: Option<String>,
) -> Result<ServiceContext> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let env = environment_arg.unwrap_or(linked_project.environment.clone());
    let environment_id = get_matched_environment(&project, env)?.id;

    let services = &project.services.edges;
    let (service_id, service_name) = match (service_arg, linked_project.service) {
        (Some(service_arg), _) => {
            let service = services
                .iter()
                .find(|s| s.node.name == service_arg || s.node.id == service_arg)
                .with_context(|| format!("Service '{service_arg}' not found"))?;
            (service.node.id.clone(), service.node.name.clone())
        }
        (_, Some(linked_service)) => {
            let name = services
                .iter()
                .find(|s| s.node.id == linked_service)
                .map(|s| s.node.name.clone())
                .unwrap_or_else(|| linked_service.clone());
            (linked_service, name)
        }
        _ => bail!(RailwayError::NoServiceLinked),
    };

    Ok(ServiceContext {
        client,
        configs,
        project,
        project_id: linked_project.project,
        environment_id,
        service_id,
        service_name,
    })
}
