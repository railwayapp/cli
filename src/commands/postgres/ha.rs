//! `railway postgres ha` -- high-availability Postgres clustering.

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;
use serde::Serialize;

use crate::controllers::{
    cluster_scale::{self, EdgeScaleSummary, ScaleClusterParams, ScaleDimensionSummary},
    config::{EnvironmentConfig, fetch_environment_config},
    patroni,
    postgres_plugins::{self, HaState, PitrState},
    project::{ServiceContext, resolve_service_context},
    template_apply::{
        self, ApplyKind, ApplyTemplateParams, HA_TEMPLATE_CODE, RevertTemplateParams,
    },
};

use super::{
    ResourceRef, confirm_or_bail, print_field, resolve_root, service_name_map, status_label,
};

/// Manage high-availability clustering for Postgres
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway postgres ha status --service postgres\n  railway postgres ha convert --service postgres --replicas 2\n  railway postgres ha convert --service postgres --replicas 2 --coordinators 3 --edge 1\n  railway postgres ha revert --service postgres --yes\n  railway postgres ha scale --service postgres --replicas 3\n  railway postgres ha switchover --service postgres --to postgres-replica-1\n\nAutomation notes:\n  Omitted --replicas/--coordinators/--edge on `convert` leave the template's authored count untouched.\n  --coordinators must be an odd number (consensus quorum)."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser)]
enum Commands {
    /// Show HA cluster status
    Status,

    /// Convert a standalone Postgres service into an HA cluster
    Convert(ConvertArgs),

    /// Revert an HA cluster back to standalone Postgres
    Revert(RevertArgs),

    /// Scale cluster replicas, coordinators, or edge nodes
    Scale(ScaleArgs),

    /// Promote a replica to leader (brief downtime)
    #[clap(visible_alias = "promote")]
    Switchover(SwitchoverArgs),
}

#[derive(Parser)]
struct ConvertArgs {
    /// Number of replicas (excluding the primary); omit to keep the template default
    #[clap(long)]
    replicas: Option<i64>,

    /// Number of coordinator/consensus nodes (e.g. etcd); must be odd; omit to keep the template default
    #[clap(long)]
    coordinators: Option<i64>,

    /// Number of edge/load-balancer replicas (e.g. HAProxy); omit to keep the template default
    #[clap(long)]
    edge: Option<i64>,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Stage the change without deploying it immediately
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct RevertArgs {
    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Stage the change without deploying it immediately
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
#[clap(group(
    clap::ArgGroup::new("target")
        .args(["replicas", "coordinators", "edge"])
        .required(true)
        .multiple(true)
))]
struct ScaleArgs {
    /// Target replica count
    #[clap(long)]
    replicas: Option<i64>,

    /// Target coordinator/consensus node count (must stay odd)
    #[clap(long)]
    coordinators: Option<i64>,

    /// Target edge/load-balancer replica count
    #[clap(long)]
    edge: Option<i64>,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Stage the change without deploying it immediately
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct SwitchoverArgs {
    /// Service name or ID of the replica to promote
    #[clap(long)]
    to: String,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,
}

pub async fn command(
    args: Args,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    match args.command {
        Commands::Status => status(project, service, environment, json).await,
        Commands::Convert(a) => convert(project, service, environment, json, a).await,
        Commands::Revert(a) => revert(project, service, environment, json, a).await,
        Commands::Scale(a) => scale(project, service, environment, json, a).await,
        Commands::Switchover(a) => switchover(project, service, environment, json, a).await,
    }
}

/// Members whose live Patroni role/state actually matters for `status` and
/// `switchover`/`revert`'s precheck -- the data nodes (root + replicas).
/// Coordinator/edge members don't run Patroni themselves.
fn data_node_members(ha_state: &HaState) -> Vec<(String, String)> {
    ha_state
        .members
        .iter()
        .filter(|m| matches!(m.cluster_role.as_deref(), Some("root") | Some("replica")))
        .map(|m| (m.service_id.clone(), m.service_name.clone()))
        .collect()
}

async fn status(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    print_status(&ctx, &config, json).await
}

async fn print_status(ctx: &ServiceContext, config: &EnvironmentConfig, json: bool) -> Result<()> {
    let root = resolve_root(ctx, config);
    let names = service_name_map(ctx);
    let ha_state = postgres_plugins::compute_ha_state(config, &root.root_id, &names);

    let live = if ha_state.is_cluster {
        match patroni::probe_members(ctx, &data_node_members(&ha_state)).await {
            Ok(live) => live,
            Err(err) => {
                eprintln!("Warning: could not probe live cluster status: {err:#}");
                Default::default()
            }
        }
    } else {
        Default::default()
    };

    let members: Vec<HaMemberOutput> = ha_state
        .members
        .iter()
        .map(|m| {
            let probe = live.get(&m.service_id);
            let self_view = probe.and_then(|p| p.self_view.as_ref());
            HaMemberOutput {
                service: ResourceRef {
                    id: m.service_id.clone(),
                    name: m.service_name.clone(),
                },
                cluster_role: m.cluster_role.clone(),
                live_role: self_view.map(|v| v.role.clone()).filter(|s| !s.is_empty()),
                live_state: self_view.map(|v| v.state.clone()).filter(|s| !s.is_empty()),
                live_lag: self_view.and_then(|v| v.lag.as_ref()).map(format_lag),
                reachable: probe.map(|p| p.reachable),
            }
        })
        .collect();

    let output = HaStatusOutput {
        service: ResourceRef {
            id: ctx.service_id.clone(),
            name: ctx.service_name.clone(),
        },
        environment: ResourceRef {
            id: ctx.environment_id.clone(),
            name: ctx.environment_name.clone(),
        },
        root: ResourceRef {
            id: root.root_id.clone(),
            name: root.root_name.clone(),
        },
        is_cluster: ha_state.is_cluster,
        members,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_ha_status(&output);
    }
    Ok(())
}

fn format_lag(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn print_ha_status(output: &HaStatusOutput) {
    println!("{}", "High availability".bold());
    println!();
    print_field("Service:", &output.service.name.green().bold());
    print_field("Environment:", &output.environment.name.blue().bold());
    if output.root.id != output.service.id {
        print_field("Cluster root:", &output.root.name);
    }
    print_field("Status:", &status_label(output.is_cluster));

    if output.is_cluster {
        println!();
        println!("{}", "Members:".bold());
        println!(
            "  {:<28} {:<12} {:<10} {:<10} LAG",
            "NAME", "CONFIG ROLE", "LIVE ROLE", "STATE"
        );
        for member in &output.members {
            let live_role = match &member.reachable {
                Some(true) => member.live_role.as_deref().unwrap_or("-"),
                Some(false) => "unreachable",
                None => "-",
            };
            let state = member.live_state.as_deref().unwrap_or("-");
            let lag = member.live_lag.as_deref().unwrap_or("-");
            println!(
                "  {:<28} {:<12} {:<10} {:<10} {}",
                member.service.name,
                member.cluster_role.as_deref().unwrap_or("-"),
                live_role,
                state,
                lag
            );
        }
    }
}

fn guardrail_blockers(state: &PitrState) -> Vec<String> {
    let mut blockers = Vec::new();
    if state.unsupported_image {
        blockers.push(
            "Image is not an official Railway Postgres image -- HA conversion is not supported."
                .to_string(),
        );
    }
    if state.minor_pinned {
        blockers.push(
            "Image is pinned to a minor version -- unpin to the major tag (e.g. `:16`) before converting to HA."
                .to_string(),
        );
    }
    if state.has_start_command {
        blockers.push(
            "A custom start command overrides the Postgres entrypoint -- clear it before converting to HA."
                .to_string(),
        );
    }
    blockers
}

async fn convert(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: ConvertArgs,
) -> Result<()> {
    if let Some(coordinators) = args.coordinators
        && coordinators % 2 == 0
    {
        bail!("--coordinators must be an odd number for consensus quorum (got {coordinators})");
    }

    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = postgres_plugins::compute_ha_state(&config, &root.root_id, &names);

    if ha_state.is_cluster {
        bail!("{} is already an HA cluster.", root.root_name);
    }

    let target_service = config.services.get(&root.root_id).with_context(|| {
        format!(
            "Service \"{}\" not found in environment config",
            root.root_name
        )
    })?;
    let blockers = guardrail_blockers(&postgres_plugins::compute_pitr_state(target_service));
    if !blockers.is_empty() {
        bail!(
            "Cannot convert {} to HA:\n  - {}",
            root.root_name,
            blockers.join("\n  - ")
        );
    }

    if !confirm_or_bail(
        &format!(
            "Convert {} to an HA cluster? Connection endpoints will change and active connections will drop.",
            root.root_name.yellow()
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    let volume_instance_id = target_service.volume_mounts.keys().next().cloned();
    let result = template_apply::apply_composable_template(
        &ctx,
        ApplyTemplateParams {
            template_code: HA_TEMPLATE_CODE.to_string(),
            service_id: root.root_id.clone(),
            volume_instance_id,
            replica_count: args.replicas,
            internal_count: args.coordinators,
            edge_count: args.edge,
            edge_variables: None,
            kind: ApplyKind::Conversion,
            auto_deploy: !args.no_deploy,
        },
    )
    .await
    .context("Failed to convert to HA")?;

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    if !json {
        let verb = if result.deployed {
            "Converted and deployed"
        } else {
            "Staged the conversion of"
        };
        println!(
            "{verb} {} to an HA cluster in environment {} (project {}).",
            root.root_name.bold(),
            ctx.environment_name.bold(),
            result.project_id
        );
    }
    print_status(&ctx, &config, json).await
}

async fn revert(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: RevertArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = postgres_plugins::compute_ha_state(&config, &root.root_id, &names);

    if !ha_state.is_cluster {
        bail!("{} is not an HA cluster.", root.root_name);
    }

    // Live precheck: revert is only safe while the root is the current
    // Patroni leader (matches the frontend's own gate before allowing
    // revert). A stale/uncaught-up former leader still running as a
    // replica would silently lose whatever writes landed on the real
    // leader once the cluster is torn down. Degrades to a warning (rather
    // than blocking) if no cluster member is reachable at all -- mirrors
    // `pitr disable`'s replication-health precheck, which does the same.
    let data_nodes = data_node_members(&ha_state);
    match patroni::probe_members(&ctx, &data_nodes).await {
        Ok(live) => {
            let root_probe = live.get(&root.root_id);
            let root_is_leader = root_probe
                .and_then(|p| p.self_view.as_ref())
                .is_some_and(|v| v.role == "leader");

            if !root_is_leader {
                let current_leader = live
                    .values()
                    .filter_map(|p| p.self_view.as_ref())
                    .find(|v| v.role == "leader")
                    .map(|v| v.name.clone());
                let any_reachable = live.values().any(|p| p.reachable);

                if any_reachable {
                    bail!(
                        "{} is not currently the Patroni leader{}. Run `railway postgres ha switchover --to {}` first, then revert.",
                        root.root_name,
                        current_leader
                            .map(|l| format!(" (current leader: {l})"))
                            .unwrap_or_default(),
                        root.root_name,
                    );
                }
                eprintln!(
                    "Warning: could not reach any cluster member to verify {} is the current Patroni leader before reverting. Proceeding anyway.",
                    root.root_name
                );
            }
        }
        Err(err) => {
            eprintln!(
                "Warning: could not check the current Patroni leader before reverting: {err:#}"
            );
        }
    }

    if !confirm_or_bail(
        &format!(
            "Revert {} to standalone Postgres? Connection endpoints will change and active connections will drop.",
            root.root_name.red()
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    let result = template_apply::revert_template(
        &ctx,
        RevertTemplateParams {
            template_code: HA_TEMPLATE_CODE.to_string(),
            root_service_id: root.root_id.clone(),
            auto_deploy: !args.no_deploy,
        },
    )
    .await
    .context("Failed to revert HA cluster")?;

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    if !json {
        let verb = if result.deployed {
            "Reverted and deployed"
        } else {
            "Staged the revert of"
        };
        println!(
            "{verb} {} to standalone Postgres in environment {} (project {}).",
            root.root_name.bold(),
            ctx.environment_name.bold(),
            result.project_id
        );
    }
    print_status(&ctx, &config, json).await
}

async fn scale(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: ScaleArgs,
) -> Result<()> {
    if let Some(coordinators) = args.coordinators {
        cluster_scale::validate_odd_coordinator_count(coordinators)?;
    }

    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = postgres_plugins::compute_ha_state(&config, &root.root_id, &names);

    if !ha_state.is_cluster {
        bail!(
            "{} is not an HA cluster. Use `railway postgres ha convert` first.",
            root.root_name
        );
    }

    let mut summary_lines = Vec::new();
    if let Some(n) = args.replicas {
        summary_lines.push(format!("replicas -> {n}"));
    }
    if let Some(n) = args.coordinators {
        summary_lines.push(format!("coordinators -> {n}"));
    }
    if let Some(n) = args.edge {
        summary_lines.push(format!("edge -> {n}"));
    }
    if !confirm_or_bail(
        &format!(
            "Scale {} ({})? This may create or delete whole services and volumes.",
            root.root_name.yellow(),
            summary_lines.join(", ")
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    let result = cluster_scale::scale_cluster(
        &ctx,
        &root.root_id,
        &root.root_name,
        &names,
        ScaleClusterParams {
            replicas: args.replicas,
            coordinators: args.coordinators,
            edge: args.edge,
            auto_deploy: !args.no_deploy,
        },
    )
    .await
    .context("Failed to scale HA cluster")?;

    if !json {
        print_scale_result(&root.root_name, &result);
    }

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    print_status(&ctx, &config, json).await
}

fn print_scale_result(root_name: &str, result: &cluster_scale::ScaleClusterResult) {
    let verb = if result.deployed {
        "Scaled and deployed"
    } else {
        "Staged the scale of"
    };
    println!("{verb} {} -- ", root_name.bold());

    let print_dimension = |label: &str, summary: &ScaleDimensionSummary| {
        if summary.is_noop() {
            println!("  {label}: already at the requested count.");
            return;
        }
        if !summary.added.is_empty() {
            println!("  {label}: added {}", summary.added.join(", "));
        }
        if !summary.removed.is_empty() {
            println!("  {label}: removed {}", summary.removed.join(", "));
        }
    };

    if let Some(summary) = &result.replicas {
        print_dimension("Replicas", summary);
    }
    if let Some(summary) = &result.coordinators {
        print_dimension("Coordinators", summary);
    }
    if let Some(EdgeScaleSummary {
        region,
        previous_replicas,
        target_replicas,
    }) = &result.edge
    {
        println!("  Edge ({region}): {previous_replicas} -> {target_replicas}");
    }
}

async fn switchover(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: SwitchoverArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = postgres_plugins::compute_ha_state(&config, &root.root_id, &names);

    if !ha_state.is_cluster {
        bail!("{} is not an HA cluster.", root.root_name);
    }

    let candidate = ha_state
        .members
        .iter()
        .find(|m| m.service_id == args.to || m.service_name.eq_ignore_ascii_case(&args.to))
        .with_context(|| format!("\"{}\" is not a member of this HA cluster", args.to))?;

    if !matches!(
        candidate.cluster_role.as_deref(),
        Some("root") | Some("replica")
    ) {
        bail!(
            "Switchover target must be a Postgres data node (root or replica), not \"{}\".",
            candidate.cluster_role.as_deref().unwrap_or("unknown")
        );
    }

    if !confirm_or_bail(
        &format!(
            "Promote {} to leader? This causes a brief write downtime while Postgres fails over.",
            candidate.service_name.yellow()
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    let data_nodes = data_node_members(&ha_state);
    let instance_ids = patroni::resolve_instance_ids(
        &ctx,
        &data_nodes
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
    )
    .await
    .context("Failed to resolve live cluster member instances")?;

    let probe_targets: Vec<String> = instance_ids.values().cloned().collect();
    let Some((probe_instance_id, cluster_members)) = patroni::probe_any(&probe_targets).await
    else {
        bail!("Could not reach any cluster member's Patroni API to determine the current leader.");
    };

    let leader = cluster_members
        .iter()
        .find(|m| m.role == "leader")
        .context("Patroni did not report a current leader")?;

    let candidate_patroni_name = candidate.service_name.to_ascii_lowercase();
    if !cluster_members
        .iter()
        .any(|m| m.name.to_ascii_lowercase() == candidate_patroni_name)
    {
        bail!(
            "\"{}\" is not currently a recognized Patroni cluster member.",
            candidate.service_name
        );
    }

    if leader.name.to_ascii_lowercase() == candidate_patroni_name {
        if !json {
            println!("{} is already the leader.", candidate.service_name.bold());
        } else {
            println!("{}", serde_json::json!({"alreadyLeader": true}));
        }
        return Ok(());
    }

    patroni::switchover(&probe_instance_id, &leader.name, &candidate_patroni_name)
        .await
        .context("Switchover request failed")?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "requestedLeader": candidate_patroni_name,
                "previousLeader": leader.name,
            })
        );
    } else {
        println!(
            "Requested switchover from {} to {}.",
            leader.name.bold(),
            candidate.service_name.bold()
        );
        println!(
            "Patroni is performing the failover -- run `railway postgres ha status` shortly to confirm the new leader."
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HaMemberOutput {
    service: ResourceRef,
    cluster_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live_lag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reachable: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HaStatusOutput {
    service: ResourceRef,
    environment: ResourceRef,
    root: ResourceRef,
    is_cluster: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    members: Vec<HaMemberOutput>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_top_level_verbs() {
        assert!(matches!(
            Args::parse_from(["ha", "status"]).command,
            Commands::Status
        ));
        assert!(matches!(
            Args::parse_from(["ha", "convert"]).command,
            Commands::Convert(_)
        ));
        assert!(matches!(
            Args::parse_from(["ha", "revert", "--yes"]).command,
            Commands::Revert(RevertArgs {
                yes: true,
                no_deploy: false
            })
        ));
    }

    #[test]
    fn parses_convert_counts() {
        let args = Args::parse_from([
            "ha",
            "convert",
            "--replicas",
            "2",
            "--coordinators",
            "3",
            "--edge",
            "1",
        ]);
        let Commands::Convert(convert) = args.command else {
            panic!("expected convert");
        };
        assert_eq!(convert.replicas, Some(2));
        assert_eq!(convert.coordinators, Some(3));
        assert_eq!(convert.edge, Some(1));
    }

    #[test]
    fn scale_requires_at_least_one_target() {
        assert!(Args::try_parse_from(["ha", "scale"]).is_err());
        let args = Args::parse_from(["ha", "scale", "--replicas", "3"]);
        assert!(matches!(
            args.command,
            Commands::Scale(ScaleArgs {
                replicas: Some(3),
                ..
            })
        ));
    }

    #[test]
    fn switchover_accepts_promote_alias_and_requires_to() {
        assert!(Args::try_parse_from(["ha", "switchover"]).is_err());
        let args = Args::parse_from(["ha", "switchover", "--to", "postgres-replica-1"]);
        let Commands::Switchover(switchover) = args.command else {
            panic!("expected switchover");
        };
        assert_eq!(switchover.to, "postgres-replica-1");

        let args = Args::parse_from(["ha", "promote", "--to", "postgres-replica-1"]);
        assert!(matches!(args.command, Commands::Switchover(_)));
    }

    #[test]
    fn switchover_accepts_short_yes_flag() {
        let args = Args::parse_from(["ha", "switchover", "--to", "postgres-replica-1", "-y"]);
        let Commands::Switchover(switchover) = args.command else {
            panic!("expected switchover");
        };
        assert!(switchover.yes);
    }

    #[test]
    fn scale_accepts_any_combination_of_targets_plus_flags() {
        let args = Args::parse_from([
            "ha",
            "scale",
            "--replicas",
            "3",
            "--coordinators",
            "5",
            "--edge",
            "2",
            "--no-deploy",
            "-y",
        ]);
        let Commands::Scale(scale) = args.command else {
            panic!("expected scale");
        };
        assert_eq!(scale.replicas, Some(3));
        assert_eq!(scale.coordinators, Some(5));
        assert_eq!(scale.edge, Some(2));
        assert!(scale.no_deploy);
        assert!(scale.yes);

        let args = Args::parse_from(["ha", "scale", "--coordinators", "3"]);
        assert!(matches!(
            args.command,
            Commands::Scale(ScaleArgs {
                replicas: None,
                coordinators: Some(3),
                edge: None,
                ..
            })
        ));

        let args = Args::parse_from(["ha", "scale", "--edge", "4"]);
        assert!(matches!(
            args.command,
            Commands::Scale(ScaleArgs { edge: Some(4), .. })
        ));
    }

    #[test]
    fn guardrail_blockers_lists_every_failing_check() {
        let state = PitrState {
            enabled: false,
            bucket_wired: false,
            minor_pinned: false,
            unsupported_image: true,
            has_start_command: true,
        };
        let blockers = guardrail_blockers(&state);
        assert_eq!(blockers.len(), 2);
    }

    #[test]
    fn data_node_members_excludes_internal_and_edge_roles() {
        use crate::controllers::postgres_plugins::HaMember;

        let ha_state = HaState {
            is_cluster: true,
            root_service_id: Some("root".to_string()),
            members: vec![
                HaMember {
                    service_id: "root".to_string(),
                    service_name: "db-prod".to_string(),
                    cluster_role: Some("root".to_string()),
                },
                HaMember {
                    service_id: "replica-1".to_string(),
                    service_name: "postgres-replica-1".to_string(),
                    cluster_role: Some("replica".to_string()),
                },
                HaMember {
                    service_id: "etcd-1".to_string(),
                    service_name: "etcd-1".to_string(),
                    cluster_role: Some("internal".to_string()),
                },
                HaMember {
                    service_id: "edge".to_string(),
                    service_name: "haproxy".to_string(),
                    cluster_role: Some("edge".to_string()),
                },
            ],
        };

        let data_nodes = data_node_members(&ha_state);
        assert_eq!(
            data_nodes,
            vec![
                ("root".to_string(), "db-prod".to_string()),
                ("replica-1".to_string(), "postgres-replica-1".to_string()),
            ]
        );
    }

    #[test]
    fn format_lag_renders_numbers_and_strings_without_quoting() {
        assert_eq!(format_lag(&serde_json::json!(0)), "0");
        assert_eq!(format_lag(&serde_json::json!("unknown")), "unknown");
    }
}
