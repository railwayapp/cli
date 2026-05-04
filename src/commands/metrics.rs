use crate::{
    controllers::{
        database::DatabaseType,
        db_stats::{self, DatabaseStats},
        environment::get_matched_environment,
        metrics::{
            FetchHttpMetricsParams, FetchProjectMetricsParams, FetchResourceMetricsParams,
            HttpMetricsResult, ResourceMetricsResult, ServiceMetricsSummary, VolumeMetrics,
            compute_sample_rate, fetch_http_metrics, fetch_project_metrics, fetch_resource_metrics,
            find_metric, format_count, format_cpu, format_gb, format_mb, get_volume_metrics,
            is_database_service, pct, utilization,
        },
        project::{ensure_project_and_environment_exist, find_service_instance, get_project},
    },
    util::{progress::create_spinner_if, time::parse_time},
};

use super::{
    queries::{
        self,
        deployments::{DeploymentListInput, DeploymentStatus},
        metrics::MetricMeasurement,
    },
    *,
};

/// View resource and HTTP metrics for a Railway service
#[derive(Parser)]
#[clap(
    alias = "metric",
    after_help = "Examples:

  Quick overview:
  railway metrics                                        # All metrics for linked service (last 1h)
  railway metrics -s my-api -e production                # Specific service and environment
  railway metrics --since 6h                             # Last 6 hours

  Focus on specific metrics:
  railway metrics --cpu --memory                         # Only CPU and memory
  railway metrics --http                                 # Only HTTP metrics
  railway metrics --network                              # Only public network traffic

  HTTP filtering (requires --http):
  railway metrics --http --method POST --path /api/users # POST requests to a specific path
  railway metrics --http --method GET                    # GET requests only

  Raw time-series data (for deeper analysis):
  railway metrics --raw --cpu                            # CPU data points in terminal
  railway metrics --raw --json --cpu                     # CPU time-series as JSON

  All services in the project:
  railway metrics --all                                  # Compact table across all services
  railway metrics --all --json                           # All services as JSON
  railway metrics --all --cpu --memory                   # Table with only CPU and memory

  JSON output (for scripting and agents):
  railway metrics --json                                 # Compact summary as JSON
  railway metrics --json --http --method POST              # HTTP metrics for POST as JSON"
)]
pub struct Args {
    /// Service to view metrics for (defaults to linked service)
    #[clap(short, long, conflicts_with = "all")]
    service: Option<String>,

    /// Show metrics for all services in the project
    #[clap(short = 'a', long, conflicts_with = "raw")]
    all: bool,

    /// Environment to view metrics for (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Time window start. Accepts relative (1h, 6h, 1d, 7d) or ISO 8601. Default: 1h
    #[clap(long, short = 'S', default_value = "1h")]
    since: String,

    /// Time window end. Same formats as --since. Default: now
    #[clap(long, short = 'U', conflicts_with = "watch")]
    until: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,

    /// Show only CPU metrics
    #[clap(long)]
    cpu: bool,

    /// Show only memory metrics
    #[clap(long)]
    memory: bool,

    /// Show only public network traffic (egress/ingress)
    #[clap(long)]
    network: bool,

    /// Show only volume/disk metrics
    #[clap(long)]
    volume: bool,

    /// Show only HTTP metrics
    #[clap(long)]
    http: bool,

    /// Output raw time-series data points instead of summaries
    #[clap(long)]
    raw: bool,

    /// Live dashboard mode — continuously refresh metrics in a TUI
    #[clap(short = 'w', long, conflicts_with_all = ["json", "raw"])]
    watch: bool,

    /// Filter HTTP metrics by request method (requires --http)
    #[clap(long, requires = "http", value_enum, ignore_case = true)]
    method: Option<crate::commands::logs::HttpMethod>,

    /// Filter HTTP metrics by request path (requires --http)
    #[clap(long, requires = "http", value_name = "PATH")]
    path: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Sections {
    pub(crate) cpu: bool,
    pub(crate) memory: bool,
    pub(crate) network: bool,
    pub(crate) volume: bool,
    pub(crate) http: bool,
    /// True when user explicitly set filter flags (vs showing everything by default)
    pub(crate) has_explicit_filter: bool,
}

impl Sections {
    pub(crate) fn from_args(args: &Args) -> Self {
        let any_filter = args.cpu || args.memory || args.network || args.volume || args.http;
        if any_filter {
            Self {
                cpu: args.cpu,
                memory: args.memory,
                network: args.network,
                volume: args.volume,
                http: args.http,
                has_explicit_filter: true,
            }
        } else {
            // Show everything by default
            Self {
                cpu: true,
                memory: true,
                network: true,
                volume: true,
                http: true,
                has_explicit_filter: false,
            }
        }
    }

    pub(crate) fn needs_resource_metrics(&self) -> bool {
        self.cpu || self.memory || self.network || self.volume
    }

    pub(crate) fn measurements(&self, include_disk: bool) -> Vec<MetricMeasurement> {
        let mut m = Vec::new();
        if self.cpu {
            m.push(MetricMeasurement::CPU_USAGE);
            m.push(MetricMeasurement::CPU_LIMIT);
        }
        if self.memory {
            m.push(MetricMeasurement::MEMORY_USAGE_GB);
            m.push(MetricMeasurement::MEMORY_LIMIT_GB);
        }
        if self.network {
            m.push(MetricMeasurement::NETWORK_TX_GB);
            m.push(MetricMeasurement::NETWORK_RX_GB);
        }
        if self.volume && include_disk {
            m.push(MetricMeasurement::DISK_USAGE_GB);
        }
        m
    }
}

fn should_include_db_stats(args: &Args, sections: &Sections, is_db: bool) -> bool {
    is_db && !args.raw && !sections.has_explicit_filter
}

pub async fn command(args: Args) -> Result<()> {
    let start_date = parse_time(&args.since)?;
    let end_date = args.until.as_ref().map(|s| parse_time(s)).transpose()?;

    if let Some(ref e) = end_date {
        if &start_date >= e {
            bail!("--since time must be before --until time");
        }
    }

    let watch_since_label = if args.watch {
        match crate::controllers::metrics_tui::normalize_time_range_label(&args.since) {
            Some(label) => Some(label),
            None => bail!(
                "--watch supports --since values: {}",
                crate::controllers::metrics_tui::supported_time_ranges_label()
            ),
        }
    } else {
        None
    };

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let backboard = configs.get_backboard();
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let environment = match args
        .environment
        .clone()
        .or_else(|| linked_project.environment_name.clone())
        .or_else(|| linked_project.environment.clone())
    {
        Some(environment) => environment,
        None => linked_project.environment_id()?.to_string(),
    };
    let environment = get_matched_environment(&project, environment)?;
    let environment_id = environment.id.clone();
    let environment_name = environment.name.clone();

    // Cross-service mode: --all
    let show_spinner = !args.json && !args.raw && !args.watch;

    if args.all {
        if args.raw {
            bail!("--raw requires a specific service (--service).");
        }

        // Live TUI for all services
        if args.watch {
            let sections = Sections::from_args(&args);
            return crate::controllers::metrics_tui::run_project(
                crate::controllers::metrics_tui::ProjectTuiParams {
                    client: client.clone(),
                    backboard: backboard.clone(),
                    project_id: linked_project.project.clone(),
                    project: project.clone(),
                    environment_id: environment_id.clone(),
                    environment_name: environment_name.clone(),
                    method: args.method.as_ref().map(|m| m.to_string()),
                    path: args.path.clone(),
                    since_label: watch_since_label
                        .clone()
                        .expect("watch time range was validated"),
                    sections,
                },
            )
            .await;
        }

        let spinner = create_spinner_if(show_spinner, "Fetching metrics...".into());
        let sections = Sections::from_args(&args);
        let mut measurements = sections.measurements(false);
        if measurements.is_empty() {
            measurements.push(MetricMeasurement::CPU_USAGE);
        }

        let duration = end_date.unwrap_or_else(chrono::Utc::now) - start_date;
        let sample_rate = compute_sample_rate(duration);
        let window_label = format_window_label(&args.since, args.until.as_deref());

        let mut services = fetch_project_metrics(
            FetchProjectMetricsParams {
                client: &client,
                backboard: &backboard,
                project_id: &linked_project.project,
                environment_id: &environment_id,
                start_date,
                end_date,
                measurements,
                sample_rate_seconds: Some(sample_rate),
            },
            &project,
        )
        .await?;

        // Populate volume data per service
        if sections.volume {
            for svc in &mut services {
                svc.volumes = get_volume_metrics(&project, &environment_id, &svc.service_id);
            }
        }

        // Fetch HTTP metrics per service (skip databases)
        if sections.http {
            let method_filter = args.method.as_ref().map(|m| m.to_string());
            let path_filter = args.path.clone();
            let end = end_date.unwrap_or_else(chrono::Utc::now);

            // Determine which services are databases
            for svc in &mut services {
                let service_instance =
                    find_service_instance(&project, &environment_id, &svc.service_id);
                let source_image = service_instance
                    .and_then(|si| si.source.as_ref())
                    .and_then(|src| src.image.as_deref());
                svc.is_database = is_database_service(source_image);
            }

            // Fetch HTTP metrics for non-database services in parallel
            let http_futures: Vec<_> = services
                .iter()
                .enumerate()
                .filter(|(_, svc)| !svc.is_database)
                .map(|(i, svc)| {
                    let params = FetchHttpMetricsParams {
                        client: &client,
                        backboard: &backboard,
                        service_id: &svc.service_id,
                        environment_id: &environment_id,
                        start_date,
                        end_date: end,
                        step_seconds: Some(sample_rate),
                        method: method_filter.clone(),
                        path: path_filter.clone(),
                        include_time_series: false,
                    };
                    async move { (i, fetch_http_metrics(params).await) }
                })
                .collect();

            let results = futures::future::join_all(http_futures).await;
            for (i, result) in results {
                if let Ok(http) = result {
                    services[i].http = http;
                }
            }
        }

        if let Some(sp) = spinner {
            sp.finish_and_clear();
        }

        let project_name = project.name.clone();
        if args.json {
            print_project_json(
                &project_name,
                &environment_name,
                start_date,
                end_date,
                &sections,
                &services,
            )?;
        } else {
            print_project_terminal(
                &project_name,
                &environment_name,
                &window_label,
                &sections,
                &services,
            );
        }
        return Ok(());
    }

    let services = project.services.edges.iter().collect::<Vec<_>>();
    let (service_id, service_name) = match (args.service.as_deref(), linked_project.service) {
        (Some(service_arg), _) => {
            let s = services
                .iter()
                .find(|s| s.node.name == service_arg || s.node.id == service_arg)
                .with_context(|| format!("Service '{service_arg}' not found"))?;
            (s.node.id.clone(), s.node.name.clone())
        }
        (_, Some(linked_service)) => {
            let name = services
                .iter()
                .find(|s| s.node.id == linked_service)
                .map(|s| s.node.name.clone())
                .unwrap_or_else(|| linked_service.clone());
            (linked_service, name)
        }
        _ => bail!(
            "No service could be found. Please either link one with `railway service` or specify one via the `--service` flag."
        ),
    };

    let sections = Sections::from_args(&args);

    // Detect if service is a database (and which type)
    let service_instance = find_service_instance(&project, &environment_id, &service_id);
    let source_image = service_instance
        .and_then(|si| si.source.as_ref())
        .and_then(|src| src.image.as_deref());
    let is_db = is_database_service(source_image);
    let db_type = detect_database_type(source_image);
    let include_db_stats = should_include_db_stats(&args, &sections, is_db);

    // DB stats go over native SSH. Do a local preflight before we try; a missing
    // SSH key is by far the most common failure mode and we can tell the user
    // exactly how to fix it without waiting on a connection.
    let db_stats_preflight_error = if db_type.is_some() && (args.watch || include_db_stats) {
        db_stats::preflight_db_stats_ssh().err()
    } else {
        None
    };

    // Resolve service instance ID for native SSH (needed for DB stats). Keep
    // it even after a preflight failure so watch mode can retry after the user
    // fixes local SSH setup and presses refresh.
    let service_instance_id = if db_type.is_some() && (args.watch || include_db_stats) {
        service_instance.map(|service_instance| service_instance.id.clone())
    } else {
        None
    };

    // Live TUI for single service
    if args.watch {
        let volumes = if sections.volume {
            get_volume_metrics(&project, &environment_id, &service_id)
        } else {
            vec![]
        };

        return crate::controllers::metrics_tui::run(
            crate::controllers::metrics_tui::ServiceTuiParams {
                client: client.clone(),
                backboard: backboard.clone(),
                service_id: service_id.clone(),
                service_name: service_name.clone(),
                environment_id: environment_id.clone(),
                environment_name: environment_name.clone(),
                since_label: watch_since_label.expect("watch time range was validated"),
                sections,
                is_db,
                db_stats_supported: db_type.is_some(),
                method: args.method.as_ref().map(|m| m.to_string()),
                path: args.path.clone(),
                volumes,
                db_type: db_type.clone(),
                service_instance_id: service_instance_id.clone(),
                db_stats_preflight_error: db_stats_preflight_error.clone(),
            },
        )
        .await;
    }

    // Compute sample rate from time window
    let duration = end_date.unwrap_or_else(chrono::Utc::now) - start_date;
    let sample_rate = compute_sample_rate(duration);

    // Build a human-readable time window label
    let window_label = format_window_label(&args.since, args.until.as_deref());

    let spinner = create_spinner_if(show_spinner, "Fetching metrics...".into());

    // Launch DB stats fetch in parallel with resource metrics (for database services)
    let db_stats_future = if include_db_stats && db_stats_preflight_error.is_none() {
        if let (Some(dt), Some(instance_id)) = (db_type.as_ref(), service_instance_id.as_ref()) {
            let dt = dt.clone();
            let instance_id = instance_id.clone();
            Some(tokio::spawn(async move {
                db_stats::fetch_db_stats(&instance_id, &dt).await
            }))
        } else {
            None
        }
    } else {
        None
    };

    let resource_result = if sections.needs_resource_metrics() {
        let measurements = sections.measurements(true);
        Some(
            fetch_resource_metrics(FetchResourceMetricsParams {
                client: &client,
                backboard: &backboard,
                service_id: &service_id,
                environment_id: &environment_id,
                start_date,
                end_date,
                measurements,
                sample_rate_seconds: Some(sample_rate),
                include_raw: args.raw,
            })
            .await?,
        )
    } else {
        None
    };

    let volume_metrics = if sections.volume {
        get_volume_metrics(&project, &environment_id, &service_id)
    } else {
        vec![]
    };

    // Only fetch deployments when needed (HTTP section or default view)
    let needs_deployments = sections.http || !sections.has_explicit_filter;
    let deployments_data = if needs_deployments {
        fetch_deployments(
            &client,
            &backboard,
            &linked_project.project,
            &environment_id,
            &service_id,
        )
        .await?
    } else {
        DeploymentsData { recent: vec![] }
    };

    // Fetch HTTP metrics using dedicated queries (skip for databases)
    let http_result = if sections.http && !is_db {
        let end = end_date.unwrap_or_else(chrono::Utc::now);
        fetch_http_metrics(FetchHttpMetricsParams {
            client: &client,
            backboard: &backboard,
            service_id: &service_id,
            environment_id: &environment_id,
            start_date,
            end_date: end,
            step_seconds: Some(sample_rate),
            method: args.method.as_ref().map(|m| m.to_string()),
            path: args.path.clone(),
            include_time_series: args.raw,
        })
        .await?
    } else {
        None
    };

    // Collect DB stats result (non-blocking -- failures don't stop resource metrics)
    let db_stats_result = if let Some(handle) = db_stats_future {
        match handle.await {
            Ok(Ok(stats)) => Some(stats),
            Ok(Err(e)) => {
                if show_spinner {
                    let dt = db_type
                        .as_ref()
                        .expect("db_type present when fetch spawned");
                    let msg = db_stats::diagnose_db_stats_failure(&e, dt);
                    eprintln!("{} database stats unavailable:", "warning:".yellow().bold());
                    for line in msg.lines() {
                        eprintln!("  {line}");
                    }
                }
                None
            }
            Err(_) => None, // task panicked
        }
    } else if let Some(ref msg) = db_stats_preflight_error {
        if show_spinner && include_db_stats {
            eprintln!("{} database stats unavailable:", "warning:".yellow().bold());
            for line in msg.lines() {
                eprintln!("  {line}");
            }
        }
        None
    } else {
        None
    };

    if let Some(sp) = spinner {
        sp.finish_and_clear();
    }

    if args.raw {
        if args.json {
            print_raw_json(
                &service_name,
                &environment_name,
                start_date,
                end_date,
                resource_result.as_ref(),
                http_result.as_ref(),
            )?;
        } else {
            print_raw_terminal(resource_result.as_ref(), http_result.as_ref());
        }
    } else if args.json {
        print_json(
            &service_name,
            &environment_name,
            start_date,
            end_date,
            &sections,
            resource_result.as_ref(),
            &volume_metrics,
            http_result.as_ref(),
            &deployments_data,
            db_stats_result.as_ref(),
        )?;
    } else {
        let has_http_filter = args.method.is_some() || args.path.is_some();
        print_terminal(
            &service_name,
            &environment_name,
            &window_label,
            &sections,
            resource_result.as_ref(),
            &volume_metrics,
            http_result.as_ref(),
            &deployments_data,
            is_db,
            has_http_filter,
        );
        if let Some(ref db_stats) = db_stats_result {
            print_db_stats_terminal(db_stats);
        }
    }

    Ok(())
}

struct DeploymentInfo {
    id: String,
    created_at: chrono::DateTime<chrono::Utc>,
    status: DeploymentStatus,
}

struct DeploymentsData {
    recent: Vec<DeploymentInfo>,
}

async fn fetch_deployments(
    client: &reqwest::Client,
    backboard: &str,
    project_id: &str,
    environment_id: &str,
    service_id: &str,
) -> Result<DeploymentsData> {
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
    let mut all: Vec<_> = deployments.edges.into_iter().map(|d| d.node).collect();
    all.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let recent = all
        .iter()
        .take(5)
        .map(|d| DeploymentInfo {
            id: d.id.clone(),
            created_at: d.created_at,
            status: d.status.clone(),
        })
        .collect();

    Ok(DeploymentsData { recent })
}

fn is_relative_time_value(value: &str) -> bool {
    let value_lower = value.trim().to_lowercase();
    value_lower.ends_with('h')
        || value_lower.ends_with('m')
        || value_lower.ends_with('d')
        || value_lower.ends_with('w')
        || value_lower.ends_with('s')
}

fn format_time_bound_label(value: &str, suffix_for_relative: Option<&str>) -> String {
    let value_lower = value.to_lowercase().trim().to_string();
    if is_relative_time_value(value) {
        match suffix_for_relative {
            Some(suffix) => format!("{value_lower} {suffix}"),
            None => value_lower,
        }
    } else {
        value.to_string()
    }
}

fn format_window_label(since: &str, until: Option<&str>) -> String {
    match until {
        Some(until) => format!(
            "from {} until {}",
            format_time_bound_label(since, Some("ago")),
            format_time_bound_label(until, Some("ago"))
        ),
        None if is_relative_time_value(since) => {
            format!("last {}", format_time_bound_label(since, None))
        }
        None => format!("since {since}"),
    }
}

/// Pad a string to `width` using its visible length (ignoring ANSI codes).
fn pad_visible(visible: &str, display: &str, width: usize) -> String {
    let pad = width.saturating_sub(visible.len());
    format!("{}{}", display, " ".repeat(pad))
}

/// Print a resource metric line in columnar format with optional utilization coloring.
fn print_resource_line(
    label: &str,
    usage: &crate::controllers::metrics::MetricSummary,
    limit: Option<&crate::controllers::metrics::MetricSummary>,
    format_fn: fn(f64) -> String,
) {
    let limit_val = limit.filter(|l| l.current > 0.0).map(|l| l.current);
    let util_pct = utilization(usage.current, limit_val);

    let current_plain = format_fn(usage.current);
    let current_display = match util_pct {
        Some(pct) => {
            let plain = format!("{} ({:.0}%)", current_plain, pct);
            let colored = format!("{} ({:.0}%)", color_by_health(&current_plain, pct), pct);
            pad_visible(&plain, &colored, 18)
        }
        None => format!("{:<18}", current_plain),
    };

    let limit_str = limit_val.map(format_fn).unwrap_or_else(|| "–".to_string());

    println!(
        "  {:<9} {} {:<15} {:<15} {}",
        label,
        current_display,
        format_fn(usage.average),
        format_fn(usage.max),
        limit_str,
    );
}

#[allow(clippy::too_many_arguments)]
fn print_terminal(
    service_name: &str,
    environment_name: &str,
    window_label: &str,
    sections: &Sections,
    resource: Option<&ResourceMetricsResult>,
    volumes: &[VolumeMetrics],
    http: Option<&HttpMetricsResult>,
    deployments: &DeploymentsData,
    is_db: bool,
    has_http_filter: bool,
) {
    println!(
        "\nMetrics for {} in {} ({})\n",
        service_name.bold(),
        environment_name.blue().bold(),
        window_label
    );

    let mut printed_any = false;

    if let Some(res) = resource {
        // Resource Usage section (CPU + Memory)
        let cpu_usage = sections
            .cpu
            .then(|| find_metric(&res.metrics, "CPU_USAGE"))
            .flatten();
        let cpu_limit = sections
            .cpu
            .then(|| find_metric(&res.metrics, "CPU_LIMIT"))
            .flatten();
        let mem_usage = sections
            .memory
            .then(|| find_metric(&res.metrics, "MEMORY_USAGE_GB"))
            .flatten();
        let mem_limit = sections
            .memory
            .then(|| find_metric(&res.metrics, "MEMORY_LIMIT_GB"))
            .flatten();

        let has_cpu = cpu_usage.is_some();
        let has_mem = mem_usage.is_some();

        if has_cpu || has_mem {
            println!("  {}", "Resource Usage".bold());
            println!("  ─────────────────────────────────────────────────────────────────────");
            println!(
                "  {}",
                "           current            avg             max             limit".dimmed()
            );
            if let Some(cpu) = cpu_usage {
                print_resource_line("CPU", cpu, cpu_limit, format_cpu);
            }
            if let Some(mem) = mem_usage {
                print_resource_line("Memory", mem, mem_limit, format_gb);
            }
            println!();
            printed_any = true;
        }

        // Network section
        if sections.network {
            let tx = find_metric(&res.metrics, "NETWORK_TX_GB");
            let rx = find_metric(&res.metrics, "NETWORK_RX_GB");
            let has_network = tx.is_some() || rx.is_some();
            if has_network {
                println!("  {}", "Public Network Traffic".bold());
                println!("  ──────────────────────────────────────────────");
                if let Some(tx) = tx {
                    println!(
                        "  {:<13}{:<24}avg: {:<17}max: {}",
                        "Egress",
                        format_gb(tx.current),
                        format_gb(tx.average),
                        format_gb(tx.max),
                    );
                }
                if let Some(rx) = rx {
                    println!(
                        "  {:<13}{:<24}avg: {:<17}max: {}",
                        "Ingress",
                        format_gb(rx.current),
                        format_gb(rx.average),
                        format_gb(rx.max),
                    );
                }
                println!();
                printed_any = true;
            }
        }

        // Volume section
        if sections.volume {
            let disk = find_metric(&res.metrics, "DISK_USAGE_GB");
            let use_service_disk_per_volume = disk.is_some() && volumes.len() <= 1;
            for vol in volumes {
                println!("  {}", format!("Volume: {}", vol.mount_path).bold());
                println!("  ──────────────────────────────────────────────");

                if use_service_disk_per_volume {
                    let disk = disk.expect("checked above");
                    let limit_val = if vol.limit_size_mb > 0.0 {
                        Some(vol.limit_size_mb / 1024.0)
                    } else {
                        None
                    };
                    let util_pct = utilization(disk.current, limit_val);
                    let current_str = format_gb(disk.current);
                    let current_display = match util_pct {
                        Some(p) => {
                            format!("{} ({:.0}%)", color_by_health(&current_str, p), p)
                        }
                        None => current_str,
                    };
                    let limit_str = if vol.limit_size_mb > 0.0 {
                        format!("   (limit: {})", format_mb(vol.limit_size_mb))
                    } else {
                        String::new()
                    };
                    println!(
                        "  {:<13}{:<24}avg: {:<17}max: {}{}",
                        "Disk",
                        current_display,
                        format_gb(disk.average),
                        format_gb(disk.max),
                        limit_str,
                    );
                } else {
                    let util = utilization(
                        vol.current_size_mb,
                        Some(vol.limit_size_mb).filter(|&l| l > 0.0),
                    );
                    let pct_str = util.map(|p| format!(" ({:.0}%)", p)).unwrap_or_default();
                    println!(
                        "  {:<13}{} / {}{}",
                        "Usage",
                        format_mb(vol.current_size_mb),
                        format_mb(vol.limit_size_mb),
                        pct_str,
                    );
                }
                println!();
                printed_any = true;
            }

            if volumes.len() > 1 {
                if let Some(disk) = disk {
                    println!("  {}", "Disk Usage (service total)".bold());
                    println!("  ──────────────────────────────────────────────");
                    println!(
                        "  {:<13}{:<24}avg: {:<17}max: {}",
                        "Current",
                        format_gb(disk.current),
                        format_gb(disk.average),
                        format_gb(disk.max),
                    );
                    println!();
                    printed_any = true;
                }
            }

            if volumes.is_empty() {
                if let Some(disk) = disk {
                    println!("  {}", "Disk Usage".bold());
                    println!("  ──────────────────────────────────────────────");
                    println!(
                        "  {:<13}{:<24}avg: {:<17}max: {}",
                        "Current",
                        format_gb(disk.current),
                        format_gb(disk.average),
                        format_gb(disk.max),
                    );
                    println!();
                    printed_any = true;
                } else if sections.has_explicit_filter {
                    println!("  No volumes attached to this service.\n");
                    printed_any = true;
                }
            }
        }
    } else if sections.volume && !volumes.is_empty() {
        for vol in volumes {
            println!("  {}", format!("Volume: {}", vol.mount_path).bold());
            println!("  ──────────────────────────────────────────────");
            let util = utilization(
                vol.current_size_mb,
                Some(vol.limit_size_mb).filter(|&l| l > 0.0),
            );
            let pct_str = util.map(|p| format!(" ({:.0}%)", p)).unwrap_or_default();
            println!(
                "  {:<13}{} / {}{}",
                "Usage",
                format_mb(vol.current_size_mb),
                format_mb(vol.limit_size_mb),
                pct_str,
            );
            println!();
            printed_any = true;
        }
    } else if sections.volume && sections.has_explicit_filter {
        println!("  No volumes attached to this service.\n");
        printed_any = true;
    }

    // HTTP section
    if sections.http {
        if is_db {
            if sections.has_explicit_filter {
                println!("  HTTP metrics are not available for database services.\n");
                printed_any = true;
            }
        } else if let Some(http) = http {
            let header = format!("HTTP Requests ({} total)", format_count(http.total));
            println!("  {}", header.bold());
            println!("  ──────────────────────────────────────────────");
            println!("  {:<13}{}", "Total", format_count(http.total));
            println!(
                "  {:<13}{} ({:.1}%)",
                "2xx".green(),
                format_count(http.status_counts[2]),
                pct(http.status_counts[2], http.total),
            );
            println!(
                "  {:<13}{} ({:.1}%)",
                "3xx".cyan(),
                format_count(http.status_counts[3]),
                pct(http.status_counts[3], http.total),
            );
            println!(
                "  {:<13}{} ({:.1}%)",
                "4xx".yellow(),
                format_count(http.status_counts[4]),
                pct(http.status_counts[4], http.total),
            );
            println!(
                "  {:<13}{} ({:.1}%)",
                "5xx".red(),
                format_count(http.status_counts[5]),
                pct(http.status_counts[5], http.total),
            );

            let error_rate_str = format!("{:.1}%", http.error_rate);
            let error_rate_display = color_by_health(&error_rate_str, http.error_rate * 10.0);
            println!("  {:<13}{}", "Error Rate", error_rate_display);
            println!();
            println!(
                "  {:<13}p50: {}ms   p90: {}ms   p95: {}ms   p99: {}ms",
                "Latency", http.p50_ms, http.p90_ms, http.p95_ms, http.p99_ms,
            );
            println!();
            printed_any = true;
        } else if sections.has_explicit_filter {
            if has_http_filter {
                println!("  No HTTP logs matched the filter.\n");
            } else {
                println!("  No HTTP logs found.\n");
            }
            printed_any = true;
        }
    }

    // Recent deployments: show when no explicit filters are set, or when HTTP data was displayed
    let show_deploys = !sections.has_explicit_filter || http.is_some();
    if show_deploys && !deployments.recent.is_empty() {
        println!("  {}", "Recent Deployments".bold());
        println!("  ──────────────────────────────────────────────");
        for d in &deployments.recent {
            let line = format!(
                "  {}   {}   {:?}",
                d.created_at.format("%Y-%m-%d %H:%M UTC"),
                &d.id[..8.min(d.id.len())],
                d.status,
            );
            if matches!(d.status, DeploymentStatus::SUCCESS) {
                println!("{}", line.green().bold());
            } else if matches!(d.status, DeploymentStatus::REMOVED) {
                println!("{}", line.dimmed());
            } else {
                println!("{}", line);
            }
        }
        println!();
    }

    // If nothing was printed at all, show a helpful message
    if !printed_any {
        println!(
            "  No metrics data available yet. Metrics typically appear\n  within a few minutes of deployment.\n"
        );
    }
}

/// Color a string based on utilization percentage: green (<60%), yellow (60-85%), red (>85%)
fn color_by_health(s: &str, utilization_pct: f64) -> colored::ColoredString {
    if utilization_pct >= 85.0 {
        s.red()
    } else if utilization_pct >= 60.0 {
        s.yellow()
    } else {
        s.green()
    }
}

#[allow(clippy::too_many_arguments)]
fn print_json(
    service_name: &str,
    environment_name: &str,
    start_date: chrono::DateTime<chrono::Utc>,
    end_date: Option<chrono::DateTime<chrono::Utc>>,
    sections: &Sections,
    resource: Option<&ResourceMetricsResult>,
    volumes: &[VolumeMetrics],
    http: Option<&HttpMetricsResult>,
    deployments: &DeploymentsData,
    db_stats: Option<&DatabaseStats>,
) -> Result<()> {
    let mut json = serde_json::Map::new();
    json.insert(
        "service".to_string(),
        serde_json::Value::String(service_name.to_string()),
    );
    json.insert(
        "environment".to_string(),
        serde_json::Value::String(environment_name.to_string()),
    );

    let mut window = serde_json::Map::new();
    window.insert(
        "since".to_string(),
        serde_json::Value::String(start_date.to_rfc3339()),
    );
    window.insert(
        "until".to_string(),
        serde_json::Value::String(end_date.unwrap_or_else(chrono::Utc::now).to_rfc3339()),
    );
    json.insert("window".to_string(), serde_json::Value::Object(window));

    if let Some(res) = resource {
        if sections.cpu {
            let cpu_usage = find_metric(&res.metrics, "CPU_USAGE");
            let cpu_limit = find_metric(&res.metrics, "CPU_LIMIT");
            if let Some(cpu) = cpu_usage {
                let mut cpu_json = serde_json::Map::new();
                cpu_json.insert("current".into(), serde_json::json!(cpu.current));
                cpu_json.insert("average".into(), serde_json::json!(cpu.average));
                cpu_json.insert("max".into(), serde_json::json!(cpu.max));
                if let Some(limit) = cpu_limit.filter(|l| l.current > 0.0) {
                    cpu_json.insert("limit".into(), serde_json::json!(limit.current));
                    if let Some(pct) = utilization(cpu.current, Some(limit.current)) {
                        cpu_json.insert(
                            "utilization_pct".into(),
                            serde_json::json!((pct * 10.0).round() / 10.0),
                        );
                    }
                }
                cpu_json.insert("unit".into(), serde_json::json!("vCPU"));
                json.insert("cpu".into(), serde_json::Value::Object(cpu_json));
            }
        }

        if sections.memory {
            let mem_usage = find_metric(&res.metrics, "MEMORY_USAGE_GB");
            let mem_limit = find_metric(&res.metrics, "MEMORY_LIMIT_GB");
            if let Some(mem) = mem_usage {
                let mut mem_json = serde_json::Map::new();
                mem_json.insert("current_mb".into(), serde_json::json!(mem.current * 1024.0));
                mem_json.insert("average_mb".into(), serde_json::json!(mem.average * 1024.0));
                mem_json.insert("max_mb".into(), serde_json::json!(mem.max * 1024.0));
                if let Some(limit) = mem_limit.filter(|l| l.current > 0.0) {
                    mem_json.insert("limit_mb".into(), serde_json::json!(limit.current * 1024.0));
                    if let Some(pct) = utilization(mem.current, Some(limit.current)) {
                        mem_json.insert(
                            "utilization_pct".into(),
                            serde_json::json!((pct * 10.0).round() / 10.0),
                        );
                    }
                }
                json.insert("memory".into(), serde_json::Value::Object(mem_json));
            }
        }

        if sections.network {
            let tx = find_metric(&res.metrics, "NETWORK_TX_GB");
            let rx = find_metric(&res.metrics, "NETWORK_RX_GB");
            if tx.is_some() || rx.is_some() {
                let mut net_json = serde_json::Map::new();
                if let Some(tx) = tx {
                    let mut egress = serde_json::Map::new();
                    egress.insert("current_mb".into(), serde_json::json!(tx.current * 1024.0));
                    egress.insert("average_mb".into(), serde_json::json!(tx.average * 1024.0));
                    egress.insert("max_mb".into(), serde_json::json!(tx.max * 1024.0));
                    net_json.insert("egress".into(), serde_json::Value::Object(egress));
                }
                if let Some(rx) = rx {
                    let mut ingress = serde_json::Map::new();
                    ingress.insert("current_mb".into(), serde_json::json!(rx.current * 1024.0));
                    ingress.insert("average_mb".into(), serde_json::json!(rx.average * 1024.0));
                    ingress.insert("max_mb".into(), serde_json::json!(rx.max * 1024.0));
                    net_json.insert("ingress".into(), serde_json::Value::Object(ingress));
                }
                json.insert(
                    "public_network_traffic".into(),
                    serde_json::Value::Object(net_json),
                );
            }
        }

        if sections.volume {
            let disk = find_metric(&res.metrics, "DISK_USAGE_GB");
            let use_service_disk_per_volume = disk.is_some() && volumes.len() <= 1;
            let vol_arr: Vec<serde_json::Value> = volumes
                .iter()
                .map(|v| {
                    let mut vol_json = serde_json::json!({
                        "name": v.volume_name,
                        "mount_path": v.mount_path,
                        "current_mb": v.current_size_mb,
                        "limit_mb": v.limit_size_mb,
                    });
                    // Only merge service-level disk usage into a volume when there is a
                    // single attached volume. Multi-volume services need to keep the
                    // per-volume snapshots separate from the aggregated disk metric.
                    if use_service_disk_per_volume {
                        let disk = disk.expect("checked above");
                        vol_json["current_mb"] = serde_json::json!(disk.current * 1024.0);
                        vol_json["average_mb"] = serde_json::json!(disk.average * 1024.0);
                        vol_json["max_mb"] = serde_json::json!(disk.max * 1024.0);
                    }
                    vol_json
                })
                .collect();
            if !vol_arr.is_empty() {
                json.insert("volumes".into(), serde_json::Value::Array(vol_arr));
            }

            if let Some(disk) = disk {
                if volumes.is_empty() || volumes.len() > 1 {
                    // No volumes or multiple volumes: expose the service-level disk metric
                    // separately so it is not mistaken for a per-volume series.
                    let mut disk_json = serde_json::Map::new();
                    disk_json.insert(
                        "current_mb".into(),
                        serde_json::json!(disk.current * 1024.0),
                    );
                    disk_json.insert(
                        "average_mb".into(),
                        serde_json::json!(disk.average * 1024.0),
                    );
                    disk_json.insert("max_mb".into(), serde_json::json!(disk.max * 1024.0));
                    json.insert("disk".into(), serde_json::Value::Object(disk_json));
                }
            }
        }
    }

    if let Some(http) = http {
        let mut http_json = serde_json::Map::new();
        http_json.insert("total".into(), serde_json::json!(http.total));
        http_json.insert("2xx".into(), serde_json::json!(http.status_counts[2]));
        http_json.insert("3xx".into(), serde_json::json!(http.status_counts[3]));
        http_json.insert("4xx".into(), serde_json::json!(http.status_counts[4]));
        http_json.insert("5xx".into(), serde_json::json!(http.status_counts[5]));
        http_json.insert("error_rate".into(), serde_json::json!(http.error_rate));
        http_json.insert("p50_ms".into(), serde_json::json!(http.p50_ms));
        http_json.insert("p90_ms".into(), serde_json::json!(http.p90_ms));
        http_json.insert("p95_ms".into(), serde_json::json!(http.p95_ms));
        http_json.insert("p99_ms".into(), serde_json::json!(http.p99_ms));
        json.insert("http".into(), serde_json::Value::Object(http_json));
    }

    if !deployments.recent.is_empty() {
        let deploys: Vec<serde_json::Value> = deployments
            .recent
            .iter()
            .map(|d| {
                serde_json::json!({
                    "id": d.id,
                    "created_at": d.created_at.to_rfc3339(),
                    "status": format!("{:?}", d.status),
                })
            })
            .collect();
        json.insert("deployments".into(), serde_json::Value::Array(deploys));
    }

    if let Some(stats) = db_stats {
        json.insert("db_stats".into(), serde_json::to_value(stats)?);
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::Value::Object(json))?
    );

    Ok(())
}

fn print_raw_json(
    service_name: &str,
    environment_name: &str,
    start_date: chrono::DateTime<chrono::Utc>,
    end_date: Option<chrono::DateTime<chrono::Utc>>,
    resource: Option<&ResourceMetricsResult>,
    http: Option<&HttpMetricsResult>,
) -> Result<()> {
    fn points_to_json(
        points: &[crate::controllers::metrics::MetricDataPoint],
    ) -> Vec<serde_json::Value> {
        points
            .iter()
            .map(|p| {
                let ts = chrono::DateTime::from_timestamp(p.ts, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_else(|| p.ts.to_string());
                serde_json::json!({"ts": ts, "value": p.value})
            })
            .collect()
    }

    let mut json = serde_json::Map::new();
    json.insert(
        "service".into(),
        serde_json::Value::String(service_name.to_string()),
    );
    json.insert(
        "environment".into(),
        serde_json::Value::String(environment_name.to_string()),
    );

    let mut window = serde_json::Map::new();
    window.insert("since".into(), serde_json::json!(start_date.to_rfc3339()));
    window.insert(
        "until".into(),
        serde_json::json!(end_date.unwrap_or_else(chrono::Utc::now).to_rfc3339()),
    );
    json.insert("window".into(), serde_json::Value::Object(window));

    let mut measurements = serde_json::Map::new();
    if let Some(res) = resource {
        for metric in &res.metrics {
            let points = points_to_json(&metric.raw_values);
            measurements.insert(metric.measurement.clone(), serde_json::Value::Array(points));
        }
    }
    json.insert(
        "measurements".into(),
        serde_json::Value::Object(measurements),
    );

    if let Some(http) = http.and_then(|result| result.time_series.as_ref()) {
        let mut http_json = serde_json::Map::new();
        let error_rate_pct: Vec<_> = http
            .error_rate_ts
            .iter()
            .zip(http.request_rate_ts.iter())
            .map(
                |(errors, total)| crate::controllers::metrics::MetricDataPoint {
                    ts: errors.ts,
                    value: if total.value > 0.0 {
                        (errors.value / total.value) * 100.0
                    } else {
                        0.0
                    },
                },
            )
            .collect();

        http_json.insert(
            "requests".into(),
            serde_json::Value::Array(points_to_json(&http.request_rate_ts)),
        );
        http_json.insert(
            "errors_5xx".into(),
            serde_json::Value::Array(points_to_json(&http.error_rate_ts)),
        );
        http_json.insert(
            "error_rate_pct".into(),
            serde_json::Value::Array(points_to_json(&error_rate_pct)),
        );
        http_json.insert(
            "status_2xx".into(),
            serde_json::Value::Array(points_to_json(&http.status_2xx_ts)),
        );
        http_json.insert(
            "status_3xx".into(),
            serde_json::Value::Array(points_to_json(&http.status_3xx_ts)),
        );
        http_json.insert(
            "status_4xx".into(),
            serde_json::Value::Array(points_to_json(&http.status_4xx_ts)),
        );
        http_json.insert(
            "status_5xx".into(),
            serde_json::Value::Array(points_to_json(&http.status_5xx_ts)),
        );
        http_json.insert(
            "p50_ms".into(),
            serde_json::Value::Array(points_to_json(&http.p50_ts)),
        );
        http_json.insert(
            "p90_ms".into(),
            serde_json::Value::Array(points_to_json(&http.p90_ts)),
        );
        http_json.insert(
            "p95_ms".into(),
            serde_json::Value::Array(points_to_json(&http.p95_ts)),
        );
        http_json.insert(
            "p99_ms".into(),
            serde_json::Value::Array(points_to_json(&http.p99_ts)),
        );

        json.insert("http".into(), serde_json::Value::Object(http_json));
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::Value::Object(json))?
    );
    Ok(())
}

fn print_project_terminal(
    project_name: &str,
    environment_name: &str,
    window_label: &str,
    sections: &Sections,
    services: &[ServiceMetricsSummary],
) {
    println!(
        "\nMetrics for {} in {} ({})\n",
        project_name.bold(),
        environment_name.blue().bold(),
        window_label
    );

    if services.is_empty() {
        println!("  No metrics data available.");
        return;
    }

    // Build header
    let mut header = format!("  {:<25}", "Service");
    if sections.cpu {
        header.push_str(&format!("{:<28}", "CPU"));
    }
    if sections.memory {
        header.push_str(&format!("{:<28}", "Memory"));
    }
    if sections.network {
        header.push_str(&format!("{:<14}{:<14}", "Egress", "Ingress"));
    }
    if sections.volume {
        header.push_str(&format!("{:<22}", "Volume"));
    }
    if sections.http {
        header.push_str(&format!("{:<12}{:<12}", "Reqs", "Err Rate"));
    }
    println!("{}", header.bold());
    println!("  {}", "─".repeat(header.len().saturating_sub(2)));

    for svc in services {
        let name = truncate(&svc.service_name, 23);
        print!("  {:<25}", name);

        if sections.cpu {
            let (val, pct) = match (&svc.cpu, &svc.cpu_limit) {
                (Some(cpu), Some(limit)) if limit.current > 0.0 => {
                    let p = (cpu.current / limit.current) * 100.0;
                    (
                        format!(
                            "{} / {}",
                            format_cpu(cpu.current),
                            format_cpu(limit.current)
                        ),
                        Some(p),
                    )
                }
                (Some(cpu), _) => (format_cpu(cpu.current), None),
                _ => ("—".to_string(), None),
            };
            // Print colored value then pad with spaces (avoids ANSI in padding)
            let display = match pct {
                Some(p) => format!("{}", color_by_health(&val, p)),
                None => val.clone(),
            };
            let padding = 28usize.saturating_sub(val.len());
            print!("{display}{:padding$}", "");
        }

        if sections.memory {
            let (val, pct) = match (&svc.memory, &svc.memory_limit) {
                (Some(mem), Some(limit)) if limit.current > 0.0 => {
                    let p = (mem.current / limit.current) * 100.0;
                    (
                        format!("{} / {}", format_gb(mem.current), format_gb(limit.current)),
                        Some(p),
                    )
                }
                (Some(mem), _) => (format_gb(mem.current), None),
                _ => ("—".to_string(), None),
            };
            let display = match pct {
                Some(p) => format!("{}", color_by_health(&val, p)),
                None => val.clone(),
            };
            let padding = 28usize.saturating_sub(val.len());
            print!("{display}{:padding$}", "");
        }

        if sections.network {
            let tx_str = svc
                .network_tx
                .as_ref()
                .filter(|t| t.current > 0.0001)
                .map(|t| format_gb(t.current))
                .unwrap_or_else(|| "—".to_string());
            let rx_str = svc
                .network_rx
                .as_ref()
                .filter(|r| r.current > 0.0001)
                .map(|r| format_gb(r.current))
                .unwrap_or_else(|| "—".to_string());
            print!("{:<14}{:<14}", tx_str, rx_str);
        }

        if sections.volume {
            if svc.volumes.is_empty() {
                print!("{:<22}", "—");
            } else {
                let vol = &svc.volumes[0];
                if vol.limit_size_mb > 0.0 {
                    let pct = (vol.current_size_mb / vol.limit_size_mb) * 100.0;
                    let val = format!(
                        "{} / {}",
                        format_mb(vol.current_size_mb),
                        format_mb(vol.limit_size_mb)
                    );
                    let display = format!("{}", color_by_health(&val, pct));
                    let padding = 22usize.saturating_sub(val.len());
                    print!("{display}{:padding$}", "");
                } else {
                    print!("{:<22}", format_mb(vol.current_size_mb));
                }
            }
        }

        if sections.http {
            if svc.is_database {
                print!("{:<12}{:<12}", "—", "—");
            } else if let Some(ref http) = svc.http {
                let reqs = format_count(http.total);
                let err_val = format!("{:.1}%", http.error_rate);
                let err_display = format!("{}", color_by_health(&err_val, http.error_rate * 10.0));
                let err_padding = 12usize.saturating_sub(err_val.len());
                print!("{:<12}{err_display}{:err_padding$}", reqs, "");
            } else {
                print!("{:<12}{:<12}", "—", "—");
            }
        }

        println!();
    }
    println!();
}

fn print_project_json(
    project_name: &str,
    environment_name: &str,
    start_date: chrono::DateTime<chrono::Utc>,
    end_date: Option<chrono::DateTime<chrono::Utc>>,
    sections: &Sections,
    services: &[ServiceMetricsSummary],
) -> Result<()> {
    let svc_arr: Vec<serde_json::Value> = services
        .iter()
        .map(|svc| {
            let mut obj = serde_json::json!({
                "name": svc.service_name,
                "id": svc.service_id,
            });

            if let Some(cpu) = sections.cpu.then_some(svc.cpu.as_ref()).flatten() {
                let mut cpu_json = serde_json::json!({
                    "current": cpu.current,
                    "unit": "vCPU",
                });
                if let Some(ref limit) = svc.cpu_limit {
                    if limit.current > 0.0 {
                        cpu_json["limit"] = serde_json::json!(limit.current);
                        if let Some(pct) = utilization(cpu.current, Some(limit.current)) {
                            cpu_json["utilization_pct"] =
                                serde_json::json!((pct * 10.0).round() / 10.0);
                        }
                    }
                }
                obj["cpu"] = cpu_json;
            }

            if let Some(mem) = sections.memory.then_some(svc.memory.as_ref()).flatten() {
                let mut mem_json = serde_json::json!({
                    "current_mb": mem.current * 1024.0,
                });
                if let Some(ref limit) = svc.memory_limit {
                    if limit.current > 0.0 {
                        mem_json["limit_mb"] = serde_json::json!(limit.current * 1024.0);
                        if let Some(pct) = utilization(mem.current, Some(limit.current)) {
                            mem_json["utilization_pct"] =
                                serde_json::json!((pct * 10.0).round() / 10.0);
                        }
                    }
                }
                obj["memory"] = mem_json;
            }

            if sections.network && (svc.network_tx.is_some() || svc.network_rx.is_some()) {
                let mut net = serde_json::Map::new();
                if let Some(ref tx) = svc.network_tx {
                    net.insert("egress_mb".into(), serde_json::json!(tx.current * 1024.0));
                }
                if let Some(ref rx) = svc.network_rx {
                    net.insert("ingress_mb".into(), serde_json::json!(rx.current * 1024.0));
                }
                obj["network"] = serde_json::Value::Object(net);
            }

            if sections.volume && !svc.volumes.is_empty() {
                let vol_arr: Vec<serde_json::Value> = svc
                    .volumes
                    .iter()
                    .map(|v| {
                        serde_json::json!({
                            "name": v.volume_name,
                            "mount_path": v.mount_path,
                            "current_mb": v.current_size_mb,
                            "limit_mb": v.limit_size_mb,
                        })
                    })
                    .collect();
                obj["volumes"] = serde_json::Value::Array(vol_arr);
            }

            if let Some(http) = sections.http.then_some(svc.http.as_ref()).flatten() {
                obj["http"] = serde_json::json!({
                    "total": http.total,
                    "2xx": http.status_counts[2],
                    "3xx": http.status_counts[3],
                    "4xx": http.status_counts[4],
                    "5xx": http.status_counts[5],
                    "error_rate": http.error_rate,
                    "p50_ms": http.p50_ms,
                    "p90_ms": http.p90_ms,
                    "p95_ms": http.p95_ms,
                    "p99_ms": http.p99_ms,
                });
            }

            obj
        })
        .collect();

    let json = serde_json::json!({
        "project": project_name,
        "environment": environment_name,
        "window": {
            "since": start_date.to_rfc3339(),
            "until": end_date.unwrap_or_else(chrono::Utc::now).to_rfc3339(),
        },
        "services": svc_arr,
    });

    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

fn print_raw_terminal(resource: Option<&ResourceMetricsResult>, http: Option<&HttpMetricsResult>) {
    fn print_raw_points(
        name: &str,
        points: &[crate::controllers::metrics::MetricDataPoint],
        printed: &mut bool,
    ) {
        for point in points {
            let ts = chrono::DateTime::from_timestamp(point.ts, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| point.ts.to_string());
            println!("{}  {:<20}  {:.6}", ts, name, point.value);
            *printed = true;
        }
    }

    let mut printed = false;
    if let Some(res) = resource {
        for metric in &res.metrics {
            print_raw_points(&metric.measurement, &metric.raw_values, &mut printed);
        }
    }
    if let Some(http) = http.and_then(|result| result.time_series.as_ref()) {
        print_raw_points("HTTP_REQUESTS", &http.request_rate_ts, &mut printed);
        print_raw_points("HTTP_5XX", &http.error_rate_ts, &mut printed);
        print_raw_points("HTTP_2XX", &http.status_2xx_ts, &mut printed);
        print_raw_points("HTTP_3XX", &http.status_3xx_ts, &mut printed);
        print_raw_points("HTTP_4XX", &http.status_4xx_ts, &mut printed);
        print_raw_points("HTTP_5XX_BUCKET", &http.status_5xx_ts, &mut printed);
        print_raw_points("HTTP_P50_MS", &http.p50_ts, &mut printed);
        print_raw_points("HTTP_P90_MS", &http.p90_ts, &mut printed);
        print_raw_points("HTTP_P95_MS", &http.p95_ts, &mut printed);
        print_raw_points("HTTP_P99_MS", &http.p99_ts, &mut printed);

        for (errors, total) in http.error_rate_ts.iter().zip(http.request_rate_ts.iter()) {
            let pct = if total.value > 0.0 {
                (errors.value / total.value) * 100.0
            } else {
                0.0
            };
            print_raw_points(
                "HTTP_ERROR_RATE_PCT",
                &[crate::controllers::metrics::MetricDataPoint {
                    ts: errors.ts,
                    value: pct,
                }],
                &mut printed,
            );
        }
    }
    if !printed {
        println!("No data points available.");
    }
}

fn print_db_stats_terminal(stats: &DatabaseStats) {
    use crate::controllers::db_stats::types::*;

    fn health_color(value: f64, warn: f64, crit: f64, inverted: bool) -> colored::ColoredString {
        let s = format!("{:.1}%", value);
        if inverted {
            // Lower is worse (cache hit ratio)
            if value < crit {
                s.red()
            } else if value < warn {
                s.yellow()
            } else {
                s.green()
            }
        } else {
            // Higher is worse (utilization)
            if value > crit {
                s.red()
            } else if value > warn {
                s.yellow()
            } else {
                s.green()
            }
        }
    }

    fn fmt_bytes(bytes: i64) -> String {
        if bytes >= 1_073_741_824 {
            format!("{:.2} GB", bytes as f64 / 1_073_741_824.0)
        } else if bytes >= 1_048_576 {
            format!("{:.1} MB", bytes as f64 / 1_048_576.0)
        } else if bytes >= 1024 {
            format!("{:.1} KB", bytes as f64 / 1024.0)
        } else {
            format!("{} B", bytes)
        }
    }

    println!();
    println!("  {}", "Database Stats".bold());
    println!("  ─────────────────────────────────────────────────────────────────────");

    match stats {
        DatabaseStats::PostgreSQL(pg) => {
            // Connections
            let util = if pg.connections.max_connections > 0 {
                pg.connections.total as f64 / pg.connections.max_connections as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "  {}  Active: {}  Idle: {}  Idle(txn): {}  Total: {} / {}  ({})",
                "Connections".bold(),
                format!("{}", pg.connections.active).cyan(),
                pg.connections.idle,
                pg.connections.idle_in_transaction,
                pg.connections.total,
                pg.connections.max_connections,
                health_color(util, 60.0, 80.0, false),
            );

            // Cache
            println!(
                "  {}    Hit Ratio: {}",
                "Cache      ".bold(),
                health_color(pg.cache.hit_ratio * 100.0, 95.0, 90.0, true),
            );

            // Database size
            println!(
                "  {}    Total: {}  Tables: {}  Indexes: {}",
                "Storage    ".bold(),
                fmt_bytes(pg.database_size.total_bytes).cyan(),
                fmt_bytes(pg.database_size.tables_bytes),
                fmt_bytes(pg.database_size.indexes_bytes),
            );

            // Table stats
            if !pg.table_stats.is_empty() {
                println!();
                println!(
                    "  {}",
                    "  Table                  Size         Seq Scan    Idx Scan    Dead Rows"
                        .dimmed()
                );
                for t in &pg.table_stats {
                    let dead_pct = if t.live_tuples + t.dead_tuples > 0 {
                        t.dead_tuples as f64 / (t.live_tuples + t.dead_tuples) as f64 * 100.0
                    } else {
                        0.0
                    };
                    let dead_str = if dead_pct > 10.0 {
                        format!("{:.1}%", dead_pct).red().to_string()
                    } else if dead_pct > 5.0 {
                        format!("{:.1}%", dead_pct).yellow().to_string()
                    } else {
                        format!("{:.1}%", dead_pct)
                    };
                    println!(
                        "  {:24} {:12} {:11} {:11} {}",
                        truncate(&t.table_name, 24),
                        fmt_bytes(t.size_bytes),
                        format_count(t.seq_scan as usize),
                        format_count(t.idx_scan as usize),
                        dead_str,
                    );
                }
            }

            // Query stats
            if let Some(ref queries) = pg.query_stats {
                if !queries.is_empty() {
                    println!();
                    println!(
                        "  {}  {}",
                        "Top Queries".bold(),
                        "(pg_stat_statements)".dimmed()
                    );
                    println!("  {}", "  Calls     Total       Mean      Query".dimmed());
                    for q in queries {
                        println!(
                            "  {:9} {:11} {:9} {}",
                            format_count(q.calls as usize),
                            format_duration_ms(q.total_time_ms),
                            format_duration_ms(q.mean_time_ms),
                            truncate(&q.query, 60),
                        );
                    }
                }
            }

            // Index health
            if !pg.index_health.unused_indexes.is_empty() {
                println!();
                println!(
                    "  {}  {} unused out of {}",
                    "Indexes    ".bold(),
                    format!("{}", pg.index_health.unused_indexes.len()).yellow(),
                    pg.index_health.total_index_count,
                );
            }
        }

        DatabaseStats::Redis(r) => {
            // Overview
            println!(
                "  {}  Version: {}  Uptime: {}  Clients: {}  Blocked: {}",
                "Server     ".bold(),
                r.server.version.cyan(),
                format_uptime(r.server.uptime_seconds),
                r.server.connected_clients,
                r.server.blocked_clients,
            );

            // Memory
            println!(
                "  {}  Used: {}  RSS: {}  Peak: {}  Frag: {:.2}x  Policy: {}",
                "Memory     ".bold(),
                fmt_bytes(r.memory.used_bytes).cyan(),
                fmt_bytes(r.memory.rss_bytes),
                fmt_bytes(r.memory.peak_bytes),
                r.memory.fragmentation_ratio,
                r.memory.eviction_policy,
            );

            // Throughput
            println!(
                "  {}  Ops/sec: {}  Total Cmds: {}  Total Conns: {}",
                "Throughput ".bold(),
                format!("{:.0}", r.throughput.ops_per_sec).cyan(),
                format_count(r.throughput.total_commands as usize),
                format_count(r.throughput.total_connections as usize),
            );

            // Cache
            println!(
                "  {}  Hit Rate: {}  Hits: {}  Misses: {}  Expired: {}  Evicted: {}",
                "Cache      ".bold(),
                health_color(r.cache.hit_rate * 100.0, 95.0, 90.0, true),
                format_count(r.cache.hits as usize),
                format_count(r.cache.misses as usize),
                format_count(r.cache.expired_keys as usize),
                format_count(r.cache.evicted_keys as usize),
            );

            // Persistence
            println!(
                "  {}  RDB: {}  AOF: {}",
                "Persistence".bold(),
                r.persistence.rdb_last_save_status,
                if r.persistence.aof_enabled {
                    "enabled"
                } else {
                    "disabled"
                },
            );

            // Keyspace
            if !r.keyspace.is_empty() {
                println!();
                println!("  {}", "  DB     Keys       Expires    Avg TTL".dimmed());
                for db in &r.keyspace {
                    println!(
                        "  db{:<4} {:10} {:10} {}",
                        db.db_index,
                        format_count(db.keys as usize),
                        format_count(db.expires as usize),
                        if db.avg_ttl > 0 {
                            format_duration_ms(db.avg_ttl as f64)
                        } else {
                            "-".to_string()
                        },
                    );
                }
            }
        }

        DatabaseStats::MySQL(my) => {
            // Connections
            let util = if my.connections.max_connections > 0 {
                my.connections.threads_connected as f64 / my.connections.max_connections as f64
                    * 100.0
            } else {
                0.0
            };
            println!(
                "  {}  Connected: {}  Running: {}  Max Used: {}  Max: {}  ({})",
                "Connections".bold(),
                format!("{}", my.connections.threads_connected).cyan(),
                my.connections.threads_running,
                my.connections.max_used_connections,
                my.connections.max_connections,
                health_color(util, 60.0, 80.0, false),
            );

            // Buffer pool
            println!(
                "  {}  Hit Ratio: {}  Usage: {:.1}%  Size: {}",
                "Buffer Pool".bold(),
                health_color(my.buffer_pool.hit_ratio * 100.0, 95.0, 90.0, true),
                my.buffer_pool.usage_pct,
                fmt_bytes(my.buffer_pool.total_bytes),
            );

            // Query stats
            println!(
                "  {}  SELECT: {}  INSERT: {}  UPDATE: {}  DELETE: {}  Slow: {}",
                "Queries    ".bold(),
                format_count(my.queries.selects as usize),
                format_count(my.queries.inserts as usize),
                format_count(my.queries.updates as usize),
                format_count(my.queries.deletes as usize),
                if my.queries.slow_queries > 0 {
                    format!("{}", my.queries.slow_queries).yellow().to_string()
                } else {
                    "0".to_string()
                },
            );

            // InnoDB
            println!(
                "  {}  Reads: {}  Inserts: {}  Updates: {}  Deletes: {}",
                "InnoDB Rows".bold(),
                format_count(my.innodb.row_reads as usize),
                format_count(my.innodb.row_inserts as usize),
                format_count(my.innodb.row_updates as usize),
                format_count(my.innodb.row_deletes as usize),
            );

            // Table sizes
            if !my.table_sizes.is_empty() {
                println!();
                println!(
                    "  {}",
                    "  Table                  Data         Index        Total".dimmed()
                );
                for t in &my.table_sizes {
                    println!(
                        "  {:24} {:12} {:12} {}",
                        truncate(&t.table_name, 24),
                        fmt_bytes(t.data_bytes),
                        fmt_bytes(t.index_bytes),
                        fmt_bytes(t.total_bytes),
                    );
                }
            }
        }

        DatabaseStats::MongoDB(m) => {
            // Connections
            println!(
                "  {}  Current: {}  Available: {}  Total Created: {}",
                "Connections".bold(),
                format!("{}", m.connections.current).cyan(),
                m.connections.available,
                format_count(m.connections.total_created as usize),
            );

            // Operations
            println!(
                "  {}  Insert: {}  Query: {}  Update: {}  Delete: {}  Command: {}",
                "Operations ".bold(),
                format_count(m.operations.insert as usize),
                format_count(m.operations.query as usize),
                format_count(m.operations.update as usize),
                format_count(m.operations.delete as usize),
                format_count(m.operations.command as usize),
            );

            // Memory
            println!(
                "  {}  Resident: {} MB  Virtual: {} MB",
                "Memory     ".bold(),
                format!("{}", m.memory.resident_mb).cyan(),
                m.memory.virtual_mb,
            );

            // WiredTiger cache
            if m.wired_tiger.cache_max_bytes > 0 {
                let util = m.wired_tiger.cache_used_bytes as f64
                    / m.wired_tiger.cache_max_bytes as f64
                    * 100.0;
                println!(
                    "  {}  Used: {} / {}  ({})",
                    "WT Cache   ".bold(),
                    fmt_bytes(m.wired_tiger.cache_used_bytes),
                    fmt_bytes(m.wired_tiger.cache_max_bytes),
                    health_color(util, 80.0, 95.0, false),
                );
            }

            // Collection stats
            if !m.collection_stats.is_empty() {
                println!();
                println!(
                    "  {}",
                    "  Collection             Size         Count      Indexes".dimmed()
                );
                for c in &m.collection_stats {
                    println!(
                        "  {:24} {:12} {:10} {}",
                        truncate(&c.name, 24),
                        fmt_bytes(c.size_bytes),
                        format_count(c.count as usize),
                        c.index_count,
                    );
                }
            }
        }
    }

    println!();
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else if max_len <= 3 {
        ".".repeat(max_len)
    } else {
        format!(
            "{}...",
            s.chars()
                .take(max_len.saturating_sub(3))
                .collect::<String>()
        )
    }
}

fn format_duration_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.1}s", ms / 1000.0)
    } else if ms >= 1.0 {
        format!("{:.1}ms", ms)
    } else {
        format!("{:.0}us", ms * 1000.0)
    }
}

fn format_uptime(seconds: i64) -> String {
    let days = seconds / 86400;
    let hours = (seconds % 86400) / 3600;
    let mins = (seconds % 3600) / 60;
    if days > 0 {
        format!("{}d {}h", days, hours)
    } else if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}

/// Detect the database type from the source image string.
fn detect_database_type(source_image: Option<&str>) -> Option<DatabaseType> {
    let img = source_image?.to_lowercase();
    if img.contains("postgres") || img.contains("postgis") || img.contains("timescale") {
        Some(DatabaseType::PostgreSQL)
    } else if img.contains("redis") || img.contains("valkey") {
        Some(DatabaseType::Redis)
    } else if img.contains("mongo") {
        Some(DatabaseType::MongoDB)
    } else if img.contains("mysql") || img.contains("mariadb") {
        Some(DatabaseType::MySQL)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::format_window_label;

    #[test]
    fn window_label_includes_until_when_present() {
        assert_eq!(
            format_window_label("1h", Some("30m")),
            "from 1h ago until 30m ago"
        );
    }

    #[test]
    fn window_label_preserves_unbounded_relative_and_absolute_since() {
        assert_eq!(format_window_label("6h", None), "last 6h");
        assert_eq!(
            format_window_label("2024-01-15T10:00:00Z", None),
            "since 2024-01-15T10:00:00Z"
        );
    }
}
