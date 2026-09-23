use std::collections::HashMap;

use chrono::{DateTime, Utc};
use chrono_humanize::HumanTime;
use serde::Serialize;
use serde_json::Value;

use crate::{
    client::post_graphql,
    controllers::{
        environment::get_matched_environment,
        project::{ensure_project_and_environment_exist, get_project, resolve_project_id_or_name},
    },
    errors::RailwayError,
    util::{progress::create_spinner_if, time::parse_time},
};

use super::*;

const DEFAULT_TRACES: i64 = 100;
const MAX_TRACES: i64 = 500;
const MAX_SPANS: i64 = 2000;

/// Manage tracing for a service or project and inspect its traces
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway trace status\n  railway trace status --all\n  railway trace enable --service api\n  railway trace enable --auto-instrument\n  railway trace enable --project-default --sample-rate 0.25\n  railway trace disable\n  railway trace inherit\n  railway trace list --since 30m --errors\n  railway trace list --all --filter '@http.route:/api/users @duration:>500' --json\n  railway trace get 4bf92f3577b34da6a3ce929d0e0e4736\n\nAutomation notes:\n  `list --json` prints one trace summary per line and `get --json` one span per line, like `railway logs --json`.\n  Tracing is a service-wide setting, not per environment; --environment only scopes status, list and get.\n  Changing tracing needs a user or workspace token. A project token (RAILWAY_TOKEN) can only read."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Service name or ID (defaults to linked service)
    #[clap(short, long, global = true)]
    service: Option<String>,

    /// Environment to use (defaults to linked environment)
    #[clap(short, long, global = true)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID", global = true)]
    project: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,
}

#[derive(Parser)]
enum Commands {
    /// Turn tracing on for a service, or for the project default with --project-default
    Enable(EnableArgs),

    /// Turn tracing off for a service, or for the project default with --project-default
    Disable(DisableArgs),

    /// Clear a service's tracing override so it follows the project default again
    Inherit,

    /// Show tracing settings and when spans were last exported
    Status {
        /// Show every service in the project
        #[clap(short = 'a', long)]
        all: bool,
    },

    /// List traces, newest first
    #[clap(visible_alias = "ls")]
    List(ListArgs),

    /// Show the spans of one trace as a tree
    #[clap(visible_alias = "show")]
    Get {
        /// W3C trace id, 32 hex characters
        #[clap(value_name = "TRACE_ID", value_parser = parse_trace_id)]
        trace_id: String,

        /// Max spans to return, oldest first (default 1000, max 2000)
        #[clap(long, value_parser = parse_max_spans)]
        max_spans: Option<i64>,
    },
}

#[derive(Parser)]
struct EnableArgs {
    /// Also turn on auto-instrumentation (eBPF/OBI) for the service
    #[clap(long, conflicts_with = "project_default")]
    auto_instrument: bool,

    /// Change the project default instead of one service
    #[clap(long)]
    project_default: bool,

    /// Fraction of client-facing requests the edge traces, 0 to 1 (with --project-default)
    #[clap(long, requires = "project_default", value_parser = parse_sample_rate)]
    sample_rate: Option<f64>,
}

#[derive(Parser)]
struct DisableArgs {
    /// Also turn off auto-instrumentation (eBPF/OBI) for the service
    #[clap(long, conflicts_with = "project_default")]
    auto_instrument: bool,

    /// Change the project default instead of one service
    #[clap(long)]
    project_default: bool,
}

#[derive(Parser)]
struct ListArgs {
    /// List traces of every service in the environment
    #[clap(short = 'a', long)]
    all: bool,

    /// Filter expression over spans, e.g. '@status:error @http.route:/api/users @duration:>500'
    #[clap(short = 'f', long)]
    filter: Option<String>,

    /// Only traces with an error span (adds @status:error to the filter)
    #[clap(long)]
    errors: bool,

    /// Start of the time window: relative (30m, 2h, 1d) or ISO 8601
    #[clap(long, short = 'S', default_value = "1h")]
    since: String,

    /// End of the time window, same formats as --since (default: now)
    #[clap(long, short = 'U')]
    until: Option<String>,

    /// Max traces to return (1 to 500)
    #[clap(short = 'n', long, default_value_t = DEFAULT_TRACES, value_parser = parse_limit)]
    limit: i64,
}

pub async fn command(args: Args) -> Result<()> {
    let Args {
        command,
        service,
        environment,
        project,
        json,
    } = args;

    match command {
        Commands::Enable(enable) => {
            if enable.project_default {
                set_project_tracing(project, environment, true, enable.sample_rate, json).await
            } else {
                set_service_tracing(
                    project,
                    service,
                    environment,
                    ServiceChange::Set {
                        enabled: true,
                        auto_instrument: enable.auto_instrument,
                    },
                    json,
                )
                .await
            }
        }
        Commands::Disable(disable) => {
            if disable.project_default {
                set_project_tracing(project, environment, false, None, json).await
            } else {
                set_service_tracing(
                    project,
                    service,
                    environment,
                    ServiceChange::Set {
                        enabled: false,
                        auto_instrument: disable.auto_instrument,
                    },
                    json,
                )
                .await
            }
        }
        Commands::Inherit => {
            set_service_tracing(project, service, environment, ServiceChange::Inherit, json).await
        }
        Commands::Status { all } => status(project, service, environment, all, json).await,
        Commands::List(list_args) => list(project, service, environment, list_args, json).await,
        Commands::Get {
            trace_id,
            max_spans,
        } => get(project, environment, trace_id, max_spans, json).await,
    }
}

// --- Scope resolution ---

struct Scope {
    client: reqwest::Client,
    configs: Configs,
    project: queries::RailwayProject,
    /// `(id, name)` of the environment, when one was asked for.
    environment: Option<(String, String)>,
    linked_service: Option<String>,
}

impl Scope {
    fn environment(&self) -> Result<&(String, String)> {
        self.environment
            .as_ref()
            .context("No environment linked. Use --environment when using --project")
    }
}

/// Project and environment from the flags or the linked project, like
/// `resolve_service_context`, but the service is picked separately so
/// project-wide commands work without one.
async fn resolve_scope(
    project_arg: Option<String>,
    environment_arg: Option<String>,
    need_environment: bool,
) -> Result<Scope> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    if need_environment && project_arg.is_some() && environment_arg.is_none() {
        bail!("--environment is required when using --project");
    }

    let linked_project = if project_arg.is_none() {
        Some(configs.get_linked_project().await?)
    } else {
        None
    };

    if let Some(ref linked_project) = linked_project {
        ensure_project_and_environment_exist(&client, &configs, linked_project).await?;
    }

    let project_id = match project_arg {
        Some(project_arg) => resolve_project_id_or_name(&client, &configs, &project_arg).await?,
        None => linked_project
            .as_ref()
            .map(|lp| lp.project.clone())
            .ok_or_else(|| {
                anyhow::anyhow!("No project specified. Use --project or run `railway link` first")
            })?,
    };

    let project = get_project(&client, &configs, project_id).await?;

    let environment_id_or_name = environment_arg.or_else(|| {
        linked_project.as_ref().and_then(|lp| {
            lp.environment_name
                .clone()
                .or_else(|| lp.environment.clone())
        })
    });
    let environment = match environment_id_or_name {
        Some(env) => {
            let environment = get_matched_environment(&project, env)?;
            Some((environment.id, environment.name))
        }
        None if need_environment => {
            bail!("No environment linked. Use --environment when using --project")
        }
        None => None,
    };

    Ok(Scope {
        client,
        configs,
        project,
        environment,
        linked_service: linked_project.and_then(|lp| lp.service),
    })
}

/// The service named by `--service`, else the linked one, else the only one.
fn pick_service(scope: &Scope, service_arg: Option<&str>) -> Result<(String, String)> {
    let services = &scope.project.services.edges;
    if services.is_empty() {
        bail!(RailwayError::ProjectHasNoServices);
    }

    match (service_arg, &scope.linked_service) {
        (Some(service_arg), _) => {
            let service = services
                .iter()
                .find(|s| s.node.name.eq_ignore_ascii_case(service_arg) || s.node.id == service_arg)
                .with_context(|| format!("Service '{service_arg}' not found"))?;
            Ok((service.node.id.clone(), service.node.name.clone()))
        }
        (None, Some(linked_service)) => {
            let name = services
                .iter()
                .find(|s| &s.node.id == linked_service)
                .map(|s| s.node.name.clone())
                .unwrap_or_else(|| linked_service.clone());
            Ok((linked_service.clone(), name))
        }
        (None, None) if services.len() == 1 => {
            let service = &services[0].node;
            Ok((service.id.clone(), service.name.clone()))
        }
        (None, None) => bail!(RailwayError::NoServiceLinked),
    }
}

// --- Settings ---

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTracing {
    pub id: String,
    pub name: String,
    /// Whether services without their own override are traced.
    pub tracing_enabled: bool,
    /// Fraction of requests the edge traces, 0 to 1. None is Railway's default.
    pub sample_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceTracing {
    pub id: String,
    pub name: String,
    /// The service's own setting: true or false pins it, None follows the project.
    pub tracing_override: Option<bool>,
    /// Whether the service is traced once the project default is applied.
    pub tracing_enabled: bool,
    pub auto_instrumentation_enabled: bool,
    /// Auto-instrumentation only does anything while tracing is enabled.
    pub auto_instrumentation_active: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceTracingStatus {
    #[serde(flatten)]
    pub tracing: ServiceTracing,
    pub last_edge_span_at: Option<String>,
    pub last_service_span_at: Option<String>,
}

/// A service's override wins; otherwise the project decides. Returns
/// `(tracing enabled, auto-instrumentation active)`.
pub fn effective_tracing(
    project_default: bool,
    service_override: Option<bool>,
    auto_instrumentation_enabled: bool,
) -> (bool, bool) {
    let enabled = service_override.unwrap_or(project_default);
    (enabled, enabled && auto_instrumentation_enabled)
}

struct TracingSettings {
    project: ProjectTracing,
    services: Vec<ServiceTracing>,
}

async fn fetch_tracing_settings(scope: &Scope) -> Result<TracingSettings> {
    let project = post_graphql::<queries::TracingSettings, _>(
        &scope.client,
        scope.configs.get_backboard(),
        queries::tracing_settings::Variables {
            project_id: scope.project.id.clone(),
        },
    )
    .await?
    .project;

    let project_tracing = ProjectTracing {
        id: project.id,
        name: project.name,
        tracing_enabled: project.tracing_enabled,
        sample_rate: project.tracing_sample_rate,
    };
    let mut services: Vec<ServiceTracing> = project
        .services
        .edges
        .into_iter()
        .map(|edge| {
            let node = edge.node;
            let (enabled, auto_active) = effective_tracing(
                project_tracing.tracing_enabled,
                node.tracing_enabled,
                node.auto_instrumentation_enabled,
            );
            ServiceTracing {
                id: node.id,
                name: node.name,
                tracing_override: node.tracing_enabled,
                tracing_enabled: enabled,
                auto_instrumentation_enabled: node.auto_instrumentation_enabled,
                auto_instrumentation_active: auto_active,
            }
        })
        .collect();
    services.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    Ok(TracingSettings {
        project: project_tracing,
        services,
    })
}

fn find_service(settings: &TracingSettings, service_id: &str) -> Result<ServiceTracing> {
    settings
        .services
        .iter()
        .find(|s| s.id == service_id)
        .cloned()
        .with_context(|| format!("Service {service_id} not found in project"))
}

enum ServiceChange {
    Set {
        enabled: bool,
        auto_instrument: bool,
    },
    Inherit,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceChangeOutput {
    project: ProjectTracing,
    service: ServiceTracing,
    updated_fields: Vec<&'static str>,
}

async fn set_service_tracing(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    change: ServiceChange,
    json: bool,
) -> Result<()> {
    let scope = resolve_scope(project, environment, false).await?;
    let (service_id, service_name) = pick_service(&scope, service.as_deref())?;
    let backboard = scope.configs.get_backboard();

    let spinner = create_spinner_if(
        !json,
        format!("Updating tracing for {}...", service_name.bold()),
    );

    let updated_fields: Vec<&'static str> = match change {
        ServiceChange::Set {
            enabled,
            auto_instrument,
        } => {
            post_graphql::<mutations::ServiceTracingUpdate, _>(
                &scope.client,
                &backboard,
                mutations::service_tracing_update::Variables {
                    id: service_id.clone(),
                    input: mutations::service_tracing_update::ServiceUpdateInput {
                        tracing_enabled: Some(enabled),
                        auto_instrumentation_enabled: auto_instrument.then_some(enabled),
                        icon: None,
                        name: None,
                    },
                },
            )
            .await?;
            if auto_instrument {
                vec!["tracingEnabled", "autoInstrumentationEnabled"]
            } else {
                vec!["tracingEnabled"]
            }
        }
        ServiceChange::Inherit => {
            post_graphql::<mutations::ServiceTracingInherit, _>(
                &scope.client,
                &backboard,
                mutations::service_tracing_inherit::Variables {
                    id: service_id.clone(),
                },
            )
            .await?;
            vec!["tracingEnabled"]
        }
    };

    let settings = fetch_tracing_settings(&scope).await?;
    let service = find_service(&settings, &service_id)?;

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ServiceChangeOutput {
                project: settings.project,
                service,
                updated_fields,
            })?
        );
        return Ok(());
    }

    println!(
        "Updated {}: {}.",
        service.name.bold(),
        format_tracing_state(&service)
    );
    println!("{}", format_project_default(&settings.project));
    if service.auto_instrumentation_enabled && !service.auto_instrumentation_active {
        println!(
            "Auto-instrumentation is on but does nothing until the service's tracing is enabled."
        );
    }
    println!("{}", TRACING_EFFECTS_NOTE.dimmed());

    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectChangeOutput {
    project: ProjectTracing,
    services: Vec<ServiceTracing>,
    updated_fields: Vec<&'static str>,
}

async fn set_project_tracing(
    project: Option<String>,
    environment: Option<String>,
    enabled: bool,
    sample_rate: Option<f64>,
    json: bool,
) -> Result<()> {
    let scope = resolve_scope(project, environment, false).await?;

    let spinner = create_spinner_if(
        !json,
        format!(
            "Updating the tracing default for {}...",
            scope.project.name.bold()
        ),
    );

    post_graphql::<mutations::ProjectTracingUpdate, _>(
        &scope.client,
        scope.configs.get_backboard(),
        mutations::project_tracing_update::Variables {
            id: scope.project.id.clone(),
            input: mutations::project_tracing_update::ProjectUpdateInput {
                tracing_enabled: Some(enabled),
                tracing_sample_rate: sample_rate,
                base_environment_id: None,
                bot_pr_environments: None,
                description: None,
                focused_pr_environments: None,
                is_public: None,
                name: None,
                pr_deploys: None,
            },
        },
    )
    .await?;

    let updated_fields = if sample_rate.is_some() {
        vec!["tracingEnabled", "tracingSampleRate"]
    } else {
        vec!["tracingEnabled"]
    };
    let settings = fetch_tracing_settings(&scope).await?;

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ProjectChangeOutput {
                project: settings.project,
                services: settings.services,
                updated_fields,
            })?
        );
        return Ok(());
    }

    println!(
        "Updated {}. {}",
        settings.project.name.bold(),
        format_project_default(&settings.project)
    );
    print_status_table(&settings.services, None);
    println!("{}", TRACING_EFFECTS_NOTE.dimmed());

    Ok(())
}

const TRACING_EFFECTS_NOTE: &str = "Edge tracing follows within seconds. Auto-instrumentation reaches running containers within about a minute. The OpenTelemetry exporter variables an app's own SDK needs are set on the next deploy.";

// --- Status ---

#[derive(Serialize)]
struct StatusOutput {
    project: ProjectTracing,
    services: Vec<ServiceTracingStatus>,
}

async fn status(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    all: bool,
    json: bool,
) -> Result<()> {
    if all && service.is_some() {
        bail!("--all and --service cannot be used together");
    }

    let scope = resolve_scope(project, environment, true).await?;
    let (environment_id, environment_name) = scope.environment()?.clone();
    let only_service = if all {
        None
    } else {
        Some(pick_service(&scope, service.as_deref())?.0)
    };

    let spinner = create_spinner_if(!json, "Fetching tracing status...".into());

    let settings = fetch_tracing_settings(&scope).await?;
    let activity = post_graphql::<queries::TracingStatus, _>(
        &scope.client,
        scope.configs.get_backboard(),
        queries::tracing_status::Variables {
            environment_id: environment_id.clone(),
        },
    )
    .await?
    .tracing_status;
    let activity: Activity = activity
        .into_iter()
        .map(|s| (s.service_id, (s.last_edge_span_at, s.last_service_span_at)))
        .collect();

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    let services: Vec<ServiceTracingStatus> = settings
        .services
        .into_iter()
        .filter(|s| only_service.as_deref().is_none_or(|id| id == s.id))
        .map(|tracing| {
            let (edge, app) = activity.get(&tracing.id).cloned().unwrap_or_default();
            ServiceTracingStatus {
                tracing,
                last_edge_span_at: edge.map(|t| t.to_rfc3339()),
                last_service_span_at: app.map(|t| t.to_rfc3339()),
            }
        })
        .collect();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&StatusOutput {
                project: settings.project,
                services,
            })?
        );
        return Ok(());
    }

    println!(
        "Tracing for project {} in environment {}",
        settings.project.name.bold(),
        environment_name.bold()
    );
    println!("{}", format_project_default(&settings.project));
    if services.is_empty() {
        println!("No services in this project.");
        return Ok(());
    }
    println!();
    print_status_table(
        &services
            .iter()
            .map(|s| s.tracing.clone())
            .collect::<Vec<_>>(),
        Some(&activity),
    );

    Ok(())
}

fn format_sample_rate(rate: Option<f64>) -> String {
    match rate {
        None => "100% (Railway default)".to_string(),
        Some(rate) => format!("{}%", (rate * 1000.0).round() / 10.0),
    }
}

fn format_project_default(project: &ProjectTracing) -> String {
    format!(
        "Project default: tracing {}, sample rate {}",
        on_off(project.tracing_enabled),
        format_sample_rate(project.sample_rate)
    )
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn tracing_cell(service: &ServiceTracing) -> String {
    format!(
        "{} ({})",
        on_off(service.tracing_enabled),
        if service.tracing_override.is_none() {
            "follows project"
        } else {
            "pinned"
        }
    )
}

fn auto_instrumentation_cell(service: &ServiceTracing) -> &'static str {
    match (
        service.auto_instrumentation_enabled,
        service.auto_instrumentation_active,
    ) {
        (false, _) => "off",
        (true, true) => "on",
        (true, false) => "on (inactive until tracing is enabled)",
    }
}

/// One line of tracing state, without the service name.
fn format_tracing_state(service: &ServiceTracing) -> String {
    format!(
        "tracing {}, auto-instrumentation {}",
        tracing_cell(service),
        auto_instrumentation_cell(service)
    )
}

type Activity = HashMap<String, (Option<DateTime<Utc>>, Option<DateTime<Utc>>)>;

fn format_last_seen(at: Option<DateTime<Utc>>) -> String {
    match at {
        Some(at) => HumanTime::from(at).to_string(),
        None => "never".to_string(),
    }
}

fn print_status_table(services: &[ServiceTracing], activity: Option<&Activity>) {
    let mut headers = vec!["Service", "Tracing", "Auto-instrumentation"];
    if activity.is_some() {
        headers.extend(["Last edge span", "Last app span"]);
    }
    let rows: Vec<Vec<String>> = services
        .iter()
        .map(|service| {
            let mut row = vec![
                service.name.clone(),
                tracing_cell(service),
                auto_instrumentation_cell(service).to_string(),
            ];
            if let Some(activity) = activity {
                let (edge, app) = activity.get(&service.id).cloned().unwrap_or_default();
                row.push(format_last_seen(edge));
                row.push(format_last_seen(app));
            }
            row
        })
        .collect();
    print_table(&headers, &rows);
}

// --- Traces ---

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TraceSummary {
    pub trace_id: String,
    pub started_at: String,
    pub duration_ms: f64,
    pub root_span_name: String,
    pub root_service_name: String,
    pub root_service_id: String,
    pub root_component: String,
    pub root_server_address: Option<String>,
    pub root_url_path: Option<String>,
    pub service_name: Option<String>,
    pub has_edge: bool,
    pub span_count: i64,
    pub error_count: i64,
}

impl From<queries::traces::TracesTraces> for TraceSummary {
    fn from(t: queries::traces::TracesTraces) -> Self {
        Self {
            trace_id: t.trace_id,
            started_at: t.started_at,
            duration_ms: t.duration_ms,
            root_span_name: t.root_span_name,
            root_service_name: t.root_service_name,
            root_service_id: t.root_service_id,
            root_component: t.root_component,
            root_server_address: t.root_server_address,
            root_url_path: t.root_url_path,
            service_name: t.service_name,
            has_edge: t.has_edge,
            span_count: t.span_count,
            error_count: t.error_count,
        }
    }
}

/// `--errors` and `--filter` combined into one filter expression.
pub fn compose_trace_filter(errors: bool, filter: Option<&str>) -> Option<String> {
    let mut parts = Vec::new();
    if errors {
        parts.push("@status:error".to_string());
    }
    if let Some(filter) = filter.map(str::trim).filter(|f| !f.is_empty()) {
        parts.push(filter.to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

async fn list(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    args: ListArgs,
    json: bool,
) -> Result<()> {
    if args.all && service.is_some() {
        bail!("--all and --service cannot be used together");
    }

    let start = parse_time(&args.since)?;
    let end = args.until.as_deref().map(parse_time).transpose()?;
    if let Some(end) = end {
        if end <= start {
            bail!("--until must be after --since");
        }
    }
    let filter = compose_trace_filter(args.errors, args.filter.as_deref());

    let scope = resolve_scope(project, environment, true).await?;
    let (environment_id, environment_name) = scope.environment()?.clone();
    let service = if args.all {
        None
    } else {
        Some(pick_service(&scope, service.as_deref())?)
    };

    let spinner = create_spinner_if(!json, "Fetching traces...".into());

    let traces: Vec<TraceSummary> = post_graphql::<queries::Traces, _>(
        &scope.client,
        scope.configs.get_backboard(),
        queries::traces::Variables {
            environment_id,
            service_id: service.as_ref().map(|(id, _)| id.clone()),
            filter,
            start_date: Some(start.to_rfc3339()),
            end_date: end.map(|t| t.to_rfc3339()),
            limit: Some(args.limit),
        },
    )
    .await?
    .traces
    .into_iter()
    .map(TraceSummary::from)
    .collect();

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    if json {
        for trace in &traces {
            println!("{}", serde_json::to_string(trace)?);
        }
        return Ok(());
    }

    let scope_label = match &service {
        Some((_, name)) => format!("service {}", name.bold()),
        None => "all services".to_string(),
    };
    if traces.is_empty() {
        println!(
            "No traces for {scope_label} in environment {} since {}.",
            environment_name.bold(),
            args.since
        );
        return Ok(());
    }

    println!(
        "Traces for {scope_label} in environment {} since {} (newest first):",
        environment_name.bold(),
        args.since
    );
    print_trace_table(&traces);
    if traces.len() as i64 >= args.limit {
        println!(
            "{}",
            format!(
                "Showing the newest {} traces; narrow the window or raise --limit to see more.",
                traces.len()
            )
            .dimmed()
        );
    }

    Ok(())
}

fn format_started_at(iso: &str) -> String {
    DateTime::parse_from_rfc3339(iso)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| iso.to_string())
}

fn format_duration_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2}s", ms / 1000.0)
    } else {
        format!("{ms:.1}ms")
    }
}

fn print_trace_table(traces: &[TraceSummary]) {
    let rows: Vec<Vec<String>> = traces
        .iter()
        .map(|t| {
            vec![
                format_started_at(&t.started_at),
                format_duration_ms(t.duration_ms),
                t.span_count.to_string(),
                t.error_count.to_string(),
                format!("{} › {}", t.root_service_name, t.root_span_name),
                t.trace_id.clone(),
            ]
        })
        .collect();
    print_table(
        &["Started", "Duration", "Spans", "Errors", "Root", "Trace ID"],
        &rows,
    );
}

// --- Spans ---

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpanEvent {
    pub timestamp: String,
    pub name: String,
    pub attributes: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpanLink {
    pub trace_id: String,
    pub span_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub started_at: String,
    pub duration_ms: f64,
    pub name: String,
    pub kind: String,
    pub component: String,
    pub service_name: String,
    pub service_id: Option<String>,
    pub deployment_id: Option<String>,
    pub deployment_instance_id: Option<String>,
    pub status_code: String,
    pub status_message: String,
    pub resource_attributes: Value,
    pub span_attributes: Value,
    pub events: Vec<SpanEvent>,
    pub links: Vec<SpanLink>,
}

impl From<queries::trace::TraceTrace> for Span {
    fn from(s: queries::trace::TraceTrace) -> Self {
        Self {
            trace_id: s.trace_id,
            span_id: s.span_id,
            parent_span_id: s.parent_span_id,
            started_at: s.started_at,
            duration_ms: s.duration_ms,
            name: s.name,
            kind: s.kind,
            component: s.component,
            service_name: s.service_name,
            service_id: s.service_id,
            deployment_id: s.deployment_id,
            deployment_instance_id: s.deployment_instance_id,
            status_code: s.status_code,
            status_message: s.status_message,
            resource_attributes: s.resource_attributes,
            span_attributes: s.span_attributes,
            events: s
                .events
                .into_iter()
                .map(|e| SpanEvent {
                    timestamp: e.timestamp,
                    name: e.name,
                    attributes: e.attributes,
                })
                .collect(),
            links: s
                .links
                .into_iter()
                .map(|l| SpanLink {
                    trace_id: l.trace_id,
                    span_id: l.span_id,
                })
                .collect(),
        }
    }
}

/// Spans as an indented tree, children by start time. A span whose parent is
/// not in the set (sampled out, or past the span cap) is shown at the root so
/// the rest still reads.
pub fn render_span_tree(spans: &[Span]) -> Vec<String> {
    let ids: std::collections::HashSet<&str> = spans.iter().map(|s| s.span_id.as_str()).collect();
    let mut children: HashMap<Option<&str>, Vec<&Span>> = HashMap::new();
    for span in spans {
        let parent = span
            .parent_span_id
            .as_deref()
            .filter(|parent| ids.contains(parent));
        children.entry(parent).or_default().push(span);
    }
    for list in children.values_mut() {
        list.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    }

    fn walk(
        children: &HashMap<Option<&str>, Vec<&Span>>,
        parent: Option<&str>,
        depth: usize,
        lines: &mut Vec<String>,
    ) {
        if let Some(list) = children.get(&parent) {
            for span in list {
                lines.push(format_span_line(span, depth));
                walk(children, Some(span.span_id.as_str()), depth + 1, lines);
            }
        }
    }

    let mut lines = Vec::new();
    walk(&children, None, 0, &mut lines);
    lines
}

fn format_span_line(span: &Span, depth: usize) -> String {
    let status = match span.status_code.as_str() {
        "ERROR" if span.status_message.is_empty() => "ERROR".red().to_string(),
        "ERROR" => format!("ERROR: {}", span.status_message).red().to_string(),
        "OK" => "OK".green().to_string(),
        other => other.dimmed().to_string(),
    };
    format!(
        "{}{} {} {} {}",
        "  ".repeat(depth),
        span.name.bold(),
        format!("[{}/{}]", span.component, span.service_name).dimmed(),
        format_duration_ms(span.duration_ms),
        status
    )
}

async fn get(
    project: Option<String>,
    environment: Option<String>,
    trace_id: String,
    max_spans: Option<i64>,
    json: bool,
) -> Result<()> {
    let scope = resolve_scope(project, environment, true).await?;
    let (environment_id, environment_name) = scope.environment()?.clone();

    let spinner = create_spinner_if(!json, "Fetching trace...".into());

    let spans: Vec<Span> = post_graphql::<queries::Trace, _>(
        &scope.client,
        scope.configs.get_backboard(),
        queries::trace::Variables {
            environment_id,
            trace_id: trace_id.clone(),
            max_spans,
        },
    )
    .await?
    .trace
    .into_iter()
    .map(Span::from)
    .collect();

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    let truncated = max_spans.is_some_and(|max| spans.len() as i64 >= max)
        || (max_spans.is_none() && spans.len() >= 1000);

    if json {
        for span in &spans {
            println!("{}", serde_json::to_string(span)?);
        }
        if truncated {
            eprintln!(
                "Showing the first {} spans; raise --max-spans to see more.",
                spans.len()
            );
        }
        return Ok(());
    }

    if spans.is_empty() {
        println!(
            "No spans found for trace {} in environment {} within the project's retention.",
            trace_id.bold(),
            environment_name.bold()
        );
        return Ok(());
    }

    println!(
        "Trace {} in environment {} ({} span{})",
        trace_id.bold(),
        environment_name.bold(),
        spans.len(),
        if spans.len() == 1 { "" } else { "s" }
    );
    println!();
    for line in render_span_tree(&spans) {
        println!("{line}");
    }
    if truncated {
        println!(
            "{}",
            format!(
                "Showing the first {} spans; raise --max-spans to see more.",
                spans.len()
            )
            .dimmed()
        );
    }

    Ok(())
}

// --- Parsing and layout ---

fn parse_sample_rate(value: &str) -> std::result::Result<f64, String> {
    let rate: f64 = value
        .parse()
        .map_err(|_| "sample rate must be a number from 0 to 1".to_string())?;
    if !(0.0..=1.0).contains(&rate) {
        return Err("sample rate must be a number from 0 to 1".to_string());
    }
    Ok(rate)
}

fn parse_limit(value: &str) -> std::result::Result<i64, String> {
    let limit: i64 = value
        .parse()
        .map_err(|_| format!("limit must be a number from 1 to {MAX_TRACES}"))?;
    if !(1..=MAX_TRACES).contains(&limit) {
        return Err(format!("limit must be a number from 1 to {MAX_TRACES}"));
    }
    Ok(limit)
}

fn parse_max_spans(value: &str) -> std::result::Result<i64, String> {
    let max: i64 = value
        .parse()
        .map_err(|_| format!("max spans must be a number from 1 to {MAX_SPANS}"))?;
    if !(1..=MAX_SPANS).contains(&max) {
        return Err(format!("max spans must be a number from 1 to {MAX_SPANS}"));
    }
    Ok(max)
}

fn parse_trace_id(value: &str) -> std::result::Result<String, String> {
    let value = value.trim();
    if value.len() != 32 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("trace id must be 32 hex characters".to_string());
    }
    Ok(value.to_ascii_lowercase())
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(i, header)| {
            rows.iter()
                .map(|row| console::measure_text_width(&row[i]))
                .max()
                .unwrap_or(0)
                .max(header.len())
        })
        .collect();

    let line = |cells: Vec<String>| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                if i == cells.len() - 1 {
                    cell.clone()
                } else {
                    format!("{:<width$}", cell, width = widths[i])
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
    };

    println!(
        "{}",
        line(headers.iter().map(|h| h.to_string()).collect()).bold()
    );
    for row in rows {
        println!("{}", line(row.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn span(id: &str, parent: Option<&str>, started_at: &str) -> Span {
        Span {
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(),
            span_id: id.into(),
            parent_span_id: parent.map(str::to_string),
            started_at: started_at.into(),
            duration_ms: 1.5,
            name: format!("span-{id}"),
            kind: "SERVER".into(),
            component: "service".into(),
            service_name: "api".into(),
            service_id: None,
            deployment_id: None,
            deployment_instance_id: None,
            status_code: "OK".into(),
            status_message: String::new(),
            resource_attributes: Value::Object(Default::default()),
            span_attributes: Value::Object(Default::default()),
            events: vec![],
            links: vec![],
        }
    }

    #[test]
    fn parses_subcommands() {
        assert!(matches!(
            Args::parse_from(["trace", "enable", "--auto-instrument"]).command,
            Commands::Enable(EnableArgs {
                auto_instrument: true,
                project_default: false,
                sample_rate: None,
            })
        ));
        assert!(matches!(
            Args::parse_from(["trace", "enable", "--project-default", "--sample-rate", "0.25"])
                .command,
            Commands::Enable(EnableArgs {
                project_default: true,
                sample_rate: Some(rate),
                ..
            }) if rate == 0.25
        ));
        assert!(matches!(
            Args::parse_from(["trace", "disable", "--project-default"]).command,
            Commands::Disable(DisableArgs {
                project_default: true,
                auto_instrument: false,
            })
        ));
        assert!(matches!(
            Args::parse_from(["trace", "inherit", "-s", "api"]).command,
            Commands::Inherit
        ));
        assert!(matches!(
            Args::parse_from(["trace", "status", "--all"]).command,
            Commands::Status { all: true }
        ));
        assert!(matches!(
            Args::parse_from(["trace", "ls", "--errors", "--since", "30m", "-n", "5"]).command,
            Commands::List(ListArgs {
                errors: true,
                limit: 5,
                ..
            })
        ));
        assert!(matches!(
            Args::parse_from(["trace", "show", "4BF92F3577B34DA6A3CE929D0E0E4736"]).command,
            Commands::Get { trace_id, max_spans: None }
                if trace_id == "4bf92f3577b34da6a3ce929d0e0e4736"
        ));
    }

    #[test]
    fn rejects_bad_values() {
        assert!(Args::try_parse_from(["trace", "get", "not-a-trace-id"]).is_err());
        assert!(Args::try_parse_from(["trace", "list", "--limit", "0"]).is_err());
        assert!(Args::try_parse_from(["trace", "list", "--limit", "501"]).is_err());
        assert!(
            Args::try_parse_from([
                "trace",
                "get",
                "4bf92f3577b34da6a3ce929d0e0e4736",
                "--max-spans",
                "2001"
            ])
            .is_err()
        );
        assert!(Args::try_parse_from(["trace", "enable", "--sample-rate", "0.5"]).is_err());
        assert!(
            Args::try_parse_from([
                "trace",
                "enable",
                "--project-default",
                "--sample-rate",
                "1.5"
            ])
            .is_err()
        );
        assert!(
            Args::try_parse_from(["trace", "enable", "--project-default", "--auto-instrument"])
                .is_err()
        );
    }

    #[test]
    fn service_override_wins_over_project_default() {
        assert_eq!(effective_tracing(false, None, true), (false, false));
        assert_eq!(effective_tracing(true, None, true), (true, true));
        assert_eq!(effective_tracing(true, Some(false), true), (false, false));
        assert_eq!(effective_tracing(false, Some(true), false), (true, false));
        assert_eq!(effective_tracing(false, Some(true), true), (true, true));
    }

    #[test]
    fn composes_the_trace_filter() {
        assert_eq!(compose_trace_filter(false, None), None);
        assert_eq!(compose_trace_filter(false, Some("  ")), None);
        assert_eq!(
            compose_trace_filter(true, None).as_deref(),
            Some("@status:error")
        );
        assert_eq!(
            compose_trace_filter(true, Some("@http.route:/api")).as_deref(),
            Some("@status:error @http.route:/api")
        );
    }

    #[test]
    fn renders_spans_as_a_tree_with_orphans_at_the_root() {
        let spans = vec![
            span("c", Some("a"), "2026-09-23T10:00:00.200Z"),
            span("a", None, "2026-09-23T10:00:00.000Z"),
            span("b", Some("a"), "2026-09-23T10:00:00.100Z"),
            span("d", Some("missing"), "2026-09-23T10:00:00.050Z"),
        ];
        let lines = render_span_tree(&spans);
        let plain: Vec<String> = lines
            .iter()
            .map(|l| console::strip_ansi_codes(l).to_string())
            .collect();
        assert_eq!(plain.len(), 4);
        assert!(plain[0].starts_with("span-a "), "{}", plain[0]);
        assert!(plain[1].starts_with("  span-b "), "{}", plain[1]);
        assert!(plain[2].starts_with("  span-c "), "{}", plain[2]);
        assert!(plain[3].starts_with("span-d "), "{}", plain[3]);
    }

    #[test]
    fn json_output_uses_camel_case_keys() {
        let value = serde_json::to_value(span("a", None, "2026-09-23T10:00:00Z")).unwrap();
        assert_eq!(value["spanId"], "a");
        assert!(value["parentSpanId"].is_null());
        assert!(value.get("span_id").is_none());

        let status = ServiceTracingStatus {
            tracing: ServiceTracing {
                id: "svc".into(),
                name: "api".into(),
                tracing_override: None,
                tracing_enabled: true,
                auto_instrumentation_enabled: false,
                auto_instrumentation_active: false,
            },
            last_edge_span_at: None,
            last_service_span_at: None,
        };
        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["tracingEnabled"], true);
        assert!(value["tracingOverride"].is_null());
        assert!(value["lastEdgeSpanAt"].is_null());
    }

    #[test]
    fn formats_sample_rates_and_durations() {
        assert_eq!(format_sample_rate(None), "100% (Railway default)");
        assert_eq!(format_sample_rate(Some(0.25)), "25%");
        assert_eq!(format_sample_rate(Some(0.001)), "0.1%");
        assert_eq!(format_duration_ms(12.34), "12.3ms");
        assert_eq!(format_duration_ms(1500.0), "1.50s");
    }
}
