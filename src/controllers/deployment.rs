use reqwest::Client;

use crate::{
    client::post_graphql,
    commands::{
        queries::{self},
        Configs,
    },
    errors::RailwayError,
};
use anyhow::Result;

pub async fn get_deployment(
    client: &Client,
    configs: &Configs,
    deployment_id: String,
) -> Result<queries::RailwayDeployment, RailwayError> {
    let vars = queries::deployment::Variables { id: deployment_id };

    let deployment = post_graphql::<queries::Deployment, _>(&client, configs.get_backboard(), vars)
        .await?
        .deployment;

    Ok(deployment)
}
