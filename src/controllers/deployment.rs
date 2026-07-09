use crate::{
    commands::{
        queries::{self},
        subscriptions::{
            self, build_logs, deployment, deployment_logs, http_logs, network_flow_logs,
        },
    },
    post_graphql,
    subscription::subscribe_graphql,
    util::retry::RetryConfig,
};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use futures::StreamExt;
use reqwest::Client;
use std::collections::{HashSet, VecDeque};
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
const NETWORK_FLOW_LOG_DEFAULT_LIMIT: i64 = 500;
const NETWORK_FLOW_STREAM_LOOKBACK_SECONDS: i64 = 30;
const NETWORK_FLOW_STREAM_DEDUPE_CACHE_SIZE: usize = 10_000;

pub struct FetchLogsParams<'a> {
    pub client: &'a Client,
    pub backboard: &'a str,
    pub deployment_id: String,
    pub limit: Option<i64>,
    pub filter: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
}

pub struct FetchNetworkFlowLogsParams<'a> {
    pub client: &'a Client,
    pub backboard: &'a str,
    pub environment_id: String,
    pub service_id: Option<String>,
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

#[derive(Debug, PartialEq, Eq)]
struct NetworkFlowLogWindow {
    before_limit: Option<i64>,
    before_date: Option<String>,
    anchor_date: Option<String>,
    after_date: Option<String>,
    after_limit: Option<i64>,
}

fn format_network_flow_log_timestamp(date: DateTime<Utc>) -> String {
    date.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn format_http_log_timestamp(date: DateTime<Utc>) -> String {
    format_network_flow_log_timestamp(date)
}

fn network_flow_log_window(
    limit: Option<i64>,
    start_date: Option<DateTime<Utc>>,
    end_date: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> NetworkFlowLogWindow {
    let before_limit = Some(limit.unwrap_or(NETWORK_FLOW_LOG_DEFAULT_LIMIT));

    match (start_date, end_date) {
        (Some(start), Some(end)) => NetworkFlowLogWindow {
            before_limit,
            before_date: Some(format_network_flow_log_timestamp(start)),
            anchor_date: Some(format_network_flow_log_timestamp(end)),
            after_date: Some(format_network_flow_log_timestamp(end)),
            after_limit: Some(0),
        },
        (Some(start), None) => NetworkFlowLogWindow {
            before_limit,
            before_date: Some(format_network_flow_log_timestamp(start)),
            anchor_date: Some(format_network_flow_log_timestamp(now)),
            after_date: Some(format_network_flow_log_timestamp(now)),
            after_limit: Some(0),
        },
        (None, Some(end)) => NetworkFlowLogWindow {
            before_limit,
            before_date: Some(format_network_flow_log_timestamp(
                DateTime::<Utc>::from_timestamp(0, 0)
                    .expect("Unix epoch should be a valid timestamp"),
            )),
            anchor_date: Some(format_network_flow_log_timestamp(end)),
            after_date: Some(format_network_flow_log_timestamp(end)),
            after_limit: Some(0),
        },
        (None, None) => NetworkFlowLogWindow {
            before_limit,
            before_date: None,
            anchor_date: None,
            after_date: None,
            after_limit: None,
        },
    }
}

pub async fn fetch_build_logs(
    params: FetchLogsParams<'_>,
    mut on_log: impl FnMut(queries::build_logs::BuildLogsBuildLogs),
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
    mut on_log: impl FnMut(queries::deployment_logs::LogFields),
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
    mut on_log: impl FnMut(queries::http_logs::HttpLogFields),
) -> Result<()> {
    let before_limit = params.limit.unwrap_or(500);
    let vars = queries::http_logs::Variables {
        deployment_id: params.deployment_id,
        filter: params.filter,
        before_limit,
        before_date: params.start_date.map(format_http_log_timestamp),
        anchor_date: params.end_date.map(format_http_log_timestamp),
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

pub async fn fetch_network_flow_logs(
    params: FetchNetworkFlowLogsParams<'_>,
    mut on_log: impl FnMut(queries::network_flow_logs::NetworkFlowLogFields),
) -> Result<()> {
    let window =
        network_flow_log_window(params.limit, params.start_date, params.end_date, Utc::now());
    let vars = queries::network_flow_logs::Variables {
        environment_id: params.environment_id,
        service_id: params.service_id,
        filter: params.filter,
        before_limit: window.before_limit,
        before_date: window.before_date,
        anchor_date: window.anchor_date,
        after_date: window.after_date,
        after_limit: window.after_limit,
    };

    let response =
        post_graphql::<queries::NetworkFlowLogs, _>(params.client, params.backboard, vars).await?;

    let logs = take_last_n_logs(response.network_flow_logs, window.before_limit);

    for log in logs {
        on_log(log);
    }

    Ok(())
}

pub async fn stream_build_logs(
    deployment_id: String,
    filter: Option<String>,
    mut on_log: impl FnMut(build_logs::LogFields),
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
    on_log: impl FnMut(http_logs::HttpLogFields),
) -> Result<()> {
    tokio::select! {
        result = stream_http_logs_inner(deployment_id.clone(), filter, on_log) => result,
        _ = wait_for_deployment_removal(&deployment_id) => {
            eprintln!("\nDeployment was removed. HTTP log stream closed.");
            Ok(())
        }
    }
}

pub async fn stream_network_flow_logs(
    environment_id: String,
    service_id: Option<String>,
    filter: Option<String>,
    mut on_log: impl FnMut(network_flow_logs::NetworkFlowLogFields),
) -> Result<()> {
    let mut max_capture_end: Option<DateTime<Utc>> = None;
    let mut seen_flow_ids = NetworkFlowLogDedupe::new(NETWORK_FLOW_STREAM_DEDUPE_CACHE_SIZE);
    let mut attempt = 0;
    let mut delay_ms = LOGS_RETRY_CONFIG.initial_delay_ms;

    loop {
        let before_date = network_flow_stream_before_date(max_capture_end);
        let vars = subscriptions::network_flow_logs::Variables {
            environment_id: environment_id.clone(),
            service_id: service_id.clone(),
            filter: filter.clone(),
            before_limit: Some(NETWORK_FLOW_LOG_DEFAULT_LIMIT),
            before_date: Some(before_date),
            anchor_date: None,
            after_date: None,
            after_limit: Some(0),
        };

        let mut stream = match subscribe_graphql::<subscriptions::NetworkFlowLogs>(vars).await {
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

        attempt = 0;
        delay_ms = LOGS_RETRY_CONFIG.initial_delay_ms;

        while let Some(response) = stream.next().await {
            let log = response
                .context("Network flow log stream error")?
                .data
                .context("Failed to retrieve network flow logs")?;

            for line in log.network_flow_logs {
                update_max_network_flow_capture_end(&line.capture_end, &mut max_capture_end);

                if !seen_flow_ids.insert(line.flow_id.clone()) {
                    continue;
                }

                on_log(line);
            }
        }
    }
}

struct NetworkFlowLogDedupe {
    seen: HashSet<String>,
    order: VecDeque<String>,
    max_size: usize,
}

impl NetworkFlowLogDedupe {
    fn new(max_size: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            max_size,
        }
    }

    fn insert(&mut self, flow_id: String) -> bool {
        if self.seen.contains(&flow_id) {
            return false;
        }

        self.seen.insert(flow_id.clone());
        self.order.push_back(flow_id);

        while self.order.len() > self.max_size {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }

        true
    }
}

fn network_flow_stream_before_date(max_capture_end: Option<DateTime<Utc>>) -> String {
    let anchor = max_capture_end.unwrap_or_else(Utc::now);
    format_network_flow_log_timestamp(
        anchor - ChronoDuration::seconds(NETWORK_FLOW_STREAM_LOOKBACK_SECONDS),
    )
}

fn update_max_network_flow_capture_end(
    capture_end: &str,
    max_capture_end: &mut Option<DateTime<Utc>>,
) {
    let Ok(capture_end) =
        DateTime::parse_from_rfc3339(capture_end).map(|date| date.with_timezone(&Utc))
    else {
        return;
    };

    if max_capture_end.is_none_or(|max| capture_end > max) {
        *max_capture_end = Some(capture_end);
    }
}

async fn wait_for_deployment_removal(deployment_id: &str) {
    loop {
        if let Ok(mut stream) =
            subscribe_graphql::<subscriptions::Deployment>(deployment::Variables {
                id: deployment_id.to_owned(),
            })
            .await
        {
            while let Some(response) = stream.next().await {
                let removed = response.ok().and_then(|r| r.data).is_some_and(|data| {
                    matches!(
                        data.deployment.status,
                        deployment::DeploymentStatus::REMOVED
                            | deployment::DeploymentStatus::REMOVING
                    )
                });
                if removed {
                    return;
                }
            }
        }
        // Subscription failed or ended without seeing removal — retry
        sleep(Duration::from_secs(5)).await;
    }
}

async fn stream_http_logs_inner(
    deployment_id: String,
    filter: Option<String>,
    mut on_log: impl FnMut(http_logs::HttpLogFields),
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
    mut on_log: impl FnMut(deployment_logs::LogFields),
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

    fn dt(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

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
    fn test_network_flow_log_window_uses_explicit_bounded_range() {
        let start = dt("2026-06-18T04:41:00Z");
        let end = dt("2026-06-18T04:42:00Z");
        let now = dt("2026-06-18T04:43:00Z");

        let window = network_flow_log_window(Some(100), Some(start), Some(end), now);

        assert_eq!(
            window,
            NetworkFlowLogWindow {
                before_limit: Some(100),
                before_date: Some("2026-06-18T04:41:00.000000000Z".to_string()),
                anchor_date: Some("2026-06-18T04:42:00.000000000Z".to_string()),
                after_date: Some("2026-06-18T04:42:00.000000000Z".to_string()),
                after_limit: Some(0),
            }
        );
    }

    #[test]
    fn test_network_flow_log_window_resolves_open_ended_ranges() {
        let start = dt("2026-06-18T04:41:00Z");
        let end = dt("2026-06-18T04:42:00Z");
        let now = dt("2026-06-18T04:43:00Z");

        let since_window = network_flow_log_window(None, Some(start), None, now);
        assert_eq!(
            since_window,
            NetworkFlowLogWindow {
                before_limit: Some(NETWORK_FLOW_LOG_DEFAULT_LIMIT),
                before_date: Some("2026-06-18T04:41:00.000000000Z".to_string()),
                anchor_date: Some("2026-06-18T04:43:00.000000000Z".to_string()),
                after_date: Some("2026-06-18T04:43:00.000000000Z".to_string()),
                after_limit: Some(0),
            }
        );

        let until_window = network_flow_log_window(None, None, Some(end), now);
        assert_eq!(
            until_window,
            NetworkFlowLogWindow {
                before_limit: Some(NETWORK_FLOW_LOG_DEFAULT_LIMIT),
                before_date: Some("1970-01-01T00:00:00.000000000Z".to_string()),
                anchor_date: Some("2026-06-18T04:42:00.000000000Z".to_string()),
                after_date: Some("2026-06-18T04:42:00.000000000Z".to_string()),
                after_limit: Some(0),
            }
        );
    }

    #[test]
    fn test_network_flow_log_window_leaves_unbounded_snapshot_to_api_defaults() {
        let now = dt("2026-06-18T04:43:00Z");

        let window = network_flow_log_window(Some(20), None, None, now);

        assert_eq!(
            window,
            NetworkFlowLogWindow {
                before_limit: Some(20),
                before_date: None,
                anchor_date: None,
                after_date: None,
                after_limit: None,
            }
        );
    }

    #[test]
    fn test_network_flow_dedupe_keeps_out_of_order_sibling_flows() {
        let mut dedupe = NetworkFlowLogDedupe::new(10);

        assert!(dedupe.insert("newer-flow".to_string()));
        assert!(dedupe.insert("older-sibling-flow".to_string()));
        assert!(!dedupe.insert("newer-flow".to_string()));
    }

    #[test]
    fn test_network_flow_dedupe_bounds_cache_size() {
        let mut dedupe = NetworkFlowLogDedupe::new(2);

        assert!(dedupe.insert("flow-1".to_string()));
        assert!(dedupe.insert("flow-2".to_string()));
        assert!(dedupe.insert("flow-3".to_string()));
        assert!(dedupe.insert("flow-1".to_string()));
    }

    #[test]
    fn test_network_flow_stream_before_date_uses_lookback() {
        let before_date = network_flow_stream_before_date(Some(dt("2026-06-18T04:43:00Z")));

        assert_eq!(before_date, "2026-06-18T04:42:30.000000000Z");
    }

    #[test]
    fn test_http_log_timestamp_uses_cursor_timestamp_format() {
        assert_eq!(
            format_http_log_timestamp(dt("2026-06-18T04:41:00Z")),
            "2026-06-18T04:41:00.000000000Z"
        );
    }

    #[test]
    fn test_update_max_network_flow_capture_end_ignores_older_rows() {
        let mut max_capture_end = Some(dt("2026-06-18T04:43:00Z"));

        update_max_network_flow_capture_end("2026-06-18T04:42:30Z", &mut max_capture_end);
        assert_eq!(max_capture_end, Some(dt("2026-06-18T04:43:00Z")));

        update_max_network_flow_capture_end("2026-06-18T04:43:30Z", &mut max_capture_end);
        assert_eq!(max_capture_end, Some(dt("2026-06-18T04:43:30Z")));
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
