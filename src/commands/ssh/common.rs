use anyhow::{Context, Result, anyhow};
use reqwest::Client;

use crate::commands::queries::RailwayProject;
use crate::config::Configs;
use crate::controllers::{
    environment::get_matched_environment, project::get_project, service::get_or_prompt_service,
};

use super::Args;

pub struct SshConnectParams {
    pub environment_id: String,
    pub service_id: String,
}

pub async fn find_service_by_name(
    client: &Client,
    configs: &Configs,
    project: &RailwayProject,
    service_id_or_name: &str,
) -> Result<String> {
    let project = get_project(client, configs, project.id.clone()).await?;

    let services = project.services.edges.iter().collect::<Vec<_>>();

    let service = services
        .iter()
        .find(|service| {
            service.node.name.to_lowercase() == service_id_or_name.to_lowercase()
                || service.node.id == service_id_or_name
        })
        .with_context(|| format!("Service '{service_id_or_name}' not found"))?
        .node
        .id
        .to_owned();

    Ok(service)
}

pub async fn get_ssh_connect_params(
    args: Args,
    configs: &Configs,
    client: &Client,
) -> Result<SshConnectParams> {
    let needs_linked_project =
        args.project.is_none() || args.environment.is_none() || args.service.is_none();

    let linked_project = if needs_linked_project {
        Some(configs.get_linked_project().await?)
    } else {
        None
    };

    let project_id = if let Some(id) = args.project {
        id
    } else {
        linked_project.as_ref().unwrap().project.clone()
    };
    let project = get_project(client, configs, project_id.clone()).await?;

    let environment = if let Some(env) = args.environment {
        env
    } else {
        linked_project
            .as_ref()
            .unwrap()
            .environment_id()?
            .to_string()
    };
    let environment_id = get_matched_environment(&project, environment)?.id;

    let service_id = if let Some(service_id_or_name) = args.service {
        find_service_by_name(client, configs, &project, &service_id_or_name).await?
    } else {
        get_or_prompt_service(linked_project.clone(), project, None)
            .await?
            .ok_or_else(|| anyhow!("No service found. Please specify a service to connect to via the `--service` flag."))?
    };

    Ok(SshConnectParams {
        environment_id,
        service_id,
    })
}
