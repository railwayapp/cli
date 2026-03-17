use crate::{
    commands::{
        queries::{self},
        subscriptions::{self, build_logs, deployment_logs, http_logs},
    },
    post_graphql,
    subscription::subscribe_graphql,
    util::retry::RetryConfig,
};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use futures::StreamExt;
use reqwest::Client;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio::time::sleep;

const LOGS_RETRY_CONFIG: RetryConfig = RetryConfig {
    max_attempts: 12,
    initial_delay_ms: 1000,
    max_delay_ms: 8000,
    backoff_multiplier: 1.5,
    on_retry: None,
};

const HTTP_LOG_STREAM_AFTER_WINDOW: Duration = Duration::from_secs(60 * 60);
const HTTP_LOG_STREAM_BATCH_SIZE: i64 = 500;
const HTTP_LOG_STREAM_STABLE_CONNECTION_DURATION: Duration = Duration::from_secs(30);

pub struct FetchLogsParams<'a> {
    pub client: &'a Client,
    pub backboard: &'a str,
    pub deployment_id: String,
    pub limit: Option<i64>,
    pub filter: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
}

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
    params: FetchLogsParams<'_>,
    on_log: impl Fn(queries::build_logs::BuildLogsBuildLogs),
) -> Result<()> {
    let vars = queries::build_logs::Variables {
        deployment_id: params.deployment_id,
        limit: params.limit,
        start_date: params.start_date,
        end_date: params.end_date,
        filter: params.filter,
    };

    let response =
        post_graphql::<queries::BuildLogs, _>(params.client, params.backboard, vars).await?;

    // Take only the requested number of logs from the end (the API has a bug and returns limit+1)
    let logs = response.build_logs;
    let logs_to_process = take_last_n_logs(logs, params.limit);

    for log in logs_to_process {
        on_log(log);
    }

    Ok(())
}

pub async fn fetch_deploy_logs(
    params: FetchLogsParams<'_>,
    on_log: impl Fn(queries::deployment_logs::LogFields),
) -> Result<()> {
    let vars = queries::deployment_logs::Variables {
        deployment_id: params.deployment_id,
        limit: params.limit,
        filter: params.filter,
        start_date: params.start_date,
        end_date: params.end_date,
    };

    let response =
        post_graphql::<queries::DeploymentLogs, _>(params.client, params.backboard, vars).await?;

    // Take only the requested number of logs from the end (the API has a bug and returns limit+1)
    let logs = response.deployment_logs;
    let logs_to_process = take_last_n_logs(logs, params.limit);

    for log in logs_to_process {
        on_log(log);
    }

    Ok(())
}

pub async fn fetch_http_logs(
    params: FetchLogsParams<'_>,
    on_log: impl Fn(queries::http_logs::HttpLogFields),
) -> Result<()> {
    let before_limit = params.limit.unwrap_or(500);
    let vars = queries::http_logs::Variables {
        deployment_id: params.deployment_id,
        filter: params.filter,
        before_limit,
        before_date: params.start_date.map(|date| date.to_rfc3339()),
        anchor_date: params.end_date.map(|date| date.to_rfc3339()),
        after_date: None,
        after_limit: None,
    };

    let response =
        post_graphql::<queries::HttpLogs, _>(params.client, params.backboard, vars).await?;

    let logs = response.http_logs;
    let logs = take_last_n_logs(logs, Some(before_limit));

    for log in logs {
        on_log(log);
    }

    Ok(())
}

pub async fn stream_build_logs(
    deployment_id: String,
    filter: Option<String>,
    on_log: impl Fn(build_logs::LogFields),
) -> Result<()> {
    let mut last_timestamp: Option<String> = None;
    let mut attempt = 0;
    let mut delay_ms = LOGS_RETRY_CONFIG.initial_delay_ms;
    let mut received_any_logs = false;

    loop {
        attempt += 1;

        let vars = subscriptions::build_logs::Variables {
            deployment_id: deployment_id.clone(),
            filter: filter.clone().or_else(|| Some(String::new())),
            limit: Some(500),
        };

        let result = async {
            let mut stream = subscribe_graphql::<subscriptions::BuildLogs>(vars).await?;

            while let Some(response) = stream.next().await {
                let log = response
                    .context("Build log stream error")?
                    .data
                    .context("Failed to retrieve build log")?;

                for line in log.build_logs {
                    if let Some(ref ts) = last_timestamp {
                        if line.timestamp <= *ts {
                            continue;
                        }
                    }
                    last_timestamp = Some(line.timestamp.clone());
                    received_any_logs = true;
                    on_log(line);
                }
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;

        match result {
            Ok(()) => return Ok(()),
            Err(e) if attempt >= LOGS_RETRY_CONFIG.max_attempts => {
                // If we received some logs before the error, treat as success
                // (the build likely finished and the stream closed)
                if received_any_logs {
                    return Ok(());
                }
                return Err(e);
            }
            Err(_) => {
                // If we've received logs and then get an error, the build likely completed
                // and the stream was closed by the server. Treat this as success.
                if received_any_logs {
                    return Ok(());
                }
                sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = ((delay_ms as f64 * LOGS_RETRY_CONFIG.backoff_multiplier) as u64)
                    .min(LOGS_RETRY_CONFIG.max_delay_ms);
            }
        }
    }
}

pub async fn stream_http_logs(
    deployment_id: String,
    filter: Option<String>,
    on_log: impl Fn(http_logs::HttpLogFields),
) -> Result<()> {
    let mut last_timestamp = Some(Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true));
    let mut seen_request_ids = HashSet::new();
    let mut attempt = 0;
    let mut delay_ms = LOGS_RETRY_CONFIG.initial_delay_ms;

    loop {
        let connected_at = Instant::now();
        let mut received_any_logs = false;
        let anchor_timestamp = last_timestamp
            .clone()
            .unwrap_or_else(|| Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true));
        let after_timestamp = (Utc::now()
            + chrono::Duration::from_std(HTTP_LOG_STREAM_AFTER_WINDOW).unwrap())
        .to_rfc3339_opts(SecondsFormat::Nanos, true);
        let vars = subscriptions::http_logs::Variables {
            deployment_id: deployment_id.clone(),
            filter: filter.clone(),
            anchor_date: Some(anchor_timestamp),
            after_date: Some(after_timestamp),
            after_limit: Some(HTTP_LOG_STREAM_BATCH_SIZE),
        };

        let mut stream = match subscribe_graphql::<subscriptions::HttpLogs>(vars).await {
            Ok(stream) => stream,
            Err(e) => {
                attempt += 1;

                if attempt >= LOGS_RETRY_CONFIG.max_attempts {
                    return Err(e);
                }

                sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = ((delay_ms as f64 * LOGS_RETRY_CONFIG.backoff_multiplier) as u64)
                    .min(LOGS_RETRY_CONFIG.max_delay_ms);
                continue;
            }
        };

        let result = async {
            while let Some(response) = stream.next().await {
                let log = response
                    .context("HTTP log stream error")?
                    .data
                    .context("Failed to retrieve HTTP logs")?;

                for line in log.http_logs {
                    if !is_new_http_log(
                        &line.timestamp,
                        &line.request_id,
                        &mut last_timestamp,
                        &mut seen_request_ids,
                    ) {
                        continue;
                    }

                    received_any_logs = true;
                    on_log(line);
                }
            }

            Ok::<(), anyhow::Error>(())
        }
        .await;

        let should_reset_retry_state =
            should_reset_http_stream_retry_state(received_any_logs, connected_at.elapsed());

        if should_reset_retry_state {
            attempt = 0;
            delay_ms = LOGS_RETRY_CONFIG.initial_delay_ms;
        } else {
            attempt += 1;

            match result {
                Err(e) if attempt >= LOGS_RETRY_CONFIG.max_attempts => return Err(e),
                Ok(()) if attempt >= LOGS_RETRY_CONFIG.max_attempts => {
                    return Err(anyhow!(
                        "HTTP log stream closed before receiving any events"
                    ));
                }
                _ => {}
            }
        }

        sleep(Duration::from_millis(delay_ms)).await;

        if !should_reset_retry_state {
            delay_ms = ((delay_ms as f64 * LOGS_RETRY_CONFIG.backoff_multiplier) as u64)
                .min(LOGS_RETRY_CONFIG.max_delay_ms);
        }
    }
}

fn should_reset_http_stream_retry_state(
    received_any_logs: bool,
    connection_duration: Duration,
) -> bool {
    received_any_logs || connection_duration >= HTTP_LOG_STREAM_STABLE_CONNECTION_DURATION
}

fn is_new_http_log(
    timestamp: &str,
    request_id: &str,
    last_timestamp: &mut Option<String>,
    seen_request_ids: &mut HashSet<String>,
) -> bool {
    if let Some(previous_timestamp) = last_timestamp.as_ref() {
        if timestamp < previous_timestamp.as_str() {
            return false;
        }

        if timestamp == previous_timestamp.as_str() && seen_request_ids.contains(request_id) {
            return false;
        }
    }

    if last_timestamp
        .as_ref()
        .is_none_or(|previous_timestamp| timestamp > previous_timestamp.as_str())
    {
        *last_timestamp = Some(timestamp.to_owned());
        seen_request_ids.clear();
    }

    seen_request_ids.insert(request_id.to_owned());
    true
}

pub async fn stream_deploy_logs(
    deployment_id: String,
    filter: Option<String>,
    on_log: impl Fn(deployment_logs::LogFields),
) -> Result<()> {
    let mut last_timestamp: Option<String> = None;
    let mut attempt = 0;
    let mut delay_ms = LOGS_RETRY_CONFIG.initial_delay_ms;
    let mut received_any_logs = false;

    loop {
        attempt += 1;

        let vars = subscriptions::deployment_logs::Variables {
            deployment_id: deployment_id.clone(),
            filter: filter.clone().or_else(|| Some(String::new())),
            limit: Some(500),
        };

        let result = async {
            let mut stream = subscribe_graphql::<subscriptions::DeploymentLogs>(vars).await?;

            while let Some(response) = stream.next().await {
                let log = response
                    .context("Deploy log stream error")?
                    .data
                    .context("Failed to retrieve deploy log")?;

                for line in log.deployment_logs {
                    if let Some(ref ts) = last_timestamp {
                        if line.timestamp <= *ts {
                            continue;
                        }
                    }
                    last_timestamp = Some(line.timestamp.clone());
                    received_any_logs = true;
                    on_log(line);
                }
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;

        match result {
            Ok(()) => return Ok(()),
            Err(e) if attempt >= LOGS_RETRY_CONFIG.max_attempts => {
                // If we received some logs before the error, treat as success
                // (the deployment likely finished and the stream closed)
                if received_any_logs {
                    return Ok(());
                }
                return Err(e);
            }
            Err(_) => {
                // If we've received logs and then get an error, the deployment likely completed
                // and the stream was closed by the server. Treat this as success.
                if received_any_logs {
                    return Ok(());
                }
                sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = ((delay_ms as f64 * LOGS_RETRY_CONFIG.backoff_multiplier) as u64)
                    .min(LOGS_RETRY_CONFIG.max_delay_ms);
            }
        }
    }
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

    #[test]
    fn test_is_new_http_log_skips_old_and_duplicate_entries() {
        let mut last_timestamp = Some("2025-01-01T00:00:00Z".to_string());
        let mut seen_request_ids = HashSet::from(["req-1".to_string()]);

        assert!(!is_new_http_log(
            "2024-12-31T23:59:59Z",
            "req-old",
            &mut last_timestamp,
            &mut seen_request_ids,
        ));
        assert!(!is_new_http_log(
            "2025-01-01T00:00:00Z",
            "req-1",
            &mut last_timestamp,
            &mut seen_request_ids,
        ));
        assert!(is_new_http_log(
            "2025-01-01T00:00:00Z",
            "req-2",
            &mut last_timestamp,
            &mut seen_request_ids,
        ));
    }

    #[test]
    fn test_is_new_http_log_advances_timestamp_window() {
        let mut last_timestamp = Some("2025-01-01T00:00:00Z".to_string());
        let mut seen_request_ids = HashSet::from(["req-1".to_string()]);

        assert!(is_new_http_log(
            "2025-01-01T00:00:01Z",
            "req-3",
            &mut last_timestamp,
            &mut seen_request_ids,
        ));
        assert_eq!(last_timestamp.as_deref(), Some("2025-01-01T00:00:01Z"));
        assert_eq!(seen_request_ids, HashSet::from(["req-3".to_string()]));
    }

    #[test]
    fn test_should_reset_http_stream_retry_state_after_logs() {
        assert!(should_reset_http_stream_retry_state(
            true,
            Duration::from_secs(1),
        ));
    }

    #[test]
    fn test_should_reset_http_stream_retry_state_after_stable_connection() {
        assert!(should_reset_http_stream_retry_state(
            false,
            HTTP_LOG_STREAM_STABLE_CONNECTION_DURATION,
        ));
    }

    #[test]
    fn test_should_not_reset_http_stream_retry_state_for_short_empty_connection() {
        assert!(!should_reset_http_stream_retry_state(
            false,
            Duration::from_secs(1),
        ));
    }
}
