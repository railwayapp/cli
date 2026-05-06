use rmcp::{ErrorData as McpError, model::*};

use crate::{
    controllers::metrics::{
        FetchResourceMetricsParams, compute_http_metrics, fetch_http_logs_for_deployment,
        fetch_resource_metrics,
    },
    gql::queries,
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

        let backboard = self.configs.get_backboard();
        fetch_http_logs_for_deployment(
            &self.client,
            &backboard,
            deployment_id,
            lines.unwrap_or(200),
            None,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to fetch HTTP logs: {e}"), None))
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

        let backboard = self.configs.get_backboard();
        let result = fetch_resource_metrics(FetchResourceMetricsParams {
            client: &self.client,
            backboard: &backboard,
            service_id: &ctx.service_id,
            environment_id: &ctx.environment_id,
            start_date,
            end_date: None,
            measurements,
            sample_rate_seconds: params.sample_rate_seconds,
            include_raw: false,
        })
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to fetch metrics: {e}"), None))?;

        if result.metrics.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No metrics data available for this service.".to_string(),
            )]));
        }

        let mut output = String::new();
        for metric in &result.metrics {
            output.push_str(&format!("\n### {}\n", metric.measurement));
            if metric.data_points == 0 {
                output.push_str("No data points.\n");
            } else {
                output.push_str(&format!(
                    "  Current: {:.4}\n  Average ({}pts): {:.4}\n  Min: {:.4}\n  Max: {:.4}\n",
                    metric.current, metric.data_points, metric.average, metric.min, metric.max
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

        match compute_http_metrics(&logs) {
            Some(result) => Ok(CallToolResult::success(vec![Content::text(format!(
                "HTTP Requests:\n  Total: {}\n  2xx: {}\n  3xx: {}\n  4xx: {}\n  5xx: {}",
                result.total,
                result.status_counts[2],
                result.status_counts[3],
                result.status_counts[4],
                result.status_counts[5]
            ))])),
            None => Ok(CallToolResult::success(vec![Content::text(
                "No HTTP logs found.".to_string(),
            )])),
        }
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

        match compute_http_metrics(&logs) {
            Some(result) => {
                let errors = result.status_counts[4] + result.status_counts[5];
                let error_rate = if result.total > 0 {
                    (errors as f64 / result.total as f64) * 100.0
                } else {
                    0.0
                };
                let success_rate = 100.0 - error_rate;
                let success = result.total - errors;
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "HTTP Error Rate (sampled {} requests):\n  Errors (4xx+5xx): {} ({:.1}%)\n  Success: {} ({:.1}%)",
                    result.total, errors, error_rate, success, success_rate
                ))]))
            }
            None => Ok(CallToolResult::success(vec![Content::text(
                "No HTTP logs found.".to_string(),
            )])),
        }
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

        match compute_http_metrics(&logs) {
            Some(result) => Ok(CallToolResult::success(vec![Content::text(format!(
                "HTTP Response Times (sampled {} requests):\n  p50: {}ms\n  p90: {}ms\n  p95: {}ms\n  p99: {}ms",
                result.total, result.p50_ms, result.p90_ms, result.p95_ms, result.p99_ms
            ))])),
            None => Ok(CallToolResult::success(vec![Content::text(
                "No HTTP logs found.".to_string(),
            )])),
        }
    }
}
