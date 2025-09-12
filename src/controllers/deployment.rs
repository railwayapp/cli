use crate::{
    commands::subscriptions::{self, build_logs, deployment_logs},
    subscription::subscribe_graphql,
    util::retry::{retry_with_backoff, RetryConfig},
};
use anyhow::{Context, Result};
use futures::StreamExt;

const LOGS_RETRY_CONFIG: RetryConfig = RetryConfig {
    max_attempts: 12,
    initial_delay_ms: 1000,
    max_delay_ms: 8000,
    backoff_multiplier: 1.5,
    on_retry: None,
};

pub async fn stream_build_logs(
    deployment_id: String,
    on_log: impl Fn(build_logs::LogFields),
) -> Result<()> {
    // Retry establishing connection for up to 60 seconds
    let mut stream = retry_with_backoff(LOGS_RETRY_CONFIG, || async {
        let vars = subscriptions::build_logs::Variables {
            deployment_id: deployment_id.clone(),
            filter: Some(String::new()),
            limit: Some(500),
        };
        subscribe_graphql::<subscriptions::BuildLogs>(vars).await
    })
    .await?;

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
    // Retry establishing connection for up to 60 seconds
    let mut stream = retry_with_backoff(LOGS_RETRY_CONFIG, || async {
        let vars = subscriptions::deployment_logs::Variables {
            deployment_id: deployment_id.clone(),
            filter: Some(String::new()),
            limit: Some(500),
        };
        subscribe_graphql::<subscriptions::DeploymentLogs>(vars).await
    })
    .await?;

    while let Some(Ok(log)) = stream.next().await {
        let log = log.data.context("Failed to retrieve deploy log")?;
        for line in log.deployment_logs {
            on_log(line);
        }
    }

    Ok(())
}
