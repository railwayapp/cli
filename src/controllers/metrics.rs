use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use reqwest::Client;
use serde::Serialize;

use crate::{
    client::post_graphql,
    controllers::deployment::{FetchLogsParams, fetch_http_logs},
    gql::queries,
    queries::project::ProjectProject,
    util::logs::HttpLogLike,
};

#[derive(Debug, Clone, Serialize)]
pub struct MetricDataPoint {
    pub ts: i64,
    pub value: f64,
}

#[derive(Debug, Clone, Default)]
pub struct ChartData {
    pub x_min: f64,
    pub x_max: f64,
    pub points: Vec<(f64, f64)>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricSummary {
    pub measurement: String,
    pub current: f64,
    pub average: f64,
    pub min: f64,
    pub max: f64,
    pub data_points: usize,
    #[serde(skip_serializing)]
    pub raw_values: Vec<MetricDataPoint>,
    #[serde(skip_serializing)]
    pub chart_data: ChartData,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceMetricsResult {
    pub metrics: Vec<MetricSummary>,
}

#[derive(Debug, Clone)]
pub struct HttpTimeSeries {
    pub request_rate_ts: Vec<MetricDataPoint>,
    pub error_rate_ts: Vec<MetricDataPoint>,
    pub status_2xx_ts: Vec<MetricDataPoint>,
    pub status_3xx_ts: Vec<MetricDataPoint>,
    pub status_4xx_ts: Vec<MetricDataPoint>,
    pub status_5xx_ts: Vec<MetricDataPoint>,
    pub p50_ts: Vec<MetricDataPoint>,
    pub p90_ts: Vec<MetricDataPoint>,
    pub p95_ts: Vec<MetricDataPoint>,
    pub p99_ts: Vec<MetricDataPoint>,
    pub error_rate_chart: ChartData,
    pub p50_chart: ChartData,
    pub p90_chart: ChartData,
    pub p95_chart: ChartData,
    pub p99_chart: ChartData,
}

#[derive(Debug, Clone, Serialize)]
pub struct HttpMetricsResult {
    pub total: usize,
    pub status_counts: [usize; 6], // index 0=other,1=1xx,2=2xx,3=3xx,4=4xx,5=5xx
    pub error_rate: f64,
    pub p50_ms: i64,
    pub p90_ms: i64,
    pub p95_ms: i64,
    pub p99_ms: i64,
    #[serde(skip_serializing)]
    pub time_series: Option<HttpTimeSeries>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VolumeMetrics {
    pub volume_name: String,
    pub mount_path: String,
    pub current_size_mb: f64,
    pub limit_size_mb: f64,
}

pub struct FetchResourceMetricsParams<'a> {
    pub client: &'a Client,
    pub backboard: &'a str,
    pub service_id: &'a str,
    pub environment_id: &'a str,
    pub start_date: DateTime<Utc>,
    pub end_date: Option<DateTime<Utc>>,
    pub measurements: Vec<queries::metrics::MetricMeasurement>,
    pub sample_rate_seconds: Option<i64>,
    pub include_raw: bool,
}

pub async fn fetch_resource_metrics(
    params: FetchResourceMetricsParams<'_>,
) -> Result<ResourceMetricsResult> {
    let vars = queries::metrics::Variables {
        project_id: None,
        service_id: Some(params.service_id.to_string()),
        environment_id: Some(params.environment_id.to_string()),
        start_date: params.start_date,
        end_date: params.end_date,
        measurements: params.measurements,
        sample_rate_seconds: params.sample_rate_seconds,
        group_by: None,
    };

    let resp = post_graphql::<queries::Metrics, _>(params.client, params.backboard, vars).await?;

    let include_raw = params.include_raw;
    let metrics = resp
        .metrics
        .iter()
        .map(|m| summarize_metric(m, include_raw))
        .collect();

    Ok(ResourceMetricsResult { metrics })
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceMetricsSummary {
    pub service_id: String,
    pub service_name: String,
    pub cpu: Option<MetricSummary>,
    pub cpu_limit: Option<MetricSummary>,
    pub memory: Option<MetricSummary>,
    pub memory_limit: Option<MetricSummary>,
    pub network_tx: Option<MetricSummary>,
    pub network_rx: Option<MetricSummary>,
    pub http: Option<HttpMetricsResult>,
    pub volumes: Vec<VolumeMetrics>,
    pub is_database: bool,
}

pub struct FetchProjectMetricsParams<'a> {
    pub client: &'a Client,
    pub backboard: &'a str,
    pub project_id: &'a str,
    pub environment_id: &'a str,
    pub start_date: DateTime<Utc>,
    pub end_date: Option<DateTime<Utc>>,
    pub measurements: Vec<queries::metrics::MetricMeasurement>,
    pub sample_rate_seconds: Option<i64>,
}

fn empty_service_metrics_summary(
    service_id: String,
    service_name: String,
) -> ServiceMetricsSummary {
    ServiceMetricsSummary {
        service_id,
        service_name,
        cpu: None,
        cpu_limit: None,
        memory: None,
        memory_limit: None,
        network_tx: None,
        network_rx: None,
        http: None,
        volumes: vec![],
        is_database: false,
    }
}

fn sort_project_services(services: &mut [ServiceMetricsSummary], sort_by_cpu: bool) {
    services.sort_by(|a, b| {
        let name_cmp = a
            .service_name
            .to_ascii_lowercase()
            .cmp(&b.service_name.to_ascii_lowercase());
        if !sort_by_cpu {
            return name_cmp;
        }

        let a_cpu = a.cpu.as_ref().map(|c| c.current).unwrap_or(0.0);
        let b_cpu = b.cpu.as_ref().map(|c| c.current).unwrap_or(0.0);
        b_cpu
            .partial_cmp(&a_cpu)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(name_cmp)
    });
}

fn chart_data_from_points(points: &[MetricDataPoint]) -> ChartData {
    let Some(first) = points.first() else {
        return ChartData {
            x_min: 0.0,
            x_max: 1.0,
            ..ChartData::default()
        };
    };

    chart_data_from_points_with_origin(points, first.ts)
}

fn chart_data_from_points_with_origin(points: &[MetricDataPoint], origin_ts: i64) -> ChartData {
    let end_ts = points.last().map(|point| point.ts).unwrap_or(origin_ts);
    let range = (end_ts - origin_ts).max(1) as f64;
    let labels = time_labels(origin_ts, end_ts);
    let origin = origin_ts as f64;
    let chart_points = points
        .iter()
        .map(|point| (point.ts as f64 - origin, point.value))
        .collect();

    ChartData {
        x_min: 0.0,
        x_max: range,
        points: chart_points,
        labels,
    }
}

fn time_labels(start_ts: i64, end_ts: i64) -> Vec<String> {
    let range = (end_ts - start_ts).max(1) as f64;
    let step = range / 4.0;
    (0..=4)
        .map(|i| {
            let ts = start_ts as f64 + step * i as f64;
            chrono::DateTime::from_timestamp(ts as i64, 0)
                .map(|dt| {
                    let local: chrono::DateTime<chrono::Local> = dt.into();
                    local.format("%-I:%M %p").to_string()
                })
                .unwrap_or_default()
        })
        .collect()
}

pub async fn fetch_project_metrics(
    params: FetchProjectMetricsParams<'_>,
    project: &ProjectProject,
) -> Result<Vec<ServiceMetricsSummary>> {
    let sort_by_cpu = params
        .measurements
        .iter()
        .any(|measurement| matches!(measurement, queries::metrics::MetricMeasurement::CPU_USAGE));
    let vars = queries::metrics::Variables {
        project_id: Some(params.project_id.to_string()),
        service_id: None,
        environment_id: Some(params.environment_id.to_string()),
        start_date: params.start_date,
        end_date: params.end_date,
        measurements: params.measurements,
        sample_rate_seconds: params.sample_rate_seconds,
        group_by: Some(vec![queries::metrics::MetricTag::SERVICE_ID]),
    };

    let resp = post_graphql::<queries::Metrics, _>(params.client, params.backboard, vars).await?;

    let mut by_service: HashMap<String, ServiceMetricsSummary> = project
        .environments
        .edges
        .iter()
        .find(|env| env.node.id == params.environment_id)
        .map(|env| {
            env.node
                .service_instances
                .edges
                .iter()
                .map(|service_instance| {
                    (
                        service_instance.node.service_id.clone(),
                        empty_service_metrics_summary(
                            service_instance.node.service_id.clone(),
                            service_instance.node.service_name.clone(),
                        ),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    for m in &resp.metrics {
        let Some(service_id) = m.tags.service_id.clone() else {
            continue;
        };

        let Some(entry) = by_service.get_mut(&service_id) else {
            continue;
        };

        let summary = summarize_metric(m, false);
        match m.measurement {
            queries::metrics::MetricMeasurement::CPU_USAGE => entry.cpu = Some(summary),
            queries::metrics::MetricMeasurement::CPU_LIMIT => entry.cpu_limit = Some(summary),
            queries::metrics::MetricMeasurement::MEMORY_USAGE_GB => entry.memory = Some(summary),
            queries::metrics::MetricMeasurement::MEMORY_LIMIT_GB => {
                entry.memory_limit = Some(summary)
            }
            queries::metrics::MetricMeasurement::NETWORK_TX_GB => entry.network_tx = Some(summary),
            queries::metrics::MetricMeasurement::NETWORK_RX_GB => entry.network_rx = Some(summary),
            _ => {}
        }
    }

    let mut services: Vec<ServiceMetricsSummary> = by_service.into_values().collect();
    sort_project_services(&mut services, sort_by_cpu);

    Ok(services)
}

fn summarize_metric(m: &queries::metrics::MetricsMetrics, include_raw: bool) -> MetricSummary {
    if m.values.is_empty() {
        MetricSummary {
            measurement: format!("{:?}", m.measurement),
            current: 0.0,
            average: 0.0,
            min: 0.0,
            max: 0.0,
            data_points: 0,
            raw_values: vec![],
            chart_data: ChartData {
                x_min: 0.0,
                x_max: 1.0,
                ..ChartData::default()
            },
        }
    } else {
        let values: Vec<f64> = m.values.iter().map(|v| v.value).collect();
        let current = values.last().copied().unwrap_or(0.0);
        let sum: f64 = values.iter().sum();
        let avg = sum / values.len() as f64;
        let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let raw_values = if include_raw {
            m.values
                .iter()
                .map(|v| MetricDataPoint {
                    ts: v.ts,
                    value: v.value,
                })
                .collect()
        } else {
            vec![]
        };
        let chart_data = chart_data_from_points(&raw_values);

        MetricSummary {
            measurement: format!("{:?}", m.measurement),
            current,
            average: avg,
            min,
            max,
            data_points: values.len(),
            raw_values,
            chart_data,
        }
    }
}

pub fn compute_http_metrics(logs: &[impl HttpLogLike]) -> Option<HttpMetricsResult> {
    let total = logs.len();
    if total == 0 {
        return None;
    }

    let mut counts = [0usize; 6];
    for log in logs {
        let bucket = (log.http_status() / 100) as usize;
        if bucket < counts.len() {
            counts[bucket] += 1;
        } else {
            counts[0] += 1;
        }
    }

    let errors = logs.iter().filter(|l| l.http_status() >= 500).count();
    let error_rate = (errors as f64 / total as f64) * 100.0;

    let mut durations: Vec<i64> = logs.iter().map(|l| l.total_duration()).collect();
    durations.sort_unstable();

    let percentile = |p: f64| -> i64 {
        let idx =
            ((durations.len() as f64 * p / 100.0) as usize).min(durations.len().saturating_sub(1));
        durations[idx]
    };

    Some(HttpMetricsResult {
        total,
        status_counts: counts,
        error_rate,
        p50_ms: percentile(50.0),
        p90_ms: percentile(90.0),
        p95_ms: percentile(95.0),
        p99_ms: percentile(99.0),
        time_series: None,
    })
}

pub async fn fetch_http_logs_for_deployment(
    client: &Client,
    backboard: &str,
    deployment_id: String,
    limit: i64,
    filter: Option<String>,
) -> Result<Vec<queries::http_logs::HttpLogFields>> {
    let mut logs: Vec<queries::http_logs::HttpLogFields> = Vec::new();
    let fetch_params = FetchLogsParams {
        client,
        backboard,
        deployment_id,
        limit: Some(limit),
        filter,
        start_date: None,
        end_date: None,
    };

    fetch_http_logs(fetch_params, |log| {
        logs.push(log);
    })
    .await?;

    Ok(logs)
}

pub struct FetchHttpMetricsParams<'a> {
    pub client: &'a Client,
    pub backboard: &'a str,
    pub service_id: &'a str,
    pub environment_id: &'a str,
    pub start_date: DateTime<Utc>,
    pub end_date: DateTime<Utc>,
    pub step_seconds: Option<i64>,
    pub method: Option<String>,
    pub path: Option<String>,
    /// When true, populate time-series fields in HttpMetricsResult (for TUI sparklines)
    pub include_time_series: bool,
}

/// Fetch HTTP metrics using the dedicated pre-aggregated queries.
/// Returns None if there is no HTTP traffic in the time window.
pub async fn fetch_http_metrics(
    params: FetchHttpMetricsParams<'_>,
) -> Result<Option<HttpMetricsResult>> {
    // Fetch status code breakdown
    let status_vars = queries::http_metrics_by_status::Variables {
        service_id: params.service_id.to_string(),
        environment_id: params.environment_id.to_string(),
        start_date: params.start_date,
        end_date: params.end_date,
        step_seconds: params.step_seconds,
        method: params.method.clone(),
        path: params.path.clone(),
    };

    let status_resp = post_graphql::<queries::HttpMetricsByStatus, _>(
        params.client,
        params.backboard,
        status_vars,
    )
    .await?;

    let mut counts = [0usize; 6]; // 0=other, 1=1xx, 2=2xx, 3=3xx, 4=4xx, 5=5xx
    let mut ts_totals: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
    let mut ts_2xx: BTreeMap<i64, f64> = BTreeMap::new();
    let mut ts_3xx: BTreeMap<i64, f64> = BTreeMap::new();
    let mut ts_4xx: BTreeMap<i64, f64> = BTreeMap::new();
    let mut ts_5xx: BTreeMap<i64, f64> = BTreeMap::new();
    for group in &status_resp.http_metrics_grouped_by_status {
        let bucket = (group.status_code / 100) as usize;
        let is_5xx = bucket == 5;
        let total_for_status: f64 = group.samples.iter().map(|s| s.value).sum();
        let count = total_for_status.round() as usize;
        if bucket < counts.len() {
            counts[bucket] += count;
        } else {
            counts[0] += count;
        }
        if params.include_time_series {
            let mut bucket_map = match bucket {
                2 => Some(&mut ts_2xx),
                3 => Some(&mut ts_3xx),
                4 => Some(&mut ts_4xx),
                5 => Some(&mut ts_5xx),
                _ => None,
            };
            for sample in &group.samples {
                let entry = ts_totals.entry(sample.ts).or_insert((0.0, 0.0));
                entry.0 += sample.value;
                if is_5xx {
                    entry.1 += sample.value;
                }
                if let Some(ref mut bmap) = bucket_map {
                    *bmap.entry(sample.ts).or_insert(0.0) += sample.value;
                }
            }
        }
    }

    let total: usize = counts.iter().sum();
    if total == 0 {
        return Ok(None);
    }

    let error_rate = if total > 0 {
        (counts[5] as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    // Summary output needs the percentile for the whole requested window.
    // Time-series callers still pass a step so the TUI/raw output can chart buckets.
    let duration_step_seconds = if params.include_time_series {
        params.step_seconds
    } else {
        None
    };

    // Fetch duration percentiles
    let duration_vars = queries::http_duration_metrics::Variables {
        service_id: params.service_id.to_string(),
        environment_id: params.environment_id.to_string(),
        start_date: params.start_date,
        end_date: params.end_date,
        step_seconds: duration_step_seconds,
        method: params.method,
        path: params.path,
        status_code: None,
    };

    let duration_resp = post_graphql::<queries::HttpDurationMetrics, _>(
        params.client,
        params.backboard,
        duration_vars,
    )
    .await?;

    let samples = &duration_resp.http_duration_metrics.samples;
    let (p50, p90, p95, p99) = if let Some(sample) = samples.last() {
        // Aggregate calls return one sample; bucketed calls use the latest sample as current.
        (
            sample.p50.round() as i64,
            sample.p90.round() as i64,
            sample.p95.round() as i64,
            sample.p99.round() as i64,
        )
    } else {
        (0, 0, 0, 0)
    };

    let btree_to_vec = |map: &BTreeMap<i64, f64>| -> Vec<MetricDataPoint> {
        map.iter()
            .map(|(&ts, &value)| MetricDataPoint { ts, value })
            .collect()
    };

    let time_series = if params.include_time_series {
        let request_rate_ts: Vec<MetricDataPoint> = ts_totals
            .iter()
            .map(|(&ts, &(total, _))| MetricDataPoint { ts, value: total })
            .collect();
        let error_rate_ts: Vec<MetricDataPoint> = ts_totals
            .iter()
            .map(|(&ts, &(_, errors))| MetricDataPoint { ts, value: errors })
            .collect();
        let p50_ts: Vec<MetricDataPoint> = samples
            .iter()
            .map(|s| MetricDataPoint {
                ts: s.ts,
                value: s.p50,
            })
            .collect();
        let p90_ts: Vec<MetricDataPoint> = samples
            .iter()
            .map(|s| MetricDataPoint {
                ts: s.ts,
                value: s.p90,
            })
            .collect();
        let p95_ts: Vec<MetricDataPoint> = samples
            .iter()
            .map(|s| MetricDataPoint {
                ts: s.ts,
                value: s.p95,
            })
            .collect();
        let p99_ts: Vec<MetricDataPoint> = samples
            .iter()
            .map(|s| MetricDataPoint {
                ts: s.ts,
                value: s.p99,
            })
            .collect();
        let error_rate_chart = chart_data_from_points(&error_rate_ts);
        let status_2xx_ts = btree_to_vec(&ts_2xx);
        let status_3xx_ts = btree_to_vec(&ts_3xx);
        let status_4xx_ts = btree_to_vec(&ts_4xx);
        let status_5xx_ts = btree_to_vec(&ts_5xx);
        let p50_chart = chart_data_from_points(&p50_ts);
        let p90_chart = chart_data_from_points(&p90_ts);
        let p95_chart = chart_data_from_points(&p95_ts);
        let p99_chart = chart_data_from_points(&p99_ts);
        Some(HttpTimeSeries {
            request_rate_ts,
            error_rate_ts,
            status_2xx_ts,
            status_3xx_ts,
            status_4xx_ts,
            status_5xx_ts,
            p50_ts,
            p90_ts,
            p95_ts,
            p99_ts,
            error_rate_chart,
            p50_chart,
            p90_chart,
            p95_chart,
            p99_chart,
        })
    } else {
        None
    };

    Ok(Some(HttpMetricsResult {
        total,
        status_counts: counts,
        error_rate,
        p50_ms: p50,
        p90_ms: p90,
        p95_ms: p95,
        p99_ms: p99,
        time_series,
    }))
}

pub fn is_database_service(source_image: Option<&str>) -> bool {
    source_image
        .map(|img| img.to_lowercase())
        .is_some_and(|img| {
            img.contains("postgres")
                || img.contains("postgis")
                || img.contains("timescale")
                || img.contains("redis")
                || img.contains("mongo")
                || img.contains("mysql")
                || img.contains("mariadb")
                || img.contains("memcached")
                || img.contains("valkey")
        })
}

pub fn get_volume_metrics(
    project: &ProjectProject,
    environment_id: &str,
    service_id: &str,
) -> Vec<VolumeMetrics> {
    project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment_id)
        .map(|env| {
            env.node
                .volume_instances
                .edges
                .iter()
                .filter(|vi| {
                    vi.node.service_id.as_deref() == Some(service_id)
                        && vi.node.environment_id == environment_id
                })
                .map(|vi| VolumeMetrics {
                    volume_name: vi.node.volume.name.clone(),
                    mount_path: vi.node.mount_path.clone(),
                    current_size_mb: vi.node.current_size_mb,
                    limit_size_mb: vi.node.size_mb as f64,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Maps time window duration to an appropriate sample rate in seconds,
/// matching the Railway dashboard behavior.
pub fn compute_sample_rate(duration: Duration) -> i64 {
    let hours = duration.num_hours();
    match hours {
        0..=1 => 30,
        2..=6 => 60,
        7..=24 => 240,
        25..=168 => 3600, // up to 7 days
        _ => 14400,       // 30 days+
    }
}

/// Find a metric by measurement name, filtering out entries with no data points.
pub fn find_metric<'a>(metrics: &'a [MetricSummary], name: &str) -> Option<&'a MetricSummary> {
    metrics
        .iter()
        .find(|m| m.measurement == name)
        .filter(|m| m.data_points > 0)
}

/// Format a CPU value with vCPU unit
pub fn format_cpu(value: f64) -> String {
    if value == 0.0 {
        "0 vCPU".to_string()
    } else if value < 0.01 {
        "< 0.01 vCPU".to_string()
    } else if value < 1.0 {
        format!("{:.2} vCPU", value)
    } else {
        format!("{:.1} vCPU", value)
    }
}

/// Format a value in GB to human-friendly units (MB or GB)
pub fn format_gb(value_gb: f64) -> String {
    if value_gb == 0.0 {
        "0 MB".to_string()
    } else if value_gb < 0.001 {
        format!("{:.2} MB", value_gb * 1024.0)
    } else if value_gb < 1.0 {
        let mb = value_gb * 1024.0;
        if mb < 10.0 {
            format!("{:.1} MB", mb)
        } else {
            format!("{:.0} MB", mb)
        }
    } else if value_gb < 10.0 {
        format!("{:.2} GB", value_gb)
    } else {
        format!("{:.1} GB", value_gb)
    }
}

/// Compute utilization percentage, returns None if limit is zero or missing
pub fn utilization(current: f64, limit: Option<f64>) -> Option<f64> {
    limit.filter(|&l| l > 0.0).map(|l| (current / l) * 100.0)
}

/// Compute percentage of count / total, returning 0.0 when total is zero
pub fn pct(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (count as f64 / total as f64) * 100.0
    }
}

/// Format a count with K/M suffixes for readability
pub fn format_count(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format a value in MB to human-friendly units (MB or GB)
pub fn format_mb(value_mb: f64) -> String {
    if value_mb < 1024.0 {
        if value_mb < 10.0 {
            format!("{:.1} MB", value_mb)
        } else {
            format!("{:.0} MB", value_mb)
        }
    } else {
        let gb = value_mb / 1024.0;
        if gb < 10.0 {
            format!("{:.2} GB", gb)
        } else {
            format!("{:.1} GB", gb)
        }
    }
}
