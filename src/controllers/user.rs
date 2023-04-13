use reqwest::Client;

use crate::{
    client::post_graphql_handle,
    commands::{
        queries::{self},
        Configs,
    },
    errors::RailwayError,
};
use anyhow::Result;

pub async fn get_user(
    client: &Client,
    configs: &Configs,
) -> Result<queries::RailwayUser, RailwayError> {
    let vars = queries::user_meta::Variables {};

    let me = post_graphql_handle::<queries::UserMeta, _>(client, configs.get_backboard(), vars)
        .await?
        .me;

    Ok(me)
}
