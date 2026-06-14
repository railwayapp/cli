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
            environment_instances::{
                EnvironmentInstancesEnvironmentServiceInstancesEdges,
                EnvironmentInstancesEnvironmentServiceInstancesEdgesNode,
                EnvironmentInstancesEnvironmentVolumeInstancesEdges,
                EnvironmentInstancesEnvironmentVolumeInstancesEdgesNode,
            },
            project::{ProjectProject, ProjectProjectServicesEdgesNode},
        },
    },
    errors::RailwayError,
    workspace::workspaces_with_client,
};

use super::environment::get_matched_environment;

pub type ProjectServiceInstanceEdge = EnvironmentInstancesEnvironmentServiceInstancesEdges;
pub type ProjectServiceInstanceNode = EnvironmentInstancesEnvironmentServiceInstancesEdgesNode;
pub type ProjectVolumeInstanceEdge = EnvironmentInstancesEnvironmentVolumeInstancesEdges;
pub type ProjectVolumeInstanceNode = EnvironmentInstancesEnvironmentVolumeInstancesEdgesNode;

const ENVIRONMENT_INSTANCE_PAGE_SIZE: i64 = 500;

#[derive(Debug, Clone, Default)]
pub struct ProjectEnvironmentInstances {
    pub service_instances: Vec<ProjectServiceInstanceEdge>,
    pub volume_instances: Vec<ProjectVolumeInstanceEdge>,
}

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

#[derive(Debug, Clone)]
struct ProjectChoice {
    id: String,
    name: String,
    workspace_name: String,
}

pub async fn resolve_project_id_or_name(
    client: &Client,
    configs: &Configs,
    project: &str,
) -> Result<String> {
    match get_project(client, configs, project.to_string()).await {
        Ok(project) => return Ok(project.id),
        Err(RailwayError::ProjectNotFound) => {}
        Err(RailwayError::GraphQLError(message)) if message.contains("Project not found") => {}
        Err(error) => return Err(error.into()),
    }

    let choices = project_choices(client, configs).await?;
    let id_matches = choices
        .iter()
        .filter(|choice| choice.id.eq_ignore_ascii_case(project))
        .cloned()
        .collect::<Vec<_>>();

    if let Some(choice) = single_project_match(id_matches, project, "project ID")? {
        return Ok(choice.id);
    }

    let name_matches = choices
        .into_iter()
        .filter(|choice| choice.name.eq_ignore_ascii_case(project))
        .collect::<Vec<_>>();

    if let Some(choice) = single_project_match(name_matches, project, "project name")? {
        return Ok(choice.id);
    }

    bail!("Project \"{}\" not found", project)
}

async fn project_choices(client: &Client, configs: &Configs) -> Result<Vec<ProjectChoice>> {
    let mut choices = Vec::new();
    for workspace in workspaces_with_client(client, configs).await? {
        let workspace_name = workspace.name().to_string();
        choices.extend(
            workspace
                .projects()
                .into_iter()
                .filter(|project| project.deleted_at().is_none())
                .map(|project| ProjectChoice {
                    id: project.id().to_string(),
                    name: project.name().to_string(),
                    workspace_name: workspace_name.clone(),
                }),
        );
    }
    Ok(choices)
}

fn single_project_match(
    matches: Vec<ProjectChoice>,
    input: &str,
    kind: &str,
) -> Result<Option<ProjectChoice>> {
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        _ => {
            let available = matches
                .iter()
                .map(|choice| format!("{} ({})", choice.id, choice.workspace_name))
                .collect::<Vec<_>>()
                .join(", ");
            bail!("Ambiguous {kind} \"{input}\". Use one of these project IDs: {available}");
        }
    }
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

    // Only validate the environment if one is linked; callers that need an
    // environment (or accept --environment) resolve and validate it themselves.
    if let Some(env_id_or_name) = linked_project
        .environment_name
        .clone()
        .or_else(|| linked_project.environment.clone())
    {
        let environment = get_matched_environment(&project, env_id_or_name);

        match environment {
            Ok(environment) => {
                if environment.deleted_at.is_some() {
                    bail!(RailwayError::EnvironmentDeleted);
                }
            }
            Err(error) => match error.downcast_ref::<RailwayError>() {
                Some(RailwayError::EnvironmentNotFound(_)) => {
                    bail!(RailwayError::EnvironmentDeleted);
                }
                Some(RailwayError::EnvironmentRestricted(_)) => return Err(error),
                _ => return Err(error),
            },
        };
    }

    Ok(())
}

pub async fn get_environment_instances(
    client: &Client,
    configs: &Configs,
    project_id: &str,
    environment_id: &str,
) -> Result<ProjectEnvironmentInstances> {
    let mut service_instances = Vec::new();
    let mut volume_instances = Vec::new();
    let mut service_after = None;
    let mut volume_after = None;
    let mut service_done = false;
    let mut volume_done = false;

    loop {
        let response = post_graphql::<queries::EnvironmentInstances, _>(
            client,
            configs.get_backboard(),
            queries::environment_instances::Variables {
                project_id: project_id.to_string(),
                environment_id: environment_id.to_string(),
                service_instances_first: Some(if service_done {
                    0
                } else {
                    ENVIRONMENT_INSTANCE_PAGE_SIZE
                }),
                service_instances_after: service_after.clone(),
                volume_instances_first: Some(if volume_done {
                    0
                } else {
                    ENVIRONMENT_INSTANCE_PAGE_SIZE
                }),
                volume_instances_after: volume_after.clone(),
            },
        )
        .await?;

        if !service_done {
            let connection = response.environment.service_instances;
            service_done = !connection.page_info.has_next_page;
            service_after = connection.page_info.end_cursor;
            service_instances.extend(connection.edges);
        }

        if !volume_done {
            let connection = response.environment.volume_instances;
            volume_done = !connection.page_info.has_next_page;
            volume_after = connection.page_info.end_cursor;
            volume_instances.extend(connection.edges);
        }

        if service_done && volume_done {
            break;
        }
    }

    Ok(ProjectEnvironmentInstances {
        service_instances,
        volume_instances,
    })
}

/// Get all service IDs that have instances in a given environment
pub fn get_service_ids_in_env(instances: &ProjectEnvironmentInstances) -> HashSet<String> {
    instances
        .service_instances
        .iter()
        .map(|si| si.node.service_id.clone())
        .collect()
}

/// Find all service instances within a specific environment.
pub fn service_instances_in_env<'a>(
    instances: &'a ProjectEnvironmentInstances,
) -> &'a [ProjectServiceInstanceEdge] {
    instances.service_instances.as_slice()
}

/// Find all volume instances within a specific environment.
pub fn volume_instances_in_env<'a>(
    instances: &'a ProjectEnvironmentInstances,
) -> &'a [ProjectVolumeInstanceEdge] {
    instances.volume_instances.as_slice()
}

/// Find a service instance within a specific environment
pub fn find_service_instance<'a>(
    instances: &'a ProjectEnvironmentInstances,
    service_id: &str,
) -> Option<&'a ProjectServiceInstanceNode> {
    instances
        .service_instances
        .iter()
        .find(|si| si.node.service_id == service_id)
        .map(|si| &si.node)
}

pub fn ensure_service_has_active_deployment(
    service_instance: &ProjectServiceInstanceNode,
    environment_name: &str,
) -> Result<()> {
    if service_instance.active_deployments.is_empty() {
        bail!(
            "Service {} has no active deployment in environment {}. Deploy or restart the service before using file commands.",
            service_instance.service_name,
            environment_name
        );
    }

    Ok(())
}

/// Resolved context for service operations (variables, etc.)
pub struct ServiceContext {
    pub client: Client,
    pub configs: Configs,
    pub project: ProjectProject,
    pub project_id: String,
    pub environment_id: String,
    pub environment_name: String,
    pub service_id: String,
    pub service_name: String,
}

/// Resolves project, environment, and service from args and linked project.
/// When project_arg is provided, environment_arg must also be provided.
pub async fn resolve_service_context(
    project_arg: Option<String>,
    service_arg: Option<String>,
    environment_arg: Option<String>,
) -> Result<ServiceContext> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    if project_arg.is_some() && environment_arg.is_none() {
        bail!("--environment is required when using --project");
    }

    let linked_project = if project_arg.is_none() {
        Some(configs.get_linked_project().await?)
    } else {
        None
    };

    if let Some(ref linked_project) = linked_project {
        ensure_project_and_environment_exist(&client, &configs, linked_project).await?;
    }

    let project_id = if let Some(project_arg) = project_arg {
        resolve_project_id_or_name(&client, &configs, &project_arg).await?
    } else {
        linked_project
            .as_ref()
            .map(|lp| lp.project.clone())
            .ok_or_else(|| {
                anyhow::anyhow!("No project specified. Use --project or run `railway link` first")
            })?
    };

    let project = get_project(&client, &configs, project_id.clone()).await?;

    let env = match environment_arg {
        Some(env) => env,
        None => linked_project
            .as_ref()
            .context("No environment linked. Use --environment when using --project")?
            .environment_id()?
            .to_string(),
    };
    let environment = get_matched_environment(&project, env)?;
    let environment_id = environment.id;
    let environment_name = environment.name;

    let linked_service = linked_project.and_then(|lp| lp.service);
    let services = &project.services.edges;
    if services.is_empty() {
        bail!(RailwayError::ProjectHasNoServices);
    }

    let (service_id, service_name) = match (service_arg, linked_service) {
        (Some(service_arg), _) => {
            let service = services
                .iter()
                .find(|s| {
                    s.node.name.eq_ignore_ascii_case(&service_arg) || s.node.id == service_arg
                })
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
        _ if services.len() == 1 => {
            let service = &services[0].node;
            (service.id.clone(), service.name.clone())
        }
        _ => bail!(RailwayError::NoServiceLinked),
    };

    Ok(ServiceContext {
        client,
        configs,
        project,
        project_id,
        environment_id,
        environment_name,
        service_id,
        service_name,
    })
}
