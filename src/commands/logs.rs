use std::fmt;
use std::str::FromStr;

use is_terminal::IsTerminal;
use tokio::task::JoinSet;

use crate::{
    controllers::{
        deployment::{
            FetchLogsParams, fetch_build_logs, fetch_deploy_logs, fetch_http_logs,
            stream_build_logs, stream_deploy_logs, stream_http_logs,
        },
        project::resolve_service_context,
    },
    util::{
        logs::{LogFormat, format_log_string_plain, print_http_log, print_log},
        time::parse_time,
    },
};
use anyhow::bail;

use super::{
    queries::deployments::{DeploymentListInput, DeploymentStatus},
    *,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum HttpMethod {
    #[value(name = "GET")]
    Get,
    #[value(name = "POST")]
    Post,
    #[value(name = "PUT")]
    Put,
    #[value(name = "DELETE")]
    Delete,
    #[value(name = "PATCH")]
    Patch,
    #[value(name = "HEAD")]
    Head,
    #[value(name = "OPTIONS")]
    Options,
}

impl fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Get => write!(f, "GET"),
            Self::Post => write!(f, "POST"),
            Self::Put => write!(f, "PUT"),
            Self::Delete => write!(f, "DELETE"),
            Self::Patch => write!(f, "PATCH"),
            Self::Head => write!(f, "HEAD"),
            Self::Options => write!(f, "OPTIONS"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum StatusFilter {
    Exact(u16),
    Comparison { op: &'static str, value: u16 },
    Range { low: u16, high: u16 },
}

impl StatusFilter {
    fn to_filter_expr(&self) -> String {
        match self {
            Self::Exact(v) => format!("@httpStatus:{v}"),
            Self::Comparison { op, value } => format!("@httpStatus:{op}{value}"),
            Self::Range { low, high } => format!("@httpStatus:{low}..{high}"),
        }
    }
}

impl FromStr for StatusFilter {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Try range first: "200..299"
        if let Some((low, high)) = s.split_once("..") {
            let low: u16 = low
                .parse()
                .map_err(|_| format!("Invalid status range start: {low}"))?;
            let high: u16 = high
                .parse()
                .map_err(|_| format!("Invalid status range end: {high}"))?;
            if low > high {
                return Err(format!("Range start ({low}) must be <= end ({high})"));
            }
            return Ok(Self::Range { low, high });
        }

        // Try comparison: ">=400", ">399", "<=499", "<500"
        for prefix in &[">=", "<=", ">", "<"] {
            if let Some(rest) = s.strip_prefix(prefix) {
                let value: u16 = rest
                    .parse()
                    .map_err(|_| format!("Invalid status code: {rest}"))?;
                return Ok(Self::Comparison { op: prefix, value });
            }
        }

        // Exact: "200"
        let value: u16 = s.parse().map_err(|_| {
            format!(
                "Invalid status filter: \"{s}\". Expected a number (200), comparison (>=400), or range (500..599)"
            )
        })?;
        Ok(Self::Exact(value))
    }
}

fn build_http_filter(args: &Args) -> Option<String> {
    compose_http_filter(
        args.method.as_ref(),
        args.status.as_ref(),
        args.path.as_deref(),
        args.request_id.as_deref(),
        args.filter.as_deref(),
    )
}

/// Build a Railway query filter string from typed HTTP filter components.
pub fn compose_http_filter(
    method: Option<&HttpMethod>,
    status: Option<&StatusFilter>,
    path: Option<&str>,
    request_id: Option<&str>,
    raw_filter: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    if let Some(method) = method {
        parts.push(format!("@method:{method}"));
    }

    if let Some(status) = status {
        parts.push(status.to_filter_expr());
    }

    if let Some(path) = path {
        parts.push(format!("@path:{path}"));
    }

    if let Some(request_id) = request_id {
        parts.push(format!("@requestId:{request_id}"));
    }

    if let Some(raw_filter) = raw_filter {
        if !raw_filter.is_empty() {
            parts.push(raw_filter.to_string());
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedLogsDeployment {
    pub deployment_id: String,
    pub default_deployment_id: String,
    pub default_deployment_status: DeploymentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeployLogTarget {
    pub service_name: String,
    pub deployment_id: String,
}

pub(crate) async fn resolve_logs_deployment(
    client: &reqwest::Client,
    backboard: &str,
    project_id: &str,
    environment_id: &str,
    service_id: &str,
    deployment_id: Option<String>,
    latest: bool,
) -> Result<ResolvedLogsDeployment> {
    let vars = queries::deployments::Variables {
        input: DeploymentListInput {
            project_id: Some(project_id.to_string()),
            environment_id: Some(environment_id.to_string()),
            service_id: Some(service_id.to_string()),
            include_deleted: None,
            status: None,
        },
        first: None,
    };
    let deployments = post_graphql::<queries::Deployments, _>(client, backboard, vars)
        .await?
        .deployments;
    let mut all_deployments: Vec<_> = deployments
        .edges
        .into_iter()
        .map(|deployment| deployment.node)
        .collect();
    all_deployments.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let default_deployment = if latest {
        all_deployments.first()
    } else {
        all_deployments
            .iter()
            .find(|deployment| deployment.status == DeploymentStatus::SUCCESS)
            .or_else(|| all_deployments.first())
    }
    .context("No deployments found")?;

    Ok(ResolvedLogsDeployment {
        deployment_id: deployment_id.unwrap_or_else(|| default_deployment.id.clone()),
        default_deployment_id: default_deployment.id.clone(),
        default_deployment_status: default_deployment.status.clone(),
    })
}

pub(crate) async fn fetch_environment_deploy_log_lines(
    client: &reqwest::Client,
    backboard: &str,
    targets: &[DeployLogTarget],
    limit_per_target: Option<i64>,
    filter: Option<String>,
) -> Result<Vec<String>> {
    let mut merged_lines = Vec::new();

    for target in targets {
        let service_name = target.service_name.clone();
        let mut target_lines = Vec::new();
        fetch_deploy_logs(
            FetchLogsParams {
                client,
                backboard,
                deployment_id: target.deployment_id.clone(),
                limit: limit_per_target,
                filter: filter.clone(),
                start_date: None,
                end_date: None,
            },
            |log| {
                target_lines.push((
                    log.timestamp.clone(),
                    format!(
                        "[{}] {}",
                        service_name,
                        format_log_string_plain(&log, LogFormat::Full)
                    ),
                ));
            },
        )
        .await?;
        merged_lines.extend(target_lines);
    }

    merged_lines.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(merged_lines.into_iter().map(|(_, line)| line).collect())
}

pub(crate) async fn stream_environment_deploy_log_lines(
    targets: Vec<DeployLogTarget>,
    filter: Option<String>,
    mut on_line: impl FnMut(String) + Send + 'static,
) -> Result<()> {
    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut streams = JoinSet::new();

    for target in targets {
        let line_tx = line_tx.clone();
        let filter = filter.clone();
        streams.spawn(async move {
            let service_name = target.service_name;
            stream_deploy_logs(target.deployment_id, filter, move |log| {
                let _ = line_tx.send(format!(
                    "[{}] {}",
                    service_name,
                    format_log_string_plain(&log, LogFormat::Full)
                ));
            })
            .await
        });
    }
    drop(line_tx);

    let mut first_error = None;
    let mut streams_done = streams.is_empty();
    let mut channel_open = true;

    while !streams_done || channel_open {
        tokio::select! {
            maybe_line = line_rx.recv(), if channel_open => {
                match maybe_line {
                    Some(line) => on_line(line),
                    None => channel_open = false,
                }
            }
            maybe_result = streams.join_next(), if !streams_done => {
                match maybe_result {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(error))) if first_error.is_none() => first_error = Some(error),
                    Some(Err(error)) if first_error.is_none() => first_error = Some(error.into()),
                    Some(_) => {}
                    None => streams_done = true,
                }
            }
        }
    }

    if let Some(error) = first_error {
        Err(error)
    } else {
        Ok(())
    }
}

#[derive(Parser)]
#[clap(
    about = "View build, deploy, or HTTP logs from a Railway deployment",
    long_about = "View build, deploy, or HTTP logs from a Railway deployment. This will stream logs by default, or fetch historical logs if the --lines, --since, or --until flags are provided.",
    after_help = "Examples:

  Deployment logs:
  railway logs                                                       # Stream live logs from latest deployment
  railway logs --build 7422c95b-c604-46bc-9de4-b7a43e1fd53d          # Stream build logs from a specific deployment
  railway logs --lines 100                                           # Pull last 100 logs without streaming
  railway logs --since 1h                                            # View logs from the last hour
  railway logs --since 30m --until 10m                               # View logs from 30 minutes ago until 10 minutes ago
  railway logs --since 2024-01-15T10:00:00Z                          # View logs since a specific timestamp
  railway logs --service backend --environment production            # Stream logs from a specific service/environment
  railway logs --lines 10 --filter \"@level:error\"                    # View 10 latest error logs
  railway logs --lines 10 --filter \"@level:warn AND rate limit\"      # View 10 latest warning logs related to rate limiting
  railway logs --json                                                # Get logs in JSON format
  railway logs --latest                                              # Stream logs from the latest deployment (even if failed/building)

  HTTP logs (typed filters):
  railway logs --http --method GET --status 200                      # GET requests with 200 status
  railway logs --http --method POST --path /api/users                # POST requests to /api/users
  railway logs --http --status \">=400\" --lines 50                    # Client/server errors, last 50
  railway logs --http --status 500..599                              # Server errors only
  railway logs --http --request-id abc123                            # Find a specific request

  HTTP logs (raw filter for advanced queries):
  railway logs --http --method GET --filter \"@totalDuration:>=1000\"  # Slow GET requests (combining typed + raw)
  railway logs --http --filter \"@srcIp:203.0.113.1 @edgeRegion:us-east-1\"  # Filter by source IP and region
  railway logs --http --filter \"@httpStatus:>=400 AND @path:/api\"   # Errors on API routes
  railway logs --http --filter \"-@method:OPTIONS\"                    # Exclude OPTIONS requests"
)]
pub struct Args {
    /// Service to view logs from (defaults to linked service). Can be service name or service ID
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to view logs from (defaults to linked environment). Can be environment name or environment ID
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Show deployment logs
    #[clap(short, long, group = "log_type")]
    deployment: bool,

    /// Show build logs
    #[clap(short, long, group = "log_type")]
    build: bool,

    /// Show HTTP request logs
    #[clap(long, group = "log_type")]
    http: bool,

    /// Deployment ID to view logs from. Defaults to most recent successful deployment, or latest deployment if none succeeded
    deployment_id: Option<String>,

    /// Output logs in JSON format. Each log line becomes a JSON object with timestamp, message, and any other attributes
    #[clap(long)]
    json: bool,

    /// Number of log lines to fetch (disables streaming)
    #[clap(short = 'n', long = "lines", visible_alias = "tail")]
    lines: Option<i64>,

    /// Filter logs using Railway's query syntax
    #[clap(
        long,
        short = 'f',
        long_help = "\
Filter logs using Railway's query syntax

For deploy/build logs:
  Text search:   \"error message\", \"user signup\"
  Level filter:  @level:error, @level:warn, @level:info

For HTTP logs (--http), all filterable fields:
  String:  @method, @path, @host, @requestId, @clientUa, @srcIp,
           @edgeRegion, @upstreamAddress, @upstreamProto,
           @downstreamProto, @responseDetails,
           @deploymentId, @deploymentInstanceId
  Numeric: @httpStatus, @totalDuration, @responseTime,
           @upstreamRqDuration, @txBytes, @rxBytes, @upstreamErrors

Numeric operators: > >= < <= .. (range, e.g. @httpStatus:200..299)
Logical operators: AND, OR, - (negation), parentheses for grouping

Examples:
  @httpStatus:>=400
  @totalDuration:>1000
  -@method:OPTIONS
  @httpStatus:>=400 AND @path:/api
  (@method:GET OR @method:POST) AND @httpStatus:500"
    )]
    filter: Option<String>,

    /// Filter HTTP logs by request method (requires --http)
    #[clap(long, requires = "http", value_enum, ignore_case = true)]
    method: Option<HttpMethod>,

    /// Filter HTTP logs by status code (requires --http). Accepts: 200, >=400, 500..599
    #[clap(long, requires = "http", value_name = "CODE")]
    status: Option<StatusFilter>,

    /// Filter HTTP logs by request path (requires --http)
    #[clap(long, requires = "http", value_name = "PATH")]
    path: Option<String>,

    /// Filter HTTP logs by request ID (requires --http)
    #[clap(long = "request-id", requires = "http", value_name = "ID")]
    request_id: Option<String>,

    /// Always show logs from the latest deployment, even if it failed or is still building
    #[clap(long)]
    latest: bool,

    /// Show logs since a specific time (disables streaming). Accepts relative times (e.g., 30s, 5m, 2h, 1d, 1w) or ISO 8601 timestamps (e.g., 2024-01-15T10:30:00Z)
    #[clap(long, short = 'S', value_name = "TIME")]
    since: Option<String>,

    /// Show logs until a specific time (disables streaming). Same formats as --since
    #[clap(long, short = 'U', value_name = "TIME")]
    until: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let backboard = configs.get_backboard();

    // Build filter before args is partially moved by service matching below
    let http_filter = build_http_filter(&args);

    let start_date = args.since.as_ref().map(|s| parse_time(s)).transpose()?;
    let end_date = args.until.as_ref().map(|s| parse_time(s)).transpose()?;

    if let (Some(s), Some(e)) = (&start_date, &end_date) {
        if s >= e {
            bail!("--since time must be before --until time");
        }
    }

    let has_time_filter = start_date.is_some() || end_date.is_some();

    // Stream only if no line limit or time filter is specified and running in a terminal
    let should_stream = args.lines.is_none() && !has_time_filter && std::io::stdout().is_terminal();

    let ctx = resolve_service_context(args.project, args.service, args.environment).await?;
    let project_id = ctx.project_id;
    let environment_id = ctx.environment_id;
    let service = ctx.service_id;

    let resolved_deployment = resolve_logs_deployment(
        &client,
        &backboard,
        &project_id,
        &environment_id,
        &service,
        args.deployment_id.clone(),
        args.latest,
    )
    .await?;
    let deployment_id = resolved_deployment.deployment_id.clone();

    let show_http_logs = args.http;
    let show_build_logs = !show_http_logs
        && !args.deployment
        && (args.build
            || (resolved_deployment.default_deployment_status == DeploymentStatus::FAILED
                && deployment_id == resolved_deployment.default_deployment_id));

    if show_http_logs {
        if should_stream {
            stream_http_logs(deployment_id.clone(), http_filter, |log| {
                print_http_log(log, args.json)
            })
            .await?;
        } else {
            fetch_http_logs(
                FetchLogsParams {
                    client: &client,
                    backboard: &backboard,
                    deployment_id: deployment_id.clone(),
                    limit: args.lines.or(Some(500)),
                    filter: http_filter,
                    start_date,
                    end_date,
                },
                |log| print_http_log(log, args.json),
            )
            .await?;
        }
    } else if show_build_logs {
        if should_stream {
            stream_build_logs(deployment_id.clone(), args.filter.clone(), |log| {
                print_log(log, args.json, LogFormat::LevelOnly)
            })
            .await?;
        } else {
            fetch_build_logs(
                FetchLogsParams {
                    client: &client,
                    backboard: &backboard,
                    deployment_id: deployment_id.clone(),
                    limit: args.lines.or(Some(500)),
                    filter: args.filter.clone(),
                    start_date,
                    end_date,
                },
                |log| print_log(log, args.json, LogFormat::LevelOnly),
            )
            .await?;
        }
    } else if should_stream {
        stream_deploy_logs(deployment_id.clone(), args.filter.clone(), |log| {
            print_log(log, args.json, LogFormat::Full)
        })
        .await?;
    } else {
        fetch_deploy_logs(
            FetchLogsParams {
                client: &client,
                backboard: &backboard,
                deployment_id: deployment_id.clone(),
                limit: args.lines.or(Some(500)),
                filter: args.filter.clone(),
                start_date,
                end_date,
            },
            |log| print_log(log, args.json, LogFormat::Full),
        )
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_http_filter_composes_typed_and_raw() {
        // No filters → None
        let args = Args::parse_from(["logs", "--http"]);
        assert_eq!(build_http_filter(&args), None);

        // All typed flags composed in order
        let args = Args::parse_from([
            "logs",
            "--http",
            "--method",
            "POST",
            "--status",
            ">=400",
            "--path",
            "/api/users",
            "--request-id",
            "abc123",
        ]);
        assert_eq!(
            build_http_filter(&args),
            Some("@method:POST @httpStatus:>=400 @path:/api/users @requestId:abc123".to_string())
        );

        // Typed + raw filter combined
        let args = Args::parse_from([
            "logs",
            "--http",
            "--method",
            "GET",
            "--filter",
            "@totalDuration:>=1000",
        ]);
        assert_eq!(
            build_http_filter(&args),
            Some("@method:GET @totalDuration:>=1000".to_string())
        );
    }
}
