use std::collections::HashMap;

use chrono::{DateTime, Utc};
use colored::Colorize;
use json_dotpath::DotPaths as _;
use serde::Serialize;
use serde_json::Value;

use crate::{
    client::post_graphql,
    config::Configs,
    controllers::project::{
        ProjectEnvironmentInstances, ProjectServiceInstanceNode, find_service_instance,
        volume_instances_in_env,
    },
    gql::queries::{
        self,
        environment_instances::{DeploymentInstanceStatus, DeploymentStatus, VolumeState},
        project::ProjectProjectServicesEdgesNode,
    },
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::commands) struct ServiceOutput {
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

pub(in crate::commands) fn build_service_output(
    environment_instances: &ProjectEnvironmentInstances,
    service: &ProjectProjectServicesEdgesNode,
    linked_service_id: Option<&str>,
    region_locations: &HashMap<String, String>,
) -> ServiceOutput {
    let id = service.id.clone();
    let is_linked = linked_service_id == Some(id.as_str());
    let instance = find_service_instance(environment_instances, &id);

    let volumes = volumes_for_service(environment_instances, &id);
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
    // field and reports `1` for multi-region services, so we must read from meta.
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

pub(in crate::commands) async fn fetch_region_locations(
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
            .flat_map(|r| {
                let mut entries = vec![(r.name, r.location.clone())];
                if let Some(region) = r.region {
                    entries.push((region, r.location));
                }
                entries
            })
            .collect(),
        Err(_) => HashMap::new(),
    }
}

const FIELD_LABEL_WIDTH: usize = 14;

pub(in crate::commands) fn print_service_card(row: &ServiceOutput, show_linked_marker: bool) {
    if row.is_linked && show_linked_marker {
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

pub(in crate::commands) fn service_resource_details(row: &ServiceOutput) -> Vec<String> {
    let mut details = vec![derived_status_line(row)];

    if let Some(url) = &row.url {
        details.push(url.clone());
    }

    if let Some(r) = &row.replicas {
        if !row.deployment_stopped
            && (r.configured > 1 || r.crashed > 0 || r.running != r.configured)
        {
            details.push(format_replicas_line(r));
        }
    }

    details.extend(row.volumes.iter().map(|volume| volume.name.clone()));
    details
}

pub(in crate::commands) fn format_size_pair(current_mb: f64, size_mb: f64) -> String {
    let show_gb = current_mb >= 1024.0 || size_mb >= 1024.0;
    if show_gb {
        format!("{:.1} GB / {:.1} GB", current_mb / 1024.0, size_mb / 1024.0)
    } else {
        format!("{:.0} MB / {:.0} MB", current_mb, size_mb)
    }
}

fn print_field(label: &str, value: &impl std::fmt::Display) {
    let padded = format!("{label:<FIELD_LABEL_WIDTH$}");
    println!("    {} {value}", padded.dimmed());
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
    r.location
        .clone()
        .or_else(|| friendly_region_fallback(&r.name))
        .unwrap_or_else(|| r.name.clone())
}

fn friendly_region_fallback(region: &str) -> Option<String> {
    let normalized = region.to_ascii_lowercase();
    let label = if normalized.starts_with("europe-west") {
        "EU West"
    } else if normalized.starts_with("europe-north") {
        "EU North"
    } else if normalized.starts_with("europe-south") {
        "EU South"
    } else if normalized.starts_with("europe-central") {
        "EU Central"
    } else if normalized.starts_with("us-west") || normalized.starts_with("northamerica-west") {
        "US West"
    } else if normalized.starts_with("us-east") || normalized.starts_with("northamerica-east") {
        "US East"
    } else if normalized.starts_with("us-central") || normalized.starts_with("northamerica-central")
    {
        "US Central"
    } else if normalized.starts_with("asia-east") {
        "Asia East"
    } else if normalized.starts_with("asia-southeast") {
        "Asia Southeast"
    } else if normalized.starts_with("asia-south") {
        "Asia South"
    } else if normalized.starts_with("australia") {
        "Australia"
    } else if normalized.starts_with("southamerica") {
        "South America"
    } else {
        return None;
    };

    Some(label.to_string())
}

fn volumes_for_service(
    environment_instances: &ProjectEnvironmentInstances,
    service_id: &str,
) -> Vec<VolumeOutput> {
    volume_instances_in_env(environment_instances)
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

fn source_from_instance(instance: &ProjectServiceInstanceNode) -> Option<ServiceSourceOutput> {
    let source = instance.source.as_ref()?;
    let repo = source.repo.clone().filter(|s| !s.is_empty());
    let image = source.image.clone().filter(|s| !s.is_empty());
    (repo.is_some() || image.is_some()).then_some(ServiceSourceOutput { repo, image })
}

fn url_from_instance(instance: &ProjectServiceInstanceNode) -> Option<String> {
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
