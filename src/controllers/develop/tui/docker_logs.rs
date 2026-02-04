use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use anyhow::Result;
use colored::Color;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::controllers::develop::{COLORS, LogLine};

/// Maps slug -> (display_name, color)
pub type ServiceMapping = HashMap<String, (String, Color)>;

pub async fn spawn_docker_logs(
    compose_path: &Path,
    service_mapping: ServiceMapping,
    tx: mpsc::Sender<LogLine>,
) -> Result<Child> {
    let mut child = Command::new("docker")
        .args([
            "compose",
            "-f",
            &compose_path.to_string_lossy(),
            "logs",
            "-f",
            "--no-color",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout piped");
    let mapping_clone = service_mapping.clone();
    let tx_clone = tx.clone();

    tokio::spawn(async move {
        parse_docker_logs(stdout, mapping_clone, tx_clone).await;
    });

    let stderr = child.stderr.take().expect("stderr piped");
    tokio::spawn(async move {
        parse_docker_logs(stderr, service_mapping, tx).await;
    });

    Ok(child)
}

async fn parse_docker_logs<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    service_mapping: ServiceMapping,
    tx: mpsc::Sender<LogLine>,
) {
    let mut lines = BufReader::new(reader).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(log) = parse_log_line(&line, &service_mapping) {
            if tx.send(log).await.is_err() {
                break;
            }
        }
    }
}

fn parse_log_line(line: &str, service_mapping: &ServiceMapping) -> Option<LogLine> {
    // Docker compose logs format: "service-name-1  | message"
    // The -1 suffix is the container instance number
    if let Some(pipe_idx) = line.find(" | ") {
        let service_part = line[..pipe_idx].trim();

        // Skip railway-proxy infrastructure logs
        if service_part.starts_with("railway-proxy") {
            return None;
        }

        let message = line[pipe_idx + 3..].to_string();

        // Try to match to a known service by slug
        // Docker format is typically "slug-1" or just "slug"
        for (slug, (display_name, color)) in service_mapping {
            // Match "slug-N" or exact "slug"
            if service_part == slug
                || service_part.starts_with(&format!("{slug}-"))
                || service_part.starts_with(&format!("{slug}_"))
            {
                return Some(LogLine {
                    service_name: display_name.clone(),
                    message,
                    is_stderr: false,
                    color: *color,
                });
            }
        }

        return Some(LogLine {
            service_name: service_part.to_string(),
            message,
            is_stderr: false,
            color: COLORS[0],
        });
    }

    None
}
