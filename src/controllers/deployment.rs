use crate::{
    commands::subscriptions::{self, build_logs, deployment_logs},
    subscription::subscribe_graphql,
    commands::queries,
    client::post_graphql,
    config::Configs,
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

pub async fn get_build_logs(
    deployment_id: String,
    limit: Option<usize>,
) -> Result<Vec<queries::build_logs::BuildLogsBuildLogs>> {
    let configs = Configs::new()?;
    let client = crate::client::GQLClient::new_authorized(&configs)?;

    // Use a recent start date if we want limited logs
    let start_date = if limit.is_some() {
        Some(chrono::Utc::now() - chrono::Duration::hours(24))
    } else {
        None
    };

    let vars = queries::build_logs::Variables {
        deployment_id,
        start_date,
    };

    let response = post_graphql::<queries::BuildLogs, _>(&client, configs.get_backboard(), vars).await?;
    let mut logs = response.build_logs;

    // Apply limit if specified
    if let Some(limit) = limit {
        // Get the last N logs
        if logs.len() > limit {
            logs = logs.into_iter().rev().take(limit).rev().collect();
        }
    }

    Ok(logs)
}

pub async fn get_deploy_logs(
    deployment_id: String,
    limit: usize,
) -> Result<Vec<deployment_logs::LogFields>> {
    let vars = subscriptions::deployment_logs::Variables {
        deployment_id: deployment_id.clone(),
        filter: Some(String::new()),
        limit: Some(limit as i64),
    };

    let mut stream = subscribe_graphql::<subscriptions::DeploymentLogs>(vars).await?;
    let mut all_logs = Vec::new();

    // Get the first batch of logs and exit (subscription usually returns historical logs first)
    if let Some(Ok(log)) = stream.next().await {
        let log = log.data.context("Failed to retrieve deploy log")?;
        all_logs.extend(log.deployment_logs);
    }

    Ok(all_logs)
}
