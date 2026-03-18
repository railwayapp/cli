use rmcp::{ErrorData as McpError, model::*};

use crate::{
    client::post_graphql,
    controllers::deployment::{FetchLogsParams, fetch_http_logs},
    gql::queries,
    util::logs::HttpLogLike,
};

use super::super::handler::RailwayMcp;
use super::super::params::{HttpObservabilityParams, ServiceMetricsParams};

impl RailwayMcp {
    pub(crate) async fn fetch_http_logs_for_service(
        &self,
        project_id: Option<String>,
        service_id: Option<String>,
        environment_id: Option<String>,
        deployment_id: Option<String>,
        lines: Option<i64>,
    ) -> Result<Vec<queries::http_logs::HttpLogFields>, McpError> {
        let ctx = self
            .resolve_service_context(project_id, service_id, environment_id)
            .await?;

        let deployment_id = match deployment_id {
            Some(did) => did,
            None => {
                self.get_latest_deployment_id(&ctx.project_id, &ctx.environment_id, &ctx.service_id)
                    .await?
            }
        };

        let mut logs: Vec<queries::http_logs::HttpLogFields> = Vec::new();
        let backboard = self.configs.get_backboard();
        let fetch_params = FetchLogsParams {
            client: &self.client,
            backboard: &backboard,
            deployment_id,
            limit: Some(lines.unwrap_or(200)),
            filter: None,
            start_date: None,
            end_date: None,
        };

        fetch_http_logs(fetch_params, |log| {
            logs.push(log);
        })
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to fetch HTTP logs: {e}"), None))?;

        Ok(logs)
    }

    pub(crate) async fn do_service_metrics(
        &self,
        params: ServiceMetricsParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;

        let hours_back = params.hours_back.unwrap_or(1);
        let start_date = chrono::Utc::now() - chrono::Duration::hours(hours_back);

        let measurement_strings = params
            .measurements
            .unwrap_or_else(|| vec!["CPU_USAGE".to_string(), "MEMORY_USAGE_GB".to_string()]);

        let measurements = measurement_strings
            .iter()
            .map(|m| match m.as_str() {
                "CPU_USAGE" => Ok(queries::metrics::MetricMeasurement::CPU_USAGE),
                "CPU_USAGE_2" => Ok(queries::metrics::MetricMeasurement::CPU_USAGE_2),
                "MEMORY_USAGE_GB" => Ok(queries::metrics::MetricMeasurement::MEMORY_USAGE_GB),
                "MEMORY_LIMIT_GB" => Ok(queries::metrics::MetricMeasurement::MEMORY_LIMIT_GB),
                "DISK_USAGE_GB" => Ok(queries::metrics::MetricMeasurement::DISK_USAGE_GB),
                "NETWORK_RX_GB" => Ok(queries::metrics::MetricMeasurement::NETWORK_RX_GB),
                "NETWORK_TX_GB" => Ok(queries::metrics::MetricMeasurement::NETWORK_TX_GB),
                "CPU_LIMIT" => Ok(queries::metrics::MetricMeasurement::CPU_LIMIT),
                other => Err(McpError::invalid_params(
                    format!(
                        "Unknown measurement '{other}'. Valid values: CPU_USAGE, CPU_USAGE_2, \
                         MEMORY_USAGE_GB, MEMORY_LIMIT_GB, DISK_USAGE_GB, NETWORK_RX_GB, \
                         NETWORK_TX_GB, CPU_LIMIT"
                    ),
                    None,
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;

        let vars = queries::metrics::Variables {
            service_id: Some(ctx.service_id),
            environment_id: Some(ctx.environment_id),
            start_date,
            end_date: None,
            measurements,
            sample_rate_seconds: params.sample_rate_seconds,
        };

        let resp =
            post_graphql::<queries::Metrics, _>(&self.client, self.configs.get_backboard(), vars)
                .await
                .map_err(|e| {
                    McpError::internal_error(format!("Failed to fetch metrics: {e}"), None)
                })?;

        if resp.metrics.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No metrics data available for this service.".to_string(),
            )]));
        }

        let mut output = String::new();
        for metric in &resp.metrics {
            output.push_str(&format!("\n### {:?}\n", metric.measurement));
            if metric.values.is_empty() {
                output.push_str("No data points.\n");
            } else {
                let last_n: Vec<_> = metric.values.iter().rev().take(5).collect();
                for point in last_n.into_iter().rev() {
                    let ts = chrono::DateTime::from_timestamp(point.ts, 0)
                        .map(|dt| dt.format("%H:%M:%S UTC").to_string())
                        .unwrap_or_else(|| point.ts.to_string());
                    output.push_str(&format!("  {ts} -> {:.4}\n", point.value));
                }
                let avg =
                    metric.values.iter().map(|p| p.value).sum::<f64>() / metric.values.len() as f64;
                output.push_str(&format!(
                    "  Average ({}pts): {:.4}\n",
                    metric.values.len(),
                    avg
                ));
            }
        }

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    pub(crate) async fn do_http_requests(
        &self,
        params: HttpObservabilityParams,
    ) -> Result<CallToolResult, McpError> {
        let logs = self
            .fetch_http_logs_for_service(
                params.project_id,
                params.service_id,
                params.environment_id,
                params.deployment_id,
                params.lines,
            )
            .await?;

        let total = logs.len();
        if total == 0 {
            return Ok(CallToolResult::success(vec![Content::text(
                "No HTTP logs found.".to_string(),
            )]));
        }

        let mut counts = [0usize; 6]; // index by status/100: 0=other,1=1xx,2=2xx,3=3xx,4=4xx,5=5xx
        for log in &logs {
            let bucket = (log.http_status() / 100) as usize;
            if bucket < counts.len() {
                counts[bucket] += 1;
            } else {
                counts[0] += 1;
            }
        }

        Ok(CallToolResult::success(vec![Content::text(format!(
            "HTTP Requests (sampled {} logs):\n  Total: {}\n  2xx: {}\n  3xx: {}\n  4xx: {}\n  5xx: {}",
            total, total, counts[2], counts[3], counts[4], counts[5]
        ))]))
    }

    pub(crate) async fn do_http_error_rate(
        &self,
        params: HttpObservabilityParams,
    ) -> Result<CallToolResult, McpError> {
        let logs = self
            .fetch_http_logs_for_service(
                params.project_id,
                params.service_id,
                params.environment_id,
                params.deployment_id,
                params.lines,
            )
            .await?;

        let total = logs.len();
        if total == 0 {
            return Ok(CallToolResult::success(vec![Content::text(
                "No HTTP logs found.".to_string(),
            )]));
        }

        let errors = logs.iter().filter(|l| l.http_status() >= 400).count();

        let rate = (errors as f64 / total as f64) * 100.0;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "HTTP Error Rate (sampled {} requests):\n  Errors (4xx+5xx): {} ({:.1}%)\n  Success (1xx-3xx): {} ({:.1}%)",
            total,
            errors,
            rate,
            total - errors,
            100.0 - rate
        ))]))
    }

    pub(crate) async fn do_http_response_time(
        &self,
        params: HttpObservabilityParams,
    ) -> Result<CallToolResult, McpError> {
        let logs = self
            .fetch_http_logs_for_service(
                params.project_id,
                params.service_id,
                params.environment_id,
                params.deployment_id,
                params.lines,
            )
            .await?;

        let total = logs.len();
        if total == 0 {
            return Ok(CallToolResult::success(vec![Content::text(
                "No HTTP logs found.".to_string(),
            )]));
        }

        let mut durations: Vec<i64> = logs.iter().map(|l| l.total_duration()).collect();
        durations.sort_unstable();

        let percentile = |p: f64| -> i64 {
            let idx = ((durations.len() as f64 * p / 100.0) as usize)
                .min(durations.len().saturating_sub(1));
            durations[idx]
        };

        Ok(CallToolResult::success(vec![Content::text(format!(
            "HTTP Response Times (sampled {} requests):\n  p50: {}ms\n  p90: {}ms\n  p95: {}ms\n  p99: {}ms",
            total,
            percentile(50.0),
            percentile(90.0),
            percentile(95.0),
            percentile(99.0),
        ))]))
    }
}
