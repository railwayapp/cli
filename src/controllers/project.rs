use reqwest::Client;

use crate::{
    client::post_graphql,
    commands::{
        queries::{
            self,
            project::{
                ProjectProject, ProjectProjectPluginsEdgesNode, ProjectProjectServicesEdgesNode,
            },
        },
        Configs,
    },
    errors::RailwayError,
};
use anyhow::{bail, Result};

pub enum PluginOrService {
    Plugin(ProjectProjectPluginsEdgesNode),
    Service(ProjectProjectServicesEdgesNode),
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

pub fn get_plugin_or_service(
    project: &ProjectProject,
    service_or_plugin_name: String,
) -> Result<PluginOrService> {
    let service = project
        .services
        .edges
        .iter()
        .find(|edge| edge.node.name.to_lowercase() == service_or_plugin_name);

    let plugin = project
        .plugins
        .edges
        .iter()
        .find(|edge| edge.node.friendly_name.to_lowercase() == service_or_plugin_name);

    if let Some(service) = service {
        return Ok(PluginOrService::Service(service.node.clone()));
    } else if let Some(plugin) = plugin {
        return Ok(PluginOrService::Plugin(plugin.node.clone()));
    }

    bail!(RailwayError::ServiceOrPluginNotFound(
        service_or_plugin_name
    ))
}
