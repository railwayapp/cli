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

pub async fn get_project(
    client: &Client,
    configs: &Configs,
    project_id: String,
) -> Result<queries::RailwayProject, RailwayError> {
    let vars = queries::project::Variables { id: project_id };

    let project =
        post_graphql_handle::<queries::Project, _>(client, configs.get_backboard(), vars)
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
