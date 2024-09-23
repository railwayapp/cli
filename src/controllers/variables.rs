use crate::{
    client::post_graphql,
    commands::{queries, Configs},
};
use anyhow::Result;
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
    let response = post_graphql::<queries::VariablesForServiceDeployment, _>(
        client,
        configs.get_backboard(),
        vars,
    )
    .await?;

    let variables = response
        .variables_for_service_deployment
        .into_iter()
        .collect();

    Ok(variables)
}
