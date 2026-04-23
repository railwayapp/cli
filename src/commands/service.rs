use anyhow::bail;
use chrono::{DateTime, Utc};
use is_terminal::IsTerminal;
use json_dotpath::DotPaths as _;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

use crate::{
    client::post_graphql,
    controllers::{
        environment::get_matched_environment,
        project::{
            ensure_project_and_environment_exist, find_service_instance, get_project,
            get_service_ids_in_env,
        },
    },
    errors::RailwayError,
    gql::queries::{
        self,
        project::{
            DeploymentInstanceStatus, DeploymentStatus,
            ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdgesNode, VolumeState,
        },
    },
    util::prompt::{PromptService, prompt_options},
};

use super::*;

/// Manage services
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,

    /// The service ID/name to link (deprecated: use 'service link' instead)
    service: Option<String>,
}

#[derive(Parser)]
enum Commands {
    /// List services in the current environment
    #[clap(alias = "ls")]
    List(ListArgs),

    /// Link a service to the current project
    Link(LinkArgs),

    /// Show deployment status for services
    Status(StatusArgs),

    /// View logs from a service
    Logs(crate::commands::logs::Args),

    /// Redeploy the latest deployment of a service
    Redeploy(crate::commands::redeploy::Args),

    /// Restart the latest deployment of a service
    Restart(crate::commands::restart::Args),

    /// Scale a service across regions
    Scale(crate::commands::scale::Args),
}

#[derive(Parser)]
struct LinkArgs {
    /// The service ID/name to link
    service: Option<String>,
}

#[derive(Parser)]
struct ListArgs {
    /// Environment to list services from (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceOutput {
    id: String,
    name: String,
    is_linked: bool,
    source: Option<ServiceSourceOutput>,
    status: Option<DeploymentStatus>,
    deployment_stopped: bool,
    deployment_id: Option<String>,
    latest_deployment: Option<LatestDeployment>,
    url: Option<String>,
    volumes: Vec<VolumeOutput>,
    regions: Vec<RegionConfig>,
    replicas: Option<ReplicasOutput>,
    volume_migrating: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RegionConfig {
    name: String,
    location: Option<String>,
    configured: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LatestDeployment {
    id: String,
    status: DeploymentStatus,
    created_at: DateTime<Utc>,
    deployment_stopped: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceSourceOutput {
    repo: Option<String>,
    image: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VolumeOutput {
    name: String,
    mount_path: String,
    current_size_mb: f64,
    size_mb: i64,
    state: Option<VolumeState>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReplicasOutput {
    configured: i64,
    running: i64,
    crashed: i64,
    exited: i64,
    total: i64,
}

struct DeploymentSnapshot {
    id: String,
    status: DeploymentStatus,
    deployment_stopped: bool,
    instances: Vec<DeploymentInstanceStatus>,
}

/// Legacy `status --all` output shape. Preserved so scripts parsing the
/// deprecated command's JSON don't break.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceStatusOutput {
    id: String,
    name: String,
    deployment_id: Option<String>,
    status: Option<String>,
    stopped: bool,
}

#[derive(Parser)]
struct StatusArgs {
    /// Service name or ID to show status for (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,

    /// Deprecated: use `railway service list` instead. Kept for backwards compatibility.
    #[clap(short, long, hide = true)]
    all: bool,

    /// Environment to check status in (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

pub async fn command(args: Args) -> Result<()> {
    // Handle legacy direct service link (when no subcommand is provided)
    // This maintains backward compatibility:
    // - `railway service` -> prompts to link
    // - `railway service <name>` -> links that service
    if args.command.is_none() {
        return link_command(LinkArgs {
            service: args.service,
        })
        .await;
    }

    match args.command {
        Some(Commands::List(list_args)) => list_command(list_args).await,
        Some(Commands::Link(link_args)) => link_command(link_args).await,
        Some(Commands::Status(status_args)) => status_command(status_args).await,
        Some(Commands::Logs(logs_args)) => crate::commands::logs::command(logs_args).await,
        Some(Commands::Redeploy(redeploy_args)) => {
            crate::commands::redeploy::command(redeploy_args).await
        }
        Some(Commands::Restart(restart_args)) => {
            crate::commands::restart::command(restart_args).await
        }
        Some(Commands::Scale(scale_args)) => crate::commands::scale::command(scale_args).await,
        None => unreachable!(),
    }
}

async fn list_command(args: ListArgs) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let (project, region_locations) = tokio::join!(
        get_project(&client, &configs, linked_project.project.clone()),
        fetch_region_locations(&client, &configs),
    );
    let project = project?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let env_id = if let Some(env_name) = args.environment {
        get_matched_environment(&project, env_name)?.id
    } else {
        linked_project.environment_id()?.to_string()
    };
    let env_name = project
        .environments
        .edges
        .iter()
        .find(|env| env.node.id == env_id)
        .map(|env| env.node.name.clone())
        .expect("environment resolved above");

    let service_ids_in_env = get_service_ids_in_env(&project, &env_id);
    let linked_service_id = linked_project.service.as_deref();

    let mut services: Vec<_> = project
        .services
        .edges
        .iter()
        .filter(|edge| service_ids_in_env.contains(&edge.node.id))
        .collect();
    services.sort_by(|a, b| a.node.name.to_lowercase().cmp(&b.node.name.to_lowercase()));

    let rows: Vec<ServiceOutput> = services
        .iter()
        .map(|edge| {
            build_service_output(
                &project,
                &env_id,
                &edge.node,
                linked_service_id,
                &region_locations,
            )
        })
        .collect();

    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if rows.is_empty() {
        println!("No services found in environment '{env_name}'");
        return Ok(());
    }

    println!();
    println!("{} {}", "Services in".bold(), env_name.blue().bold());
    println!();

    for row in &rows {
        print_service_card(row);
    }

    Ok(())
}

fn build_service_output(
    project: &crate::gql::queries::project::ProjectProject,
    env_id: &str,
    service: &crate::gql::queries::project::ProjectProjectServicesEdgesNode,
    linked_service_id: Option<&str>,
    region_locations: &HashMap<String, String>,
) -> ServiceOutput {
    let id = service.id.clone();
    let is_linked = linked_service_id == Some(id.as_str());
    let instance = find_service_instance(project, env_id, &id);

    let volumes = volumes_for_service(project, env_id, &id);
    let volume_migrating = volumes
        .iter()
        .any(|v| matches!(v.state, Some(VolumeState::MIGRATING)));

    let active = instance
        .map(|i| i.active_deployments.as_slice())
        .unwrap_or(&[]);
    let latest = instance.and_then(|i| i.latest_deployment.as_ref());

    let stable_active = active.iter().find(|d| is_stable_status(&d.status));
    let in_progress = active.iter().find(|d| is_in_progress_status(&d.status));
    // Use the currently serving deployment for the primary status snapshot, but
    // keep the latest deployment alongside it so rollouts that are still queued,
    // waiting for approval, or have already failed remain visible.
    let serving_snapshot = stable_active
        .map(|d| DeploymentSnapshot {
            id: d.id.clone(),
            status: d.status.clone(),
            deployment_stopped: d.deployment_stopped,
            instances: d.instances.iter().map(|i| i.status.clone()).collect(),
        })
        .or_else(|| {
            latest.map(|d| DeploymentSnapshot {
                id: d.id.clone(),
                status: d.status.clone(),
                deployment_stopped: d.deployment_stopped,
                instances: d.instances.iter().map(|i| i.status.clone()).collect(),
            })
        });

    // Resolve region config from the deployment's service manifest, mirroring
    // what `railway service scale` reads (and what the dashboard sums).
    // `ServiceInstance.numReplicas` on the public API is the legacy per-region
    // field and reports `1` for multi-region services — so we must read from meta.
    let mut regions = stable_active
        .and_then(|d| d.meta.as_ref())
        .map(regions_from_meta)
        .filter(|r| !r.is_empty())
        .or_else(|| {
            in_progress
                .and_then(|d| d.meta.as_ref())
                .map(regions_from_meta)
                .filter(|r| !r.is_empty())
        })
        .or_else(|| {
            latest
                .and_then(|d| d.meta.as_ref())
                .map(regions_from_meta)
                .filter(|r| !r.is_empty())
        })
        .unwrap_or_default();

    for r in &mut regions {
        r.location = region_locations.get(&r.name).cloned();
    }

    let configured = if !regions.is_empty() {
        regions.iter().map(|r| r.configured).sum()
    } else {
        instance.and_then(|i| i.num_replicas).unwrap_or(1)
    };

    // Snapshot fields: prefer the stable deployment from `activeDeployments`
    // (reflects what's currently serving during rollouts), fall back to
    // `latestDeployment` for cases where nothing stable is active.
    let (status, deployment_stopped, deployment_id, replica_instances) = serving_snapshot
        .map(|d| {
            (
                Some(d.status),
                d.deployment_stopped,
                Some(d.id),
                Some(d.instances),
            )
        })
        .unwrap_or((None, false, None, None));

    let replicas = replica_instances.map(|s| count_replicas(configured, s.into_iter()));

    let latest_deployment = latest.map(|d| LatestDeployment {
        id: d.id.clone(),
        status: d.status.clone(),
        created_at: d.created_at,
        deployment_stopped: d.deployment_stopped,
    });

    ServiceOutput {
        source: instance.and_then(source_from_instance),
        status,
        deployment_stopped,
        deployment_id,
        latest_deployment,
        url: instance.and_then(url_from_instance),
        volumes,
        regions,
        replicas,
        volume_migrating,
        id,
        name: service.name.clone(),
        is_linked,
    }
}

fn is_stable_status(status: &DeploymentStatus) -> bool {
    matches!(
        status,
        DeploymentStatus::SUCCESS | DeploymentStatus::CRASHED | DeploymentStatus::SLEEPING
    )
}

fn is_in_progress_status(status: &DeploymentStatus) -> bool {
    matches!(
        status,
        DeploymentStatus::BUILDING
            | DeploymentStatus::DEPLOYING
            | DeploymentStatus::INITIALIZING
            | DeploymentStatus::QUEUED
            | DeploymentStatus::WAITING
    )
}

fn is_rollout_status(status: &DeploymentStatus) -> bool {
    is_in_progress_status(status)
        || matches!(
            status,
            DeploymentStatus::NEEDS_APPROVAL | DeploymentStatus::FAILED
        )
}

fn count_replicas(
    configured: i64,
    statuses: impl Iterator<Item = DeploymentInstanceStatus>,
) -> ReplicasOutput {
    let mut running = 0i64;
    let mut crashed = 0i64;
    let mut exited = 0i64;
    let mut total = 0i64;
    for status in statuses {
        if matches!(
            status,
            DeploymentInstanceStatus::REMOVED | DeploymentInstanceStatus::REMOVING
        ) {
            continue;
        }
        total += 1;
        match status {
            DeploymentInstanceStatus::RUNNING => running += 1,
            DeploymentInstanceStatus::CRASHED => crashed += 1,
            DeploymentInstanceStatus::EXITED => exited += 1,
            _ => {}
        }
    }
    ReplicasOutput {
        configured,
        running,
        crashed,
        exited,
        total,
    }
}

/// Extracts per-region replica configuration from a deployment's `meta` blob.
///
/// Reads `serviceManifest.deploy.multiRegionConfig` (keyed by region name).
/// Falls back to `deploy.region` + `deploy.numReplicas` for legacy single-region
/// deployments. Returns an empty vec if the meta has neither.
/// This is the same path `railway service scale` uses to read the config.
fn regions_from_meta(meta: &Value) -> Vec<RegionConfig> {
    let Some(deploy) = meta
        .dot_get::<Value>("serviceManifest.deploy")
        .ok()
        .flatten()
    else {
        return Vec::new();
    };

    if let Some(config) = deploy
        .dot_get::<Value>("multiRegionConfig")
        .ok()
        .flatten()
        .and_then(|v| v.as_object().cloned())
    {
        let mut regions: Vec<RegionConfig> = config
            .into_iter()
            .map(|(name, v)| RegionConfig {
                name,
                location: None,
                configured: v.get("numReplicas").and_then(Value::as_i64).unwrap_or(0),
            })
            .collect();
        regions.sort_by(|a, b| a.name.cmp(&b.name));
        return regions;
    }

    if let Some(region) = deploy.get("region").and_then(Value::as_str) {
        let configured = deploy
            .get("numReplicas")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        return vec![RegionConfig {
            name: region.to_string(),
            location: None,
            configured,
        }];
    }

    Vec::new()
}

/// Fetches the region directory and returns a lookup from region code (e.g.
/// "europe-west4-drams3a") to its friendly location ("EU West").
/// Returns an empty map on failure — callers display raw codes instead.
async fn fetch_region_locations(
    client: &reqwest::Client,
    configs: &Configs,
) -> HashMap<String, String> {
    match post_graphql::<queries::Regions, _>(
        client,
        configs.get_backboard(),
        queries::regions::Variables,
    )
    .await
    {
        Ok(resp) => resp
            .regions
            .into_iter()
            .filter(|r| !r.location.is_empty())
            .map(|r| (r.name, r.location))
            .collect(),
        Err(_) => HashMap::new(),
    }
}

const FIELD_LABEL_WIDTH: usize = 14;

fn print_field(label: &str, value: &impl std::fmt::Display) {
    let padded = format!("{label:<FIELD_LABEL_WIDTH$}");
    println!("    {} {value}", padded.dimmed());
}

fn print_service_card(row: &ServiceOutput) {
    if row.is_linked {
        println!("{} {}", row.name.green().bold(), "(linked)".green());
    } else {
        println!("{}", row.name.bold());
    }
    print_field("status:", &derived_status_line(row));
    if let Some(source) = &row.source {
        if let Some(repo) = &source.repo {
            print_field("repo:", repo);
        }
        if let Some(image) = &source.image {
            print_field("image:", image);
        }
    }
    if let Some(url) = &row.url {
        print_field("url:", &url.clone().cyan());
    }
    for volume in &row.volumes {
        print_field("volume:", &format_volume_line(volume));
    }
    match row.regions.len() {
        0 => {}
        1 => print_field("region:", &format_regions_line(&row.regions)),
        _ => print_field("regions:", &format_regions_line(&row.regions)),
    }
    if let Some(r) = &row.replicas {
        if !row.deployment_stopped
            && (r.configured > 1 || r.crashed > 0 || r.running != r.configured)
        {
            print_field("replicas:", &format_replicas_line(r));
        }
    }
    if let Some(dep_id) = &row.deployment_id {
        print_field("deployment ID:", &dep_id.clone().dimmed());
    }
    print_field("service ID:", &row.id.clone().dimmed());
    println!();
}

fn format_replicas_line(r: &ReplicasOutput) -> String {
    let primary = format!("{}/{} running", r.running, r.configured);
    if r.crashed > 0 {
        format!("{}, {} crashed", primary, r.crashed.to_string().red())
    } else {
        primary
    }
}

fn format_regions_line(regions: &[RegionConfig]) -> String {
    if regions.len() == 1 {
        return region_display_name(&regions[0]);
    }
    let sep = " · ".dimmed();
    regions
        .iter()
        .map(|r| {
            format!(
                "{} ({})",
                region_display_name(r),
                r.configured.to_string().dimmed()
            )
        })
        .collect::<Vec<_>>()
        .join(&sep.to_string())
}

fn region_display_name(r: &RegionConfig) -> String {
    r.location.clone().unwrap_or_else(|| r.name.clone())
}

fn volumes_for_service(
    project: &crate::gql::queries::project::ProjectProject,
    env_id: &str,
    service_id: &str,
) -> Vec<VolumeOutput> {
    project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == env_id)
        .map(|env| {
            env.node
                .volume_instances
                .edges
                .iter()
                .filter(|vi| vi.node.service_id.as_deref() == Some(service_id))
                .map(|vi| VolumeOutput {
                    name: vi.node.volume.name.clone(),
                    mount_path: vi.node.mount_path.clone(),
                    current_size_mb: vi.node.current_size_mb,
                    size_mb: vi.node.size_mb,
                    state: vi.node.state.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn derived_status_line(row: &ServiceOutput) -> String {
    if row.volume_migrating {
        return format!(
            "{} {}",
            "●".dimmed(),
            "Service temporarily offline".dimmed()
        );
    }

    if row.status.as_ref().is_some_and(is_timed_rollout_status) {
        if let Some(deployment) = current_status_deployment(row) {
            let (dot, label) = rollout_status_line(deployment);
            return format!("{dot} {label}");
        }

        let (dot, label) = style_status(row.status.as_ref(), row.deployment_stopped);
        return format!("{dot} {label}");
    }

    let stable_line = stable_status_label(row);

    match (stable_line, latest_rollout_deployment(row)) {
        (Some(stable), Some(deployment)) => {
            let (_, label) = rollout_status_line(deployment);
            format!("{stable} {} {label}", "·".dimmed())
        }
        (Some(stable), None) => stable,
        (None, Some(deployment)) => {
            let (dot, label) = rollout_status_line(deployment);
            format!("{dot} {label}")
        }
        (None, None) => {
            let (dot, label) = style_status(row.status.as_ref(), row.deployment_stopped);
            format!("{dot} {label}")
        }
    }
}

fn stable_status_label(row: &ServiceOutput) -> Option<String> {
    let status = row.status.as_ref()?;
    let some_running = row.replicas.as_ref().is_some_and(|r| r.running > 0);
    let has_multiple = row
        .replicas
        .as_ref()
        .is_some_and(|r| r.total > 1 || r.configured > 1);
    let some_crashed = has_multiple && row.replicas.as_ref().is_some_and(|r| r.crashed > 0);

    let is_online = matches!(status, DeploymentStatus::SUCCESS)
        || (matches!(status, DeploymentStatus::CRASHED) && some_running);
    let is_crashed_out = matches!(status, DeploymentStatus::CRASHED) && !some_running;

    if is_online && row.deployment_stopped {
        return Some(format!("{} {}", "●".green(), "Completed".green()));
    }
    if is_online && some_crashed {
        let r = row
            .replicas
            .as_ref()
            .expect("some_crashed implies replicas");
        let label = format!("Online ({}/{} replicas crashed)", r.crashed, r.total);
        return Some(format!("{} {}", "●".yellow(), label.yellow()));
    }
    if is_online {
        return Some(format!("{} {}", "●".green(), "Online".green()));
    }
    if is_crashed_out {
        return Some(format!("{} {}", "●".red(), "Crashed".red()));
    }
    let (dot, label) = style_status(Some(status), row.deployment_stopped);
    Some(format!("{dot} {label}"))
}

fn is_timed_rollout_status(status: &DeploymentStatus) -> bool {
    is_in_progress_status(status) || matches!(status, DeploymentStatus::NEEDS_APPROVAL)
}

fn current_status_deployment(row: &ServiceOutput) -> Option<&LatestDeployment> {
    row.latest_deployment
        .as_ref()
        .filter(|d| Some(d.id.as_str()) == row.deployment_id.as_deref())
}

fn latest_rollout_deployment(row: &ServiceOutput) -> Option<&LatestDeployment> {
    row.latest_deployment.as_ref().filter(|d| {
        Some(d.id.as_str()) != row.deployment_id.as_deref() && is_rollout_status(&d.status)
    })
}

fn rollout_status_line(
    deployment: &LatestDeployment,
) -> (colored::ColoredString, colored::ColoredString) {
    let label = match &deployment.status {
        DeploymentStatus::BUILDING => "Building",
        DeploymentStatus::DEPLOYING => "Deploying",
        DeploymentStatus::INITIALIZING => "Initializing",
        DeploymentStatus::QUEUED => "Queued",
        DeploymentStatus::WAITING => "Waiting for CI",
        DeploymentStatus::NEEDS_APPROVAL => "Awaiting approval",
        DeploymentStatus::FAILED => "Deploy failed",
        _ => {
            return style_status(Some(&deployment.status), deployment.deployment_stopped);
        }
    };
    let label = format!("{label} ({})", format_elapsed(deployment.created_at));
    match deployment.status {
        DeploymentStatus::QUEUED => ("●".dimmed(), label.dimmed()),
        DeploymentStatus::NEEDS_APPROVAL => ("●".yellow(), label.yellow()),
        DeploymentStatus::FAILED => ("●".red(), label.red()),
        _ => ("●".blue(), label.blue()),
    }
}

fn format_elapsed(since: DateTime<Utc>) -> String {
    let seconds = (Utc::now() - since).num_seconds().max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        let h = seconds / 3600;
        let m = (seconds % 3600) / 60;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h {m}m")
        }
    } else {
        format!("{}d", seconds / 86400)
    }
}

fn format_volume_line(volume: &VolumeOutput) -> String {
    let total = volume.size_mb as f64;
    let pct = if total > 0.0 {
        volume.current_size_mb / total * 100.0
    } else {
        0.0
    };
    let usage = format_size_pair(volume.current_size_mb, total);
    let colored_usage = if pct >= 100.0 {
        usage.red()
    } else if pct >= 75.0 {
        usage.yellow()
    } else {
        usage.normal()
    };
    let sep = "·".dimmed();
    format!(
        "{} {sep} {} {sep} {}",
        volume.name, volume.mount_path, colored_usage,
    )
}

fn format_size_pair(current_mb: f64, size_mb: f64) -> String {
    let show_gb = current_mb >= 1024.0 || size_mb >= 1024.0;
    if show_gb {
        format!("{:.1} GB / {:.1} GB", current_mb / 1024.0, size_mb / 1024.0)
    } else {
        format!("{:.0} MB / {:.0} MB", current_mb, size_mb)
    }
}

fn source_from_instance(
    instance: &ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdgesNode,
) -> Option<ServiceSourceOutput> {
    let source = instance.source.as_ref()?;
    let repo = source.repo.clone().filter(|s| !s.is_empty());
    let image = source.image.clone().filter(|s| !s.is_empty());
    (repo.is_some() || image.is_some()).then_some(ServiceSourceOutput { repo, image })
}

fn url_from_instance(
    instance: &ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdgesNode,
) -> Option<String> {
    instance
        .domains
        .custom_domains
        .first()
        .map(|d| d.domain.clone())
        .or_else(|| {
            instance
                .domains
                .service_domains
                .first()
                .map(|d| d.domain.clone())
        })
        .map(|d| format!("https://{d}"))
}

fn style_status(
    status: Option<&DeploymentStatus>,
    stopped: bool,
) -> (colored::ColoredString, colored::ColoredString) {
    let Some(status) = status else {
        return ("○".dimmed(), "Offline".dimmed());
    };
    match (status, stopped) {
        (DeploymentStatus::SUCCESS, true) => ("●".green(), "Completed".green()),
        (DeploymentStatus::SUCCESS, false) => ("●".green(), "Online".green()),
        (DeploymentStatus::FAILED, _) => ("●".red(), "Failed".red()),
        (DeploymentStatus::CRASHED, _) => ("●".red(), "Crashed".red()),
        (DeploymentStatus::BUILDING, _) => ("●".blue(), "Building".blue()),
        (DeploymentStatus::DEPLOYING, _) => ("●".blue(), "Deploying".blue()),
        (DeploymentStatus::INITIALIZING, _) => ("●".blue(), "Initializing".blue()),
        (DeploymentStatus::QUEUED, _) => ("●".dimmed(), "Queued".dimmed()),
        (DeploymentStatus::WAITING, _) => ("●".blue(), "Waiting for CI".blue()),
        (DeploymentStatus::SLEEPING, _) => ("●".yellow(), "Sleeping".yellow()),
        (DeploymentStatus::NEEDS_APPROVAL, _) => ("●".yellow(), "Pending".yellow()),
        (DeploymentStatus::REMOVED, _) => ("●".dimmed(), "Removed".dimmed()),
        (DeploymentStatus::REMOVING, _) => ("●".dimmed(), "Removing".dimmed()),
        (DeploymentStatus::SKIPPED, _) => ("●".dimmed(), "Skipped".dimmed()),
        (DeploymentStatus::Other(s), _) => ("●".white(), s.clone().white()),
    }
}

async fn link_command(args: LinkArgs) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let service_ids_in_env = get_service_ids_in_env(&project, linked_project.environment_id()?);
    let services: Vec<_> = project
        .services
        .edges
        .iter()
        .filter(|a| service_ids_in_env.contains(&a.node.id))
        .map(|s| PromptService(&s.node))
        .collect();

    let service = if let Some(name) = args.service {
        services
            .into_iter()
            .find(|s| s.0.id.eq_ignore_ascii_case(&name) || s.0.name.eq_ignore_ascii_case(&name))
            .ok_or_else(|| RailwayError::ServiceNotFound(name))?
    } else if services.is_empty() {
        bail!("No services found")
    } else {
        if !std::io::stdout().is_terminal() {
            bail!("Service name required in non-interactive mode. Usage: railway service <name>");
        }
        prompt_options("Select a service", services)?
    };

    configs.link_service(service.0.id.clone())?;
    configs.write()?;
    println!("Linked service {}", service.0.name.green());
    Ok(())
}

async fn status_command(args: StatusArgs) -> Result<()> {
    if args.all {
        eprintln!(
            "{}",
            "Warning: `railway service status --all` is deprecated. Please use `railway service list` instead."
                .yellow()
        );
    }

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    // Determine which environment to use
    let environment_id = if let Some(env_name) = args.environment {
        let env = get_matched_environment(&project, env_name)?;
        env.id
    } else {
        linked_project.environment_id()?.to_string()
    };

    let environment_name = project
        .environments
        .edges
        .iter()
        .find(|env| env.node.id == environment_id)
        .map(|env| env.node.name.clone())
        .context("Environment not found")?;

    // Collect service instances for the environment
    let mut service_statuses: Vec<ServiceStatusOutput> = Vec::new();

    let env = project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment_id)
        .context("Environment not found")?;

    for instance_edge in &env.node.service_instances.edges {
        let instance = &instance_edge.node;
        let deployment = &instance.latest_deployment;

        service_statuses.push(ServiceStatusOutput {
            id: instance.service_id.clone(),
            name: instance.service_name.clone(),
            deployment_id: deployment.as_ref().map(|d| d.id.clone()),
            status: deployment.as_ref().map(|d| format!("{:?}", d.status)),
            stopped: deployment
                .as_ref()
                .map(|d| d.deployment_stopped)
                .unwrap_or(false),
        });
    }

    if args.all {
        // Show all services
        if args.json {
            println!("{}", serde_json::to_string_pretty(&service_statuses)?);
        } else {
            if service_statuses.is_empty() {
                println!("No services found in environment '{}'", environment_name);
                return Ok(());
            }

            println!("Services in {}:\n", environment_name.blue().bold());

            for status in service_statuses {
                let status_display = format_status_display(&status);

                println!(
                    "{:<20} | {:<14} | {}",
                    status.name.bold(),
                    status.deployment_id.as_deref().unwrap_or("N/A").dimmed(),
                    status_display
                );
            }
        }
    } else {
        // Show single service (specified or linked)
        let target_service = if let Some(service_name) = args.service {
            service_statuses
                .iter()
                .find(|s| s.id == service_name || s.name == service_name)
                .ok_or_else(|| RailwayError::ServiceNotFound(service_name.clone()))?
        } else {
            // Use linked service
            let linked_service_id = linked_project
                .service
                .as_ref()
                .context("No service linked. Use --service flag or --all to see all services")?;

            service_statuses
                .iter()
                .find(|s| &s.id == linked_service_id)
                .context("Linked service not found in this environment")?
        };

        if args.json {
            println!("{}", serde_json::to_string_pretty(&target_service)?);
        } else {
            println!("Service: {}", target_service.name.green().bold());
            println!(
                "Deployment: {}",
                target_service
                    .deployment_id
                    .as_deref()
                    .unwrap_or("No deployment")
                    .dimmed()
            );
            println!("Status: {}", format_status_display(target_service));
        }
    }

    Ok(())
}

fn format_status_display(status: &ServiceStatusOutput) -> colored::ColoredString {
    if status.stopped && status.status.as_deref() == Some("SUCCESS") {
        return "STOPPED".yellow();
    }

    match status.status.as_deref() {
        Some("SUCCESS") => "SUCCESS".green(),
        Some("FAILED") | Some("CRASHED") => status.status.as_deref().unwrap_or("UNKNOWN").red(),
        Some("BUILDING") | Some("DEPLOYING") | Some("INITIALIZING") | Some("QUEUED") => {
            status.status.as_deref().unwrap_or("UNKNOWN").blue()
        }
        Some("SLEEPING") => "SLEEPING".yellow(),
        Some("REMOVED") | Some("REMOVING") => {
            status.status.as_deref().unwrap_or("UNKNOWN").dimmed()
        }
        Some(s) => s.white(),
        None => "NO DEPLOYMENT".dimmed(),
    }
}
