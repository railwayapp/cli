use crate::{
    client::post_graphql,
    commands::{queries, Configs},
};
use anyhow::Result;
use reqwest::Client;
use std::collections::BTreeMap;

use super::project::PluginOrService;

pub async fn get_service_variables(
    client: &Client,
    configs: &Configs,
    project_id: String,
    environment_id: String,
    service_id: String,
) -> Result<BTreeMap<String, String>> {
    let vars = queries::variables_for_service_deployment::Variables {
        project_id,
        environment_id,
        service_id,
    };
    let variables = post_graphql::<queries::VariablesForServiceDeployment, _>(
        client,
        configs.get_backboard(),
        vars,
    )
    .await?
    .variables_for_service_deployment;

    Ok(variables)
}

// note - this is only for projects with no services
pub async fn get_all_plugin_variables(
    client: &Client,
    configs: &Configs,
    project_id: String,
    environment_id: String,
    plugins: &[String],
) -> Result<BTreeMap<String, String>> {
    let mut plugin_variables = BTreeMap::new();
    for plugin in plugins {
        let mut vars = get_plugin_variables(
            client,
            configs,
            project_id.clone(),
            environment_id.clone(),
            plugin.clone(),
        )
        .await?;
        plugin_variables.append(&mut vars);
    }
    Ok(plugin_variables)
}

pub async fn get_plugin_variables(
    client: &Client,
    configs: &Configs,
    project_id: String,
    environment_id: String,
    plugin_id: String,
) -> Result<BTreeMap<String, String>> {
    let vars = queries::variables_for_plugin::Variables {
        project_id: project_id.clone(),
        environment_id: environment_id.clone(),
        plugin_id: plugin_id.clone(),
    };
    let variables =
        post_graphql::<queries::VariablesForPlugin, _>(client, configs.get_backboard(), vars)
            .await?
            .variables;

    Ok(variables)
}

pub async fn get_plugin_or_service_variables(
    client: &Client,
    configs: &Configs,
    project_id: String,
    environment_id: String,
    plugin_or_service: &PluginOrService,
) -> Result<BTreeMap<String, String>> {
    let variables = match plugin_or_service {
        PluginOrService::Plugin(plugin) => {
            let query = queries::variables_for_plugin::Variables {
                project_id: project_id.clone(),
                environment_id: environment_id.clone(),
                plugin_id: plugin.id.clone(),
            };

            post_graphql::<queries::VariablesForPlugin, _>(client, configs.get_backboard(), query)
                .await?
                .variables
        }
        PluginOrService::Service(service) => {
            let query = queries::variables_for_service_deployment::Variables {
                project_id: project_id.clone(),
                environment_id: environment_id.clone(),
                service_id: service.id.clone(),
            };

            post_graphql::<queries::VariablesForServiceDeployment, _>(
                client,
                configs.get_backboard(),
                query,
            )
            .await?
            .variables_for_service_deployment
        }
    };

    Ok(variables)
}
