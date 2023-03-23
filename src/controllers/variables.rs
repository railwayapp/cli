use crate::{
    client::post_graphql,
    commands::{queries, Configs},
};
use anyhow::{Context, Result};
use reqwest::Client;
use std::collections::BTreeMap;

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
    let res = post_graphql::<queries::VariablesForServiceDeployment, _>(
        client,
        configs.get_backboard(),
        vars,
    )
    .await?;

    let body = res
        .data
        .context("Failed to get service variables (query VariablesForServiceDeployment)")?;

    Ok(body.variables_for_service_deployment)
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
    let res = post_graphql::<queries::VariablesForPlugin, _>(client, configs.get_backboard(), vars)
        .await?;
    let body = res
        .data
        .context("Failed to get plugin variables (query VariablesForPlugin)")?;

    Ok(body.variables)
}
