use crate::{
    commands::{
        queries::{self},
        subscriptions::{self, build_logs, deployment_logs},
    },
    post_graphql,
    subscription::subscribe_graphql,
    util::retry::{RetryConfig, retry_with_backoff},
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

// Helper to handle the API's off-by-one bug where it returns limit+1 logs
fn take_last_n_logs<T>(mut logs: Vec<T>, limit: Option<i64>) -> Vec<T> {
    if let Some(l) = limit {
        let l = l as usize;
        if logs.len() > l {
            // Remove items from the beginning to keep only the last l items
            logs.drain(0..logs.len() - l);
        }
    }
    logs
}

pub async fn fetch_build_logs(
    client: &Client,
    backboard: &str,
    deployment_id: String,
    limit: Option<i64>,
    filter: Option<String>,
    on_log: impl Fn(queries::build_logs::BuildLogsBuildLogs),
) -> Result<()> {
    let vars = queries::build_logs::Variables {
        deployment_id,
        limit,
        start_date: None,
        filter,
    };

    let response = post_graphql::<queries::BuildLogs, _>(client, backboard, vars).await?;

    // Take only the requested number of logs from the end (the API has a bug and returns limit+1)
    let logs = response.build_logs;
    let logs_to_process = take_last_n_logs(logs, limit);

    for log in logs_to_process {
        on_log(log);
    }

    Ok(())
}

pub async fn fetch_deploy_logs(
    client: &Client,
    backboard: &str,
    deployment_id: String,
    limit: Option<i64>,
    filter: Option<String>,
    on_log: impl Fn(queries::deployment_logs::LogFields),
) -> Result<()> {
    let vars = queries::deployment_logs::Variables {
        deployment_id,
        limit,
        filter,
    };

    let response = post_graphql::<queries::DeploymentLogs, _>(client, backboard, vars).await?;

    // Take only the requested number of logs from the end (the API has a bug and returns limit+1)
    let logs = response.deployment_logs;
    let logs_to_process = take_last_n_logs(logs, limit);

    for log in logs_to_process {
        on_log(log);
    }

    Ok(())
}

pub async fn stream_build_logs(
    deployment_id: String,
    filter: Option<String>,
    on_log: impl Fn(build_logs::LogFields),
) -> Result<()> {
    // Retry establishing connection for up to 60 seconds
    let mut stream = retry_with_backoff(LOGS_RETRY_CONFIG, || async {
        let vars = subscriptions::build_logs::Variables {
            deployment_id: deployment_id.clone(),
            filter: filter.clone().or_else(|| Some(String::new())),
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
    filter: Option<String>,
    on_log: impl Fn(deployment_logs::LogFields),
) -> Result<()> {
    // Retry establishing connection for up to 60 seconds
    let mut stream = retry_with_backoff(LOGS_RETRY_CONFIG, || async {
        let vars = subscriptions::deployment_logs::Variables {
            deployment_id: deployment_id.clone(),
            filter: filter.clone().or_else(|| Some(String::new())),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_take_last_n_logs_with_limit() {
        let logs = vec!["log1", "log2", "log3", "log4", "log5"];

        // Request 3 logs from 5
        let result = take_last_n_logs(logs.clone(), Some(3));
        assert_eq!(result, vec!["log3", "log4", "log5"]);

        // Request 2 logs from 5
        let result = take_last_n_logs(logs.clone(), Some(2));
        assert_eq!(result, vec!["log4", "log5"]);
    }

    #[test]
    fn test_take_last_n_logs_limit_exceeds_size() {
        let logs = vec!["log1", "log2", "log3"];

        // Request 5 logs but only 3 available
        let result = take_last_n_logs(logs.clone(), Some(5));
        assert_eq!(result, vec!["log1", "log2", "log3"]);
    }

    #[test]
    fn test_take_last_n_logs_no_limit() {
        let logs = vec!["log1", "log2", "log3"];

        // No limit specified, return all
        let result = take_last_n_logs(logs.clone(), None);
        assert_eq!(result, vec!["log1", "log2", "log3"]);
    }

    #[test]
    fn test_take_last_n_logs_empty_vec() {
        let logs: Vec<String> = vec![];

        // Empty input with limit
        let result = take_last_n_logs(logs.clone(), Some(5));
        assert_eq!(result, Vec::<String>::new());

        // Empty input without limit
        let result = take_last_n_logs(logs, None);
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    fn test_take_last_n_logs_limit_zero() {
        let logs = vec!["log1", "log2", "log3"];

        // Limit of 0 should return empty vec
        let result = take_last_n_logs(logs, Some(0));
        assert_eq!(result, Vec::<&str>::new());
    }
}
