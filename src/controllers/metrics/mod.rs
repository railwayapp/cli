pub mod tui;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use colored::Colorize;
use reqwest::Client;
use serde::Serialize;

use crate::{
    client::post_graphql,
    commands::Configs,
    gql::queries::{
        self,
        metrics::{MetricMeasurement, MetricTag},
        project::ProjectProject,
    },
};

#[derive(Debug, Clone, Serialize)]
pub struct MetricsData {
    pub cpu_usage: Vec<MetricSample>,
    pub memory_usage_gb: Vec<MetricSample>,
    pub network_rx_gb: Vec<MetricSample>,
    pub network_tx_gb: Vec<MetricSample>,
    pub service_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricSample {
    pub timestamp: i64,
    pub value: f64,
}

pub fn parse_time_range(time_str: &str) -> Result<DateTime<Utc>> {
    let now = Utc::now();
    let duration = match time_str.to_lowercase().as_str() {
        "1h" => Duration::hours(1),
        "6h" => Duration::hours(6),
        "12h" => Duration::hours(12),
        "1d" | "24h" => Duration::days(1),
        "3d" => Duration::days(3),
        "7d" => Duration::days(7),
        _ => bail!(
            "Invalid time range '{}'. Valid options: 1h, 6h, 12h, 1d, 3d, 7d",
            time_str
        ),
    };

    Ok(now - duration)
}

pub async fn fetch_metrics(
    client: &Client,
    configs: &Configs,
    project_id: &str,
    environment_id: &str,
    service_id: Option<&str>,
    start_date: DateTime<Utc>,
) -> Result<Vec<MetricsData>> {
    let measurements = vec![
        MetricMeasurement::CPU_USAGE,
        MetricMeasurement::MEMORY_USAGE_GB,
        MetricMeasurement::NETWORK_RX_GB,
        MetricMeasurement::NETWORK_TX_GB,
    ];

    let group_by = Some(vec![MetricTag::SERVICE_ID]);

    let vars = queries::metrics::Variables {
        project_id: project_id.to_string(),
        environment_id: Some(environment_id.to_string()),
        service_id: service_id.map(|s| s.to_string()),
        measurements,
        start_date,
        end_date: None,
        sample_rate_seconds: Some(60),
        group_by,
    };

    let response = post_graphql::<queries::Metrics, _>(client, configs.get_backboard(), vars)
        .await
        .context("Failed to fetch metrics")?;

    let mut metrics_by_service: std::collections::HashMap<String, MetricsData> =
        std::collections::HashMap::new();

    for result in response.metrics {
        let service_id = result.tags.service_id.clone();
        let key = service_id.clone().unwrap_or_else(|| "unknown".to_string());

        let entry = metrics_by_service.entry(key).or_insert_with(|| MetricsData {
            cpu_usage: Vec::new(),
            memory_usage_gb: Vec::new(),
            network_rx_gb: Vec::new(),
            network_tx_gb: Vec::new(),
            service_id: service_id.clone(),
        });

        let samples: Vec<MetricSample> = result
            .values
            .iter()
            .map(|v| MetricSample {
                timestamp: v.ts.into(),
                value: v.value,
            })
            .collect();

        match result.measurement {
            MetricMeasurement::CPU_USAGE => entry.cpu_usage = samples,
            MetricMeasurement::MEMORY_USAGE_GB => entry.memory_usage_gb = samples,
            MetricMeasurement::NETWORK_RX_GB => entry.network_rx_gb = samples,
            MetricMeasurement::NETWORK_TX_GB => entry.network_tx_gb = samples,
            _ => {}
        }
    }

    Ok(metrics_by_service.into_values().collect())
}

pub fn print_metrics_table(metrics: &[MetricsData], project: &ProjectProject) -> Result<()> {
    if metrics.is_empty() {
        println!("{}", "No metrics data available.".yellow());
        return Ok(());
    }

    println!(
        "\n{:─<80}",
        format!(" {} ", "Service Metrics".bold().cyan())
    );
    println!(
        "{:<30} {:>12} {:>12} {:>10} {:>10}",
        "Service".bold(),
        "CPU".bold(),
        "Memory".bold(),
        "Net RX".bold(),
        "Net TX".bold()
    );
    println!("{:─<80}", "");

    for metric in metrics {
        let service_name = if let Some(ref sid) = metric.service_id {
            project
                .services
                .edges
                .iter()
                .find(|s| &s.node.id == sid)
                .map(|s| s.node.name.clone())
                .unwrap_or_else(|| sid.clone())
        } else {
            "Unknown".to_string()
        };

        let cpu = metric
            .cpu_usage
            .last()
            .map(|s| format!("{:.1}%", s.value * 100.0))
            .unwrap_or_else(|| "-".to_string());

        let mem = metric
            .memory_usage_gb
            .last()
            .map(|s| format_bytes(s.value * 1024.0 * 1024.0 * 1024.0))
            .unwrap_or_else(|| "-".to_string());

        let net_rx = metric
            .network_rx_gb
            .last()
            .map(|s| format_bytes(s.value * 1024.0 * 1024.0 * 1024.0))
            .unwrap_or_else(|| "-".to_string());

        let net_tx = metric
            .network_tx_gb
            .last()
            .map(|s| format_bytes(s.value * 1024.0 * 1024.0 * 1024.0))
            .unwrap_or_else(|| "-".to_string());

        let cpu_color = if let Some(s) = metric.cpu_usage.last() {
            if s.value > 0.8 {
                cpu.red()
            } else if s.value > 0.5 {
                cpu.yellow()
            } else {
                cpu.green()
            }
        } else {
            cpu.normal()
        };

        println!(
            "{:<30} {:>12} {:>12} {:>10} {:>10}",
            service_name.blue(),
            cpu_color,
            mem.normal(),
            net_rx.normal(),
            net_tx.normal()
        );

        if metric.cpu_usage.len() > 2 {
            let sparkline = generate_sparkline(&metric.cpu_usage, 40);
            println!("  CPU: {}", sparkline.dimmed());
        }
    }

    println!("{:─<80}\n", "");

    Ok(())
}

fn format_bytes(bytes: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    if bytes >= GB {
        format!("{:.2} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{:.0} B", bytes)
    }
}

pub fn generate_sparkline(samples: &[MetricSample], width: usize) -> String {
    if samples.is_empty() {
        return String::new();
    }

    let chars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

    let step = samples.len().max(1) / width.max(1);
    let step = step.max(1);

    let sampled: Vec<f64> = samples.iter().step_by(step).map(|s| s.value).collect();

    let min_val = sampled.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_val = sampled.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let range = max_val - min_val;

    sampled
        .iter()
        .take(width)
        .map(|&val| {
            let normalized = if range > 0.0 {
                (val - min_val) / range
            } else {
                0.5
            };
            let idx = ((normalized * 7.0).round() as usize).min(7);
            chars[idx]
        })
        .collect()
}
