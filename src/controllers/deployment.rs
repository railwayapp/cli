use crate::{
    commands::subscriptions::{self, build_logs, deployment_logs},
    subscription::subscribe_graphql,
};
use anyhow::{Context, Result};
use futures::StreamExt;

pub async fn stream_build_logs(
    deployment_id: String,
    on_log: impl Fn(build_logs::LogFields),
) -> Result<()> {
    let vars = subscriptions::build_logs::Variables {
        deployment_id: deployment_id.clone(),
        filter: Some(String::new()),
        limit: Some(500),
    };

    let mut stream = subscribe_graphql::<subscriptions::BuildLogs>(vars).await?;
    while let Some(Ok(log)) = stream.next().await {
        let log = log.data.context("Failed to retrieve build log")?;
        for line in log.build_logs {
            on_log(line);
        }
    }

    Ok(())
}

pub async fn stream_deploy_logs(
    deployment_id: String,
    on_log: impl Fn(deployment_logs::LogFields),
) -> Result<()> {
    let vars = subscriptions::deployment_logs::Variables {
        deployment_id: deployment_id.clone(),
        filter: Some(String::new()),
        limit: Some(500),
    };

    let mut stream = subscribe_graphql::<subscriptions::DeploymentLogs>(vars).await?;
    while let Some(Ok(log)) = stream.next().await {
        let log = log.data.context("Failed to retrieve deploy log")?;
        for line in log.deployment_logs {
            on_log(line);
        }
    }

    Ok(())
}
