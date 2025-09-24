use crate::{
    commands::{
        queries::{self},
        subscriptions::{self, build_logs, deployment_logs},
    },
    post_graphql,
    subscription::subscribe_graphql,
    util::retry::{retry_with_backoff, RetryConfig},
};
use anyhow::{Context, Result};
use futures::StreamExt;
use reqwest::Client;

const LOGS_RETRY_CONFIG: RetryConfig = RetryConfig {
    max_attempts: 12,
    initial_delay_ms: 1000,
    max_delay_ms: 8000,
    backoff_multiplier: 1.5,
    on_retry: None,
};

pub async fn fetch_build_logs(
    client: &Client,
    backboard: &str,
    deployment_id: String,
    limit: Option<i64>,
    on_log: impl Fn(queries::build_logs::BuildLogsBuildLogs),
) -> Result<()> {
    // Adjust for API returning limit + 1 logs
    let api_limit = limit.map(|n| if n > 0 { n - 1 } else { 0 });

    let vars = queries::build_logs::Variables {
        deployment_id,
        limit: api_limit,
        start_date: None,
    };

    let response = post_graphql::<queries::BuildLogs, _>(client, backboard, vars).await?;

    // Take only the requested number of logs
    let logs_to_show = if let Some(l) = limit {
        response.build_logs.into_iter().take(l as usize)
    } else {
        response.build_logs.into_iter().take(usize::MAX)
    };

    for log in logs_to_show {
        on_log(log);
    }

    Ok(())
}

pub async fn fetch_deploy_logs(
    client: &Client,
    backboard: &str,
    deployment_id: String,
    limit: Option<i64>,
    on_log: impl Fn(queries::deployment_logs::LogFields),
) -> Result<()> {
    // Adjust for API returning limit + 1 logs
    let api_limit = limit.map(|n| if n > 0 { n - 1 } else { 0 });

    let vars = queries::deployment_logs::Variables {
        deployment_id,
        limit: api_limit,
    };

    let response = post_graphql::<queries::DeploymentLogs, _>(client, backboard, vars).await?;

    // Take only the requested number of logs
    let logs_to_show = if let Some(l) = limit {
        response.deployment_logs.into_iter().take(l as usize)
    } else {
        response.deployment_logs.into_iter().take(usize::MAX)
    };

    for log in logs_to_show {
        on_log(log);
    }

    Ok(())
}

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
