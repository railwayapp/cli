//! The `ha` verb -- high-availability clustering, for every engine that
//! ships an HA companion template.
//!
//! What a cluster IS, and how it is driven, is read from the template's own
//! declarations rather than compiled in: `haTemplateCode` names the companion
//! to deploy, `haConversionConfig` bounds the topology the user may ask for,
//! and `clusterWiring` says how each node reports its role and how a
//! switchover is requested. The two switchover mechanisms the platform
//! defines -- a coordinator API the CLI speaks (Patroni), and the generic
//! per-node HTTP contract -- are selected from that same declaration, so a
//! topology that swaps coordinators is a template change.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;
use serde::Serialize;

use crate::controllers::{
    adoption_eligibility::{self, AdoptionRules, AdoptionTarget},
    cluster_probe,
    cluster_scale::{self, EdgeScaleSummary, ScaleClusterParams, ScaleDimensionSummary},
    config::{ClusterWiring, EnvironmentConfig, fetch_environment_config},
    database_engines::{DatabaseEngine, SwitchoverMechanism},
    database_plugins::{self, HaState},
    patroni,
    project::{ServiceContext, resolve_service_context},
    template_apply::{self, ApplyKind, ApplyTemplateParams, RevertTemplateParams},
};

use super::{
    ResourceRef, confirm_or_bail, print_field, resolve_root, service_name_map, status_label,
};

/// Manage high-availability clustering
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  ha status --service my-database\n  ha convert --service my-database --replicas 2\n  ha convert --service my-database --replicas 2 --coordinators 3 --edge 1\n  ha revert --service my-database --yes\n  ha scale --service my-database --replicas 3\n  ha switchover --service my-database --to my-database-replica-1\n\nAutomation notes:\n  Omitted --replicas/--coordinators/--edge on `convert` leave the template's authored count untouched.\n  Which roles a cluster has, and the counts each accepts, are declared by the engine's HA template -- `convert` reports the allowed values when a count is refused.\n  --coordinators applies only to clusters with a separate coordinator tier, and must be odd (consensus quorum).\n  Where the data nodes themselves carry the failover vote, their total must be odd and at least three, so --replicas must be even."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser)]
enum Commands {
    /// Show HA cluster status
    Status,

    /// Convert a standalone service into an HA cluster
    Convert(ConvertArgs),

    /// Revert an HA cluster back to a standalone service
    Revert(RevertArgs),

    /// Scale cluster replicas, coordinators, or edge nodes
    Scale(ScaleArgs),

    /// Promote a replica to primary (brief downtime)
    #[clap(visible_alias = "promote")]
    Switchover(SwitchoverArgs),
}

#[derive(Parser)]
struct ConvertArgs {
    /// Number of replicas (excluding the primary); omit to keep the template default
    #[clap(long, value_parser = clap::value_parser!(i64).range(0..))]
    replicas: Option<i64>,

    /// Number of coordinator/consensus nodes (e.g. etcd), for clusters that have a coordinator tier; must be odd
    #[clap(long, value_parser = clap::value_parser!(i64).range(1..))]
    coordinators: Option<i64>,

    /// Number of edge/load-balancer replicas (e.g. HAProxy); omit to keep the template default
    #[clap(long, value_parser = clap::value_parser!(i64).range(0..))]
    edge: Option<i64>,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Commit the config change without triggering deploys (applies on the next deploy)
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct RevertArgs {
    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Commit the config change without triggering deploys (applies on the next deploy)
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
    #[clap(long, value_parser = clap::value_parser!(i64).range(0..))]
    replicas: Option<i64>,

    /// Target coordinator/consensus node count (must stay odd)
    #[clap(long, value_parser = clap::value_parser!(i64).range(1..))]
    coordinators: Option<i64>,

    /// Target edge/load-balancer replica count
    #[clap(long, value_parser = clap::value_parser!(i64).range(0..))]
    edge: Option<i64>,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Commit the config change without triggering deploys (applies on the next deploy)
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct SwitchoverArgs {
    /// Service name or ID of the node to promote
    #[clap(long)]
    to: String,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,
}

pub async fn command(
    engine: &'static DatabaseEngine,
    args: Args,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    match args.command {
        Commands::Status => status(engine, project, service, environment, json).await,
        Commands::Convert(a) => convert(engine, project, service, environment, json, a).await,
        Commands::Revert(a) => revert(engine, project, service, environment, json, a).await,
        Commands::Scale(a) => scale(engine, project, service, environment, json, a).await,
        Commands::Switchover(a) => switchover(engine, project, service, environment, json, a).await,
    }
}

/// The members `ha revert` must still sweep after `templateRevert`, and the
/// services it must never touch.
///
/// templateRevert tears down the members the template itself tracks; a node
/// added later by LIVE scaling only belongs to the cluster through the
/// environment config -- the dashboard's revert finds those via canvas-group
/// membership, which the public API can't stamp. Reverting IS the
/// instruction to remove every member, so sweep any pre-revert member still
/// alive afterwards (volume first, then the service, same as scale-down),
/// matched against the pre-revert snapshot by id: the revert patch clears
/// parentServiceId on stragglers, so they can't be re-derived from the fresh
/// config.
///
/// The public patch path also DROPS parentServiceId on creation, so a node
/// added by live scaling is invisible to the membership snapshot too. Those
/// are swept as debris, but only on evidence that they are THIS cluster's
/// debris: a role-stamped, parentless service qualifies just when it runs an
/// image the engine's own companion publishes (`companion_repositories`).
/// Without that scope the scan was environment-wide -- every orphan of every
/// engine, so reverting a Redis cluster in a mixed environment would delete
/// the Postgres cluster's live-scaled replicas, and a role stamp was the only
/// thing standing between an unrelated service and deletion. An empty
/// `companion_repositories` (the record was unreachable) skips the debris
/// scan entirely: the snapshot members are the part we can still prove.
///
/// The pooler is excluded from BOTH paths where the engine declares one. It
/// hangs off the root exactly like a member (parent = root, role "edge"), so
/// the membership walk picks it up -- but it belongs to the pooling feature,
/// not to the HA conversion: it may well predate the convert, and reverting
/// the cluster to standalone is not an instruction to remove pooling. `pool
/// remove` is. Deleting it here silently destroyed a customer-configured
/// pooler on every revert of a pooled cluster.
fn revert_sweep_targets(
    engine: &DatabaseEngine,
    pre_revert_members: &[(String, String)],
    config: &EnvironmentConfig,
    root_id: &str,
    names: &BTreeMap<String, String>,
    companion_repositories: &[String],
) -> Vec<(String, String)> {
    let is_pooler = |id: &str| {
        engine.pooling.is_some_and(|pooling| {
            config
                .services
                .get(id)
                .is_some_and(|service| database_plugins::is_pooler_service(service, &pooling))
        })
    };
    let mut leftovers: Vec<(String, String)> = pre_revert_members
        .iter()
        .filter(|(member_id, _)| {
            config
                .services
                .get(member_id)
                .is_some_and(|service| !service.is_deleted.unwrap_or(false))
                && !is_pooler(member_id)
        })
        .cloned()
        .collect();

    if companion_repositories.is_empty() {
        return leftovers;
    }

    for (id, service) in &config.services {
        let runs_a_companion_image = adoption_eligibility::image_is_from_repository(
            service.source.as_ref().and_then(|s| s.image.as_deref()),
            companion_repositories,
        );
        let orphaned_member = matches!(
            service.cluster_role.as_deref(),
            Some("replica") | Some("internal") | Some("edge")
        ) && service.parent_service_id.is_none()
            && runs_a_companion_image
            && !service.is_deleted.unwrap_or(false)
            && id.as_str() != root_id
            && !is_pooler(id)
            && !leftovers.iter().any(|(seen, _)| seen == id);
        if orphaned_member {
            leftovers.push((
                id.clone(),
                names.get(id).cloned().unwrap_or_else(|| id.clone()),
            ));
        }
    }
    leftovers
}

/// The root's declared `clusterWiring` -- how this cluster reports node health
/// and role, and how a switchover is asked for. Absent on legacy clusters
/// converted before templates carried it; callers degrade to "no live signal"
/// rather than guessing.
fn cluster_wiring(config: &EnvironmentConfig, root_id: &str) -> Option<ClusterWiring> {
    config
        .services
        .get(root_id)
        .and_then(|root| root.cluster_wiring.clone())
}

/// Members whose live role/state actually matters for `status`, `switchover`
/// and `revert`'s precheck -- the data nodes (root + replicas). Coordinator
/// and edge members answer to none of the probes below. Each entry is
/// `(service_id, node_name)`, the name the probe join uses.
fn data_node_members(ha_state: &HaState, config: &EnvironmentConfig) -> Vec<(String, String)> {
    let root_id = ha_state.root_service_id.clone().unwrap_or_default();
    ha_state
        .members
        .iter()
        .filter(|m| matches!(m.cluster_role.as_deref(), Some("root") | Some("replica")))
        .map(|m| {
            (
                m.service_id.clone(),
                database_plugins::member_identity_name(
                    config,
                    &root_id,
                    &m.service_id,
                    &m.service_name,
                ),
            )
        })
        .collect()
}

/// One data node's live view, however its topology reports it.
///
/// The two mechanisms answer different amounts: a coordinator API returns a
/// cluster-wide member list (role, state, replication lag), while the generic
/// per-node contract answers only "am I the primary" and "am I healthy". The
/// shape is the union, and every field an engine's mechanism cannot supply
/// stays `None` -- never a fabricated default, which would read as fact.
#[derive(Debug, Clone, Default)]
struct LiveMember {
    reachable: bool,
    is_primary: Option<bool>,
    role: Option<String>,
    state: Option<String>,
    lag: Option<String>,
}

/// Probes every data node through whichever mechanism this cluster declares.
///
/// Best-effort by contract: a cluster nobody can reach yields an empty map and
/// the caller reports "unknown", rather than failing a read-only command.
async fn probe_data_nodes(
    engine: &DatabaseEngine,
    ctx: &ServiceContext,
    config: &EnvironmentConfig,
    ha_state: &HaState,
) -> BTreeMap<String, LiveMember> {
    let members = data_node_members(ha_state, config);
    if members.is_empty() {
        return BTreeMap::new();
    }
    let root_id = ha_state.root_service_id.clone().unwrap_or_default();
    let wiring = cluster_wiring(config, &root_id);

    match engine.ha.map(|ha| ha.switchover) {
        Some(SwitchoverMechanism::Patroni) => match patroni::probe_members(ctx, &members).await {
            Ok(probes) => probes
                .into_iter()
                .map(|(service_id, probe)| {
                    let view = probe.self_view;
                    (
                        service_id,
                        LiveMember {
                            reachable: probe.reachable,
                            is_primary: view.as_ref().map(|v| v.role == "leader"),
                            role: view
                                .as_ref()
                                .map(|v| v.role.clone())
                                .filter(|s| !s.is_empty()),
                            state: view
                                .as_ref()
                                .map(|v| v.state.clone())
                                .filter(|s| !s.is_empty()),
                            lag: view.as_ref().and_then(|v| v.lag.as_ref()).map(format_lag),
                        },
                    )
                })
                .collect(),
            Err(err) => {
                eprintln!("Warning: could not probe live cluster status: {err:#}");
                BTreeMap::new()
            }
        },
        Some(SwitchoverMechanism::DeclaredHttp) => {
            let Some(wiring) = wiring else {
                return BTreeMap::new();
            };
            let service_ids: Vec<String> = members.iter().map(|(id, _)| id.clone()).collect();
            let instance_ids = match patroni::resolve_instance_ids(ctx, &service_ids).await {
                Ok(ids) => ids,
                Err(err) => {
                    eprintln!("Warning: could not resolve live cluster instances: {err:#}");
                    return BTreeMap::new();
                }
            };
            cluster_probe::probe_nodes(
                &instance_ids,
                wiring.data_node_health_check.as_ref(),
                wiring.data_node_role_check.as_ref(),
            )
            .await
            .into_iter()
            .map(|(service_id, status)| {
                (
                    service_id,
                    LiveMember {
                        reachable: status.reachable,
                        is_primary: status.is_primary,
                        // This contract reports a verdict, not a role name:
                        // deriving the label here keeps the display uniform
                        // without inventing detail the node never sent.
                        role: status
                            .is_primary
                            .map(|primary| if primary { "primary" } else { "replica" }.to_string()),
                        state: status.healthy.map(|healthy| {
                            if healthy { "healthy" } else { "unhealthy" }.to_string()
                        }),
                        lag: None,
                    },
                )
            })
            .collect()
        }
        None => BTreeMap::new(),
    }
}

async fn status(
    engine: &'static DatabaseEngine,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    print_status(engine, &ctx, &config, json, true).await
}

/// `include_live == false` skips the per-member probe -- used right after
/// `convert`/`revert`/`scale`, where brand-new (or just-deleted) members
/// haven't rolled out yet, so probing them would only add seconds of
/// "unreachable" noise.
async fn print_status(
    engine: &'static DatabaseEngine,
    ctx: &ServiceContext,
    config: &EnvironmentConfig,
    json: bool,
    include_live: bool,
) -> Result<()> {
    let root = resolve_root(ctx, config);
    let names = service_name_map(ctx);
    let ha_state = database_plugins::compute_ha_state(config, &root.root_id, &names, engine);

    let live = if include_live && ha_state.is_cluster {
        probe_data_nodes(engine, ctx, config, &ha_state).await
    } else {
        BTreeMap::new()
    };

    let members: Vec<HaMemberOutput> = ha_state
        .members
        .iter()
        .map(|m| {
            let probe = live.get(&m.service_id);
            HaMemberOutput {
                service: ResourceRef {
                    id: m.service_id.clone(),
                    name: m.service_name.clone(),
                },
                cluster_role: m.cluster_role.clone(),
                live_role: probe.and_then(|p| p.role.clone()),
                live_state: probe.and_then(|p| p.state.clone()),
                live_lag: probe.and_then(|p| p.lag.clone()),
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
        print_ha_status(engine, &output);
    }
    Ok(())
}

fn format_lag(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn print_ha_status(engine: &DatabaseEngine, output: &HaStatusOutput) {
    println!("{}", "High availability".bold());
    println!();
    print_field("Engine:", &engine.display_name);
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

/// Validates one requested count against the companion's declared selector
/// for that role.
///
/// A role the companion does not declare at all does not exist in this
/// topology -- Sentinel and Group Replication colocate their coordinator on
/// the data nodes, so asking for coordinators there is not a count to clamp
/// but a request the cluster has no shape for. Refuse, naming what the
/// template does offer, rather than silently ignoring the flag.
fn validate_role_count(
    rules: &AdoptionRules,
    role: &str,
    flag: &str,
    requested: i64,
    engine: &DatabaseEngine,
) -> Result<()> {
    // A companion that declares no conversion config at all gives no local
    // bounds to check against; the server-side gate still applies.
    if rules.role_options.is_empty() {
        return Ok(());
    }

    let Some(options) = rules.role_options.get(role) else {
        bail!(
            "{} high-availability clusters have no {role} nodes, so {flag} does not apply here.",
            engine.display_name
        );
    };

    if !options.is_empty() && !options.contains(&requested) {
        bail!(
            "{flag} must be one of {} for a {} cluster (got {requested}).",
            options
                .iter()
                .map(|o| o.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            engine.display_name
        );
    }
    Ok(())
}

async fn convert(
    engine: &'static DatabaseEngine,
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
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = database_plugins::compute_ha_state(&config, &root.root_id, &names, engine);

    if ha_state.is_cluster {
        bail!("{} is already an HA cluster.", root.root_name);
    }

    let target_service = config.services.get(&root.root_id).with_context(|| {
        format!(
            "Service \"{}\" not found in environment config",
            root.root_name
        )
    })?;

    // The companion the service's own origin template names, so a service
    // provisioned from a first-party template converts into the companion that
    // template was authored against rather than a code-side assumption.
    let template_code = engine
        .ha_template_code_for(target_service.ha_template_code.as_deref())
        .with_context(|| {
            format!(
                "{} has no high-availability companion template.",
                engine.display_name
            )
        })?;

    // Everything the conversion is bounded by -- eligible images, supported
    // majors, and the counts each role accepts -- is declared by the COMPANION
    // being applied, which is the same record the server-side gate reads.
    // Pre-flighting it here means an ineligible image or an impossible
    // topology is refused with its remedy before the prompt, rather than after
    // the user has already confirmed.
    let rules = template_apply::fetch_adoption_rules(&ctx, &template_code).await?;

    if let Some(replicas) = args.replicas {
        validate_role_count(&rules, "replica", "--replicas", replicas, engine)?;
    }
    if let Some(coordinators) = args.coordinators {
        validate_role_count(&rules, "internal", "--coordinators", coordinators, engine)?;
    }
    if let Some(edge) = args.edge {
        validate_role_count(&rules, "edge", "--edge", edge, engine)?;
    }

    let blockers = rules.blockers(&AdoptionTarget {
        image: target_service
            .source
            .as_ref()
            .and_then(|s| s.image.as_deref()),
        has_start_command: target_service
            .deploy
            .as_ref()
            .and_then(|d| d.start_command.as_deref())
            .is_some_and(|c| !c.trim().is_empty()),
    });
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

    // Live volume-instance id (NOT the config volumeMounts key, which is the
    // volume id) for the pre-conversion safety backup. Best-effort: the backup
    // itself is best-effort, so failing to resolve just skips it.
    let volume_instance_id = crate::controllers::project::get_environment_instances(
        &ctx.client,
        &ctx.configs,
        &ctx.project_id,
        &ctx.environment_id,
    )
    .await
    .ok()
    .and_then(|instances| {
        instances
            .volume_instances
            .iter()
            .find(|edge| edge.node.service_id.as_deref() == Some(root.root_id.as_str()))
            .map(|edge| edge.node.id.clone())
    });
    let result = template_apply::apply_composable_template(
        &ctx,
        ApplyTemplateParams {
            template_code,
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

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    if !json {
        let verb = if result.deployed {
            "Converted and deployed"
        } else {
            "Converted (deploys skipped -- applies on the next deploy)"
        };
        println!(
            "{verb} {} to an HA cluster in environment {} (project {}).",
            root.root_name.bold(),
            ctx.environment_name.bold(),
            result.project_id
        );
    }
    print_status(engine, &ctx, &config, json, false).await
}

async fn revert(
    engine: &'static DatabaseEngine,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: RevertArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = database_plugins::compute_ha_state(&config, &root.root_id, &names, engine);

    let template_code = engine
        .ha
        .map(|ha| ha.template_code.to_string())
        .with_context(|| {
            format!(
                "{} has no high-availability companion template.",
                engine.display_name
            )
        })?;

    // What this engine's companion actually deploys, so the sweep below can
    // recognize its own debris instead of every orphan in the environment.
    let companion_repositories =
        template_apply::fetch_companion_image_repositories(&ctx, &template_code).await;
    if companion_repositories.is_empty() {
        eprintln!(
            "Warning: could not read the {template_code} template's images, so members left behind by an earlier scale-up cannot be told apart from unrelated services and will be left in place. Re-run once the template is reachable, or remove them from the dashboard."
        );
    }

    if !ha_state.is_cluster {
        // A revert that died mid-sweep leaves no cluster to detect --
        // templateRevert already cleared the root's HA marker and the
        // survivors' parent links -- but the members it never got to are
        // still deployed and still billing, stamped with a cluster role and
        // no parent. Bailing here would strand them with no command able to
        // remove them (this is exactly what re-running `ha revert` after a
        // transient delete failure used to do), so finish the sweep instead.
        let leftovers = revert_sweep_targets(
            engine,
            &[],
            &config,
            &root.root_id,
            &names,
            &companion_repositories,
        );
        if leftovers.is_empty() {
            bail!("{} is not an HA cluster.", root.root_name);
        }
        // Name them. This path deletes services the user never listed, on
        // evidence they cannot see, so the prompt has to show its work.
        if !confirm_or_bail(
            &format!(
                "{} is already standalone, but {} cluster member(s) from an earlier revert are still deployed: {}. Remove them?",
                root.root_name.red(),
                leftovers.len(),
                leftovers
                    .iter()
                    .map(|(_, name)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            args.yes,
        )? {
            println!("Cancelled.");
            return Ok(());
        }
        for (member_id, member_name) in &leftovers {
            if !json {
                println!(
                    "Removing cluster member {} left behind by an earlier revert...",
                    member_name.bold()
                );
            }
            cluster_scale::delete_member(&ctx, &config, member_id)
                .await
                .with_context(|| format!("Failed to remove cluster member {member_name}"))?;
        }
        let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
            .await?
            .config;
        if !json {
            println!(
                "Removed {} cluster member(s) left behind by an earlier revert of {}.",
                leftovers.len(),
                root.root_name.bold()
            );
        }
        return print_status(engine, &ctx, &config, json, false).await;
    }

    // Live precheck: revert is only safe while the root is the current
    // primary. A stale former primary still running as a replica would
    // silently lose whatever writes landed on the real one once the cluster is
    // torn down. Degrades to a warning (rather than blocking) when no member
    // is reachable at all -- refusing to tear down a cluster nobody can reach
    // would leave the customer with no way out.
    let live = probe_data_nodes(engine, &ctx, &config, &ha_state).await;
    let root_is_primary = live
        .get(&root.root_id)
        .and_then(|m| m.is_primary)
        .unwrap_or(false);
    if !root_is_primary {
        let any_reachable = live.values().any(|m| m.reachable);
        if any_reachable {
            let current_primary = live
                .iter()
                .find(|(_, m)| m.is_primary == Some(true))
                .and_then(|(id, _)| {
                    ha_state
                        .members
                        .iter()
                        .find(|m| &m.service_id == id)
                        .map(|m| m.service_name.clone())
                });
            bail!(
                "{} is not currently the primary{}. Run `railway {} ha switchover --to {}` first, then revert.",
                root.root_name,
                current_primary
                    .map(|p| format!(" (current primary: {p})"))
                    .unwrap_or_default(),
                engine.key,
                root.root_name,
            );
        }
        eprintln!(
            "Warning: could not reach any cluster member to verify {} is the current primary before reverting. Proceeding anyway.",
            root.root_name
        );
    }

    if !confirm_or_bail(
        &format!(
            "Revert {} to a standalone {}? Connection endpoints will change and active connections will drop.",
            root.root_name.red(),
            engine.display_name
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    // Snapshot the membership BEFORE reverting: the revert patch clears
    // parentServiceId on survivors it doesn't delete, so a post-revert
    // parent-based scan can't find them anymore.
    let pre_revert_members: Vec<(String, String)> = ha_state
        .members
        .iter()
        .filter(|m| m.service_id != root.root_id)
        .map(|m| (m.service_id.clone(), m.service_name.clone()))
        .collect();

    let result = template_apply::revert_template(
        &ctx,
        RevertTemplateParams {
            template_code,
            root_service_id: root.root_id.clone(),
            auto_deploy: !args.no_deploy,
        },
    )
    .await
    .context("Failed to revert HA cluster")?;

    // templateRevert COMMITS the reverted config, but the commit only
    // enqueues the apply -- until it lands, a fresh config read still shows
    // the root HA-enabled. Wait it out (bounded) before sweeping: a sweep
    // failure past this point is then retryable, because the re-run's
    // `is_cluster` check sees the applied (standalone) config and takes the
    // resume path. Without the wait, a retry arriving inside the apply
    // window took the NORMAL path instead and died in the leader precheck,
    // probing a half-torn-down cluster and advising an impossible
    // switchover. On timeout, proceed -- the sweep itself only needs the
    // pre-revert snapshot, and stopping here would leave every member
    // running.
    let mut config;
    let apply_deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    loop {
        config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
            .await?
            .config;
        let still_ha = database_plugins::compute_ha_state(
            &config,
            &root.root_id,
            &service_name_map(&ctx),
            engine,
        )
        .is_cluster;
        if !still_ha {
            break;
        }
        if std::time::Instant::now() >= apply_deadline {
            eprintln!(
                "Warning: the reverted configuration has not finished applying yet; sweeping remaining members anyway."
            );
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }

    let names = service_name_map(&ctx);
    let leftovers = revert_sweep_targets(
        engine,
        &pre_revert_members,
        &config,
        &root.root_id,
        &names,
        &companion_repositories,
    );
    for (member_id, member_name) in &leftovers {
        if !json {
            println!(
                "Removing live-scaled cluster member {} left behind by the template revert...",
                member_name.bold()
            );
        }
        cluster_scale::delete_member(&ctx, &config, member_id)
            .await
            .with_context(|| format!("Failed to remove cluster member {member_name}"))?;
    }
    let config = if leftovers.is_empty() {
        config
    } else {
        fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
            .await?
            .config
    };

    if !json {
        let verb = if result.deployed {
            "Reverted and deployed"
        } else {
            "Reverted (deploys skipped -- applies on the next deploy)"
        };
        println!(
            "{verb} {} to a standalone {} in environment {} (project {}).",
            root.root_name.bold(),
            engine.display_name,
            ctx.environment_name.bold(),
            result.project_id
        );
    }
    print_status(engine, &ctx, &config, json, false).await
}

async fn scale(
    engine: &'static DatabaseEngine,
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
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = database_plugins::compute_ha_state(&config, &root.root_id, &names, engine);

    if !ha_state.is_cluster {
        bail!(
            "{} is not an HA cluster. Run `ha convert` first.",
            root.root_name
        );
    }

    // A cluster with no coordinator tier has no coordinators to scale. The
    // members are the truth here rather than the conversion config, which
    // describes the shape the service was converted INTO and may be absent on
    // a legacy cluster.
    if args.coordinators.is_some()
        && !ha_state
            .members
            .iter()
            .any(|m| m.cluster_role.as_deref() == Some("internal"))
    {
        bail!(
            "This {} cluster has no coordinator nodes, so --coordinators does not apply.",
            engine.display_name
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

    // When replicas are being REMOVED, ask the cluster which node currently
    // holds the primary role, so scale-down never deletes the acting primary:
    // deletion order is by node number, and after a failover the primary can
    // be ANY replica, whatever its number. Degrades to a warning when no
    // member answers -- the same posture as revert's primacy precheck.
    let current_replicas = ha_state
        .members
        .iter()
        .filter(|m| m.cluster_role.as_deref() == Some("replica"))
        .count() as i64;
    let live_primary_id = match args.replicas {
        Some(target) if target < current_replicas => {
            let live = probe_data_nodes(engine, &ctx, &config, &ha_state).await;
            let primary = live
                .iter()
                .find(|(_, member)| member.is_primary == Some(true))
                .map(|(id, _)| id.clone());
            if primary.is_none() {
                eprintln!(
                    "Warning: could not determine {}'s current primary before scaling down; removing the highest-numbered replica(s) by name alone.",
                    root.root_name
                );
            }
            primary
        }
        _ => None,
    };

    let result = cluster_scale::scale_cluster(
        &ctx,
        engine,
        &root.root_id,
        &root.root_name,
        &names,
        ScaleClusterParams {
            replicas: args.replicas,
            coordinators: args.coordinators,
            edge: args.edge,
            auto_deploy: !args.no_deploy,
            live_primary_id,
        },
    )
    .await
    .context("Failed to scale HA cluster")?;

    if !json {
        print_scale_result(&root.root_name, &result);
    }

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    print_status(engine, &ctx, &config, json, false).await
}

fn print_scale_result(root_name: &str, result: &cluster_scale::ScaleClusterResult) {
    let verb = if result.deployed {
        "Scaled and deployed"
    } else {
        "Scaled (deploys skipped -- applies on the next deploy)"
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

/// Resolves the SSH key the live probes need, failing with the setup recipe
/// rather than a misleading "could not reach the cluster". Interactive runs may
/// register a key on the spot; `--yes` runs must never block on a prompt.
async fn preflight_ssh(ctx: &ServiceContext, yes: bool) -> Result<()> {
    let preflight = if yes {
        crate::commands::ssh::native::ensure_ssh_key_noninteractive(&ctx.client, &ctx.configs).await
    } else {
        crate::commands::ssh::native::ensure_ssh_key_quiet(&ctx.client, &ctx.configs).await
    };
    if let Err(e) = preflight {
        bail!(
            "Switchover is driven over SSH (ssh <instance>@ssh.railway.com), and no usable \
             SSH key is available: {e:#}"
        );
    }
    Ok(())
}

async fn switchover(
    engine: &'static DatabaseEngine,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: SwitchoverArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = database_plugins::compute_ha_state(&config, &root.root_id, &names, engine);

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
            "Switchover target must be a {} data node (root or replica), not \"{}\".",
            engine.display_name,
            candidate.cluster_role.as_deref().unwrap_or("unknown")
        );
    }

    if !confirm_or_bail(
        &format!(
            "Promote {} to primary? This causes a brief write downtime while the cluster fails over.",
            candidate.service_name.yellow()
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    preflight_ssh(&ctx, args.yes).await?;

    match engine.ha.map(|ha| ha.switchover) {
        Some(SwitchoverMechanism::Patroni) => {
            switchover_via_patroni(&ctx, &config, &root, &ha_state, candidate, json).await
        }
        Some(SwitchoverMechanism::DeclaredHttp) => {
            switchover_via_declared_endpoint(&ctx, &config, &root, candidate, json).await
        }
        None => bail!(
            "{} has no high-availability companion template.",
            engine.display_name
        ),
    }
}

/// Switchover through a coordinator API: one member's view names the whole
/// cluster, so the request is addressed from whichever member answers.
async fn switchover_via_patroni(
    ctx: &ServiceContext,
    config: &EnvironmentConfig,
    root: &super::RootContext,
    ha_state: &HaState,
    candidate: &database_plugins::HaMember,
    json: bool,
) -> Result<()> {
    let data_nodes = data_node_members(ha_state, config);
    let instance_ids = patroni::resolve_instance_ids(
        ctx,
        &data_nodes
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
    )
    .await
    .context("Failed to resolve live cluster member instances")?;

    let probe_targets: Vec<String> = instance_ids.values().cloned().collect();
    let (probe_instance_id, cluster_members) = match patroni::probe_any(&probe_targets).await {
        Ok(hit) => hit,
        Err(failures) => {
            let detail = failures
                .iter()
                .map(|(id, err)| format!("  {id}: {err}"))
                .collect::<Vec<_>>()
                .join("\n");
            bail!(
                "Could not reach any cluster member's coordinator API to determine the current \
                 primary. Per-member errors:\n{detail}"
            );
        }
    };

    let leader = cluster_members
        .iter()
        .find(|m| m.role == "leader")
        .context("The cluster's coordinator did not report a current primary")?;

    let candidate_node_name = database_plugins::member_identity_name(
        config,
        &root.root_id,
        &candidate.service_id,
        &candidate.service_name,
    );
    if !cluster_members
        .iter()
        .any(|m| m.name.to_ascii_lowercase() == candidate_node_name)
    {
        bail!(
            "\"{}\" is not currently a recognized cluster member.",
            candidate.service_name
        );
    }

    if leader.name.to_ascii_lowercase() == candidate_node_name {
        return report_already_primary(&candidate.service_name, json);
    }

    patroni::switchover(&probe_instance_id, &leader.name, &candidate_node_name)
        .await
        .context("Switchover request failed")?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "requestedLeader": candidate_node_name,
                "previousLeader": leader.name,
            }))?
        );
    } else {
        println!(
            "Requested switchover from {} to {}.",
            leader.name.bold(),
            candidate.service_name.bold()
        );
        println!(
            "The cluster is performing the failover -- run `ha status` shortly to confirm the new primary."
        );
    }
    Ok(())
}

/// Switchover through the generic per-node contract: the request goes to the
/// CANDIDATE's own container, asking its colocated coordinator to make that
/// node the primary. There is no cluster-wide endpoint to address, and the
/// response only says the handoff was accepted -- the role probe flipping is
/// what confirms it.
async fn switchover_via_declared_endpoint(
    ctx: &ServiceContext,
    config: &EnvironmentConfig,
    root: &super::RootContext,
    candidate: &database_plugins::HaMember,
    json: bool,
) -> Result<()> {
    let wiring = cluster_wiring(config, &root.root_id).with_context(|| {
        format!(
            "This cluster declares no wiring, so there is no way to ask {} to become the primary.",
            candidate.service_name
        )
    })?;
    let endpoint = cluster_probe::resolve(wiring.data_node_switchover.as_ref()).with_context(
        || "This cluster does not offer a switchover endpoint. Fail over through the dashboard.",
    )?;

    let instance_ids =
        patroni::resolve_instance_ids(ctx, std::slice::from_ref(&candidate.service_id))
            .await
            .context("Failed to resolve the target's live instance")?;
    let instance_id = instance_ids.get(&candidate.service_id).with_context(|| {
        format!(
            "{} has no running deployment to promote.",
            candidate.service_name
        )
    })?;

    // Asking a node that already holds the primary role to take it again is a
    // no-op at best and an unnecessary election at worst.
    if let Some(role_endpoint) = cluster_probe::resolve(wiring.data_node_role_check.as_ref()) {
        let mut one = BTreeMap::new();
        one.insert(candidate.service_id.clone(), instance_id.clone());
        let statuses =
            cluster_probe::probe_nodes(&one, None, wiring.data_node_role_check.as_ref()).await;
        let _ = role_endpoint;
        if statuses
            .get(&candidate.service_id)
            .and_then(|s| s.is_primary)
            == Some(true)
        {
            return report_already_primary(&candidate.service_name, json);
        }
    }

    cluster_probe::request_switchover(instance_id, &endpoint)
        .await
        .context("Switchover request failed")?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "requestedPrimary": candidate.service_name,
            }))?
        );
    } else {
        println!(
            "Requested that {} become the primary.",
            candidate.service_name.bold()
        );
        println!(
            "The cluster is performing the failover -- run `ha status` shortly to confirm the new primary."
        );
    }
    Ok(())
}

fn report_already_primary(candidate_name: &str, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"alreadyPrimary": true}))?
        );
    } else {
        println!("{} is already the primary.", candidate_name.bold());
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
    use crate::controllers::config::{ServiceInstance, Variable};
    use crate::controllers::database_engines::{MYSQL, POSTGRES, REDIS};
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
    fn convert_and_scale_reject_negative_counts_at_parse_time() {
        assert!(Args::try_parse_from(["ha", "convert", "--replicas=-1"]).is_err());
        assert!(Args::try_parse_from(["ha", "convert", "--edge=-2"]).is_err());
        assert!(Args::try_parse_from(["ha", "scale", "--replicas=-1"]).is_err());
        assert!(Args::try_parse_from(["ha", "scale", "--edge=-1"]).is_err());
        // Zero is a legal target (remove all replicas / scale edge to zero).
        assert!(Args::try_parse_from(["ha", "scale", "--replicas=0"]).is_ok());
        assert!(Args::try_parse_from(["ha", "scale", "--edge=0"]).is_ok());
    }

    #[test]
    fn coordinators_reject_zero_and_negatives_at_parse_time() {
        assert!(Args::try_parse_from(["ha", "convert", "--coordinators=0"]).is_err());
        assert!(Args::try_parse_from(["ha", "convert", "--coordinators=-1"]).is_err());
        assert!(Args::try_parse_from(["ha", "scale", "--coordinators=0"]).is_err());
        assert!(Args::try_parse_from(["ha", "scale", "--coordinators=-3"]).is_err());
        // Parity is still enforced at runtime, not parse time.
        assert!(Args::try_parse_from(["ha", "scale", "--coordinators=4"]).is_ok());
        assert!(cluster_scale::validate_odd_coordinator_count(4).is_err());
        assert!(cluster_scale::validate_odd_coordinator_count(5).is_ok());
    }

    /// The rules a companion template yields, built from the shape its record
    /// really has -- these mirror redis-ha/mysql-ha and postgres-ha.
    fn rules_from(conversion: serde_json::Value) -> AdoptionRules {
        crate::controllers::adoption_eligibility::rules_from_template(&serde_json::json!({
            "services": { "root": { "clusterRole": "root", "haConversionConfig": conversion } }
        }))
    }

    /// redis-ha's and mysql-ha's real shape: replicas and edge, and no
    /// coordinator tier at all.
    fn colocated_rules() -> AdoptionRules {
        rules_from(serde_json::json!({
            "replica": { "label": "Replicas", "options": [2, 4, 6, 8] },
            "internal": null,
            "edge": { "label": "Reverse Proxies", "options": [1, 2] }
        }))
    }

    /// postgres-ha's real shape: all three roles.
    fn coordinated_rules() -> AdoptionRules {
        rules_from(serde_json::json!({
            "replica": { "label": "Replicas", "options": [2, 3, 4, 5, 6, 7] },
            "internal": { "label": "Coordinator Nodes", "options": [3, 5, 7, 9] },
            "edge": { "label": "Reverse Proxy", "options": [2, 3, 4, 5] }
        }))
    }

    #[test]
    fn coordinators_are_refused_for_a_topology_that_has_no_coordinator_tier() {
        let err = validate_role_count(&colocated_rules(), "internal", "--coordinators", 3, &REDIS)
            .unwrap_err()
            .to_string();
        // Naming the role the cluster lacks beats silently ignoring the flag
        // and converting into a shape the user did not ask for.
        assert!(err.contains("no internal nodes"));
        assert!(err.contains("--coordinators"));

        // The same flag is fine where the tier exists.
        assert!(
            validate_role_count(
                &coordinated_rules(),
                "internal",
                "--coordinators",
                3,
                &POSTGRES
            )
            .is_ok()
        );
    }

    #[test]
    fn role_counts_are_bounded_by_the_companions_own_options() {
        let colocated = colocated_rules();
        for allowed in [2, 4, 6, 8] {
            assert!(
                validate_role_count(&colocated, "replica", "--replicas", allowed, &MYSQL).is_ok()
            );
        }

        // An odd replica count would leave an even number of data nodes,
        // which cannot hold a quorum -- the template declares only even
        // options for exactly that reason.
        let err = validate_role_count(&colocated, "replica", "--replicas", 3, &MYSQL)
            .unwrap_err()
            .to_string();
        assert!(err.contains("2, 4, 6, 8"));
        assert!(err.contains("MySQL"));

        // Postgres declares a different set and is bounded by its own.
        let coordinated = coordinated_rules();
        assert!(validate_role_count(&coordinated, "replica", "--replicas", 3, &POSTGRES).is_ok());
        assert!(
            validate_role_count(&coordinated, "internal", "--coordinators", 4, &POSTGRES).is_err()
        );
    }

    #[test]
    fn a_companion_declaring_no_conversion_config_is_left_to_the_server_gate() {
        // Inventing bounds for a companion that declares none would refuse
        // conversions the backend would have accepted.
        let rules = AdoptionRules::default();
        for role in ["replica", "internal", "edge"] {
            assert!(validate_role_count(&rules, role, "--flag", 99, &POSTGRES).is_ok());
        }
    }

    #[test]
    fn a_role_declared_without_options_accepts_any_count() {
        let rules = rules_from(serde_json::json!({
            "replica": { "label": "Replicas" },
            "edge": { "label": "Proxies", "options": [1, 2] }
        }));
        assert!(validate_role_count(&rules, "replica", "--replicas", 42, &POSTGRES).is_ok());
        assert!(validate_role_count(&rules, "edge", "--edge", 42, &POSTGRES).is_err());
    }

    #[test]
    fn data_node_members_excludes_coordinator_and_edge_roles() {
        use crate::controllers::database_plugins::HaMember;

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

        // With no identity variable declared, names fall back to the
        // lowercased service names.
        let config = EnvironmentConfig::default();
        let data_nodes = data_node_members(&ha_state, &config);
        assert_eq!(
            data_nodes,
            vec![
                ("root".to_string(), "db-prod".to_string()),
                ("replica-1".to_string(), "postgres-replica-1".to_string()),
            ]
        );

        // Where one IS declared it wins: the HA template authors the root's
        // node name, which never matches the adopted service's own.
        let mut config = EnvironmentConfig::default();
        let mut root = ServiceInstance {
            cluster_wiring: Some(ClusterWiring {
                replica_node_name_variable: Some("PATRONI_NAME".to_string()),
                ..ClusterWiring::default()
            }),
            ..ServiceInstance::default()
        };
        root.variables.insert(
            "PATRONI_NAME".to_string(),
            Some(Variable {
                value: Some("postgres-1".to_string()),
                ..Variable::default()
            }),
        );
        config.services.insert("root".to_string(), root);
        let data_nodes = data_node_members(&ha_state, &config);
        assert_eq!(
            data_nodes[0],
            ("root".to_string(), "postgres-1".to_string())
        );
    }

    #[test]
    fn format_lag_renders_numbers_and_strings_without_quoting() {
        assert_eq!(format_lag(&serde_json::json!(0)), "0");
        assert_eq!(format_lag(&serde_json::json!("unknown")), "unknown");
    }

    /// The repositories `postgres-ha` declares across its slots, as
    /// `companion_image_repositories` reads them off the live record.
    fn postgres_ha_repositories() -> Vec<String> {
        [
            "ghcr.io/railwayapp-templates/postgres-ha/etcd",
            "ghcr.io/railwayapp-templates/postgres-ha/haproxy",
            "ghcr.io/railwayapp-templates/postgres-ha/postgres-patroni",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn redis_ha_repositories() -> Vec<String> {
        [
            "ghcr.io/railwayapp-templates/redis-ha/haproxy",
            "ghcr.io/railwayapp-templates/redis-ha/redis-sentinel",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    /// A mixed environment mid-revert: a Postgres HA cluster (whose pooler
    /// and live-scaled replica lost their parent links to the revert patch,
    /// and whose haproxy was orphaned earlier by the parent-dropping public
    /// patch) sitting beside an unrelated app service that happens to carry
    /// an edge role.
    fn mixed_environment() -> (EnvironmentConfig, BTreeMap<String, String>) {
        use crate::controllers::config::{ServiceInstance, ServiceSource};

        let service =
            |parent: Option<&str>, role: Option<&str>, image: Option<&str>| ServiceInstance {
                parent_service_id: parent.map(str::to_string),
                cluster_role: role.map(str::to_string),
                source: image.map(|i| ServiceSource {
                    image: Some(i.to_string()),
                    ..ServiceSource::default()
                }),
                ..ServiceInstance::default()
            };

        let mut config = EnvironmentConfig::default();
        config.services.insert(
            "root".to_string(),
            service(
                None,
                Some("root"),
                Some("ghcr.io/railwayapp-templates/postgres-ha/postgres-patroni:16"),
            ),
        );
        config.services.insert(
            "pooler".to_string(),
            service(
                None,
                Some("edge"),
                Some("ghcr.io/railwayapp-templates/pgbouncer:latest"),
            ),
        );
        config.services.insert(
            "replica-1".to_string(),
            service(
                None,
                Some("replica"),
                Some("ghcr.io/railwayapp-templates/postgres-ha/postgres-patroni:16"),
            ),
        );
        config.services.insert(
            "haproxy".to_string(),
            service(
                None,
                Some("edge"),
                Some("ghcr.io/railwayapp-templates/postgres-ha/haproxy:3.2"),
            ),
        );

        let names: BTreeMap<String, String> = [
            ("haproxy", "Postgres HA"),
            ("pooler", "PgBouncer"),
            ("replica-1", "Postgres-2"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        (config, names)
    }

    #[test]
    fn revert_sweep_never_deletes_the_engines_pooler() {
        let (config, names) = mixed_environment();
        let repositories = postgres_ha_repositories();

        // The membership snapshot picked the pooler up too: it hangs off the
        // root exactly like a member does.
        let pre_revert_members = vec![
            ("replica-1".to_string(), "Postgres-2".to_string()),
            ("pooler".to_string(), "PgBouncer".to_string()),
        ];

        let mut targets = revert_sweep_targets(
            &POSTGRES,
            &pre_revert_members,
            &config,
            "root",
            &names,
            &repositories,
        );
        targets.sort();

        // The replica (snapshot path) and the orphaned haproxy (debris path)
        // are swept; the pooler is excluded from BOTH paths, and the root is
        // never a target.
        assert_eq!(
            targets,
            vec![
                ("haproxy".to_string(), "Postgres HA".to_string()),
                ("replica-1".to_string(), "Postgres-2".to_string()),
            ]
        );

        // A RESUMED revert has no membership snapshot at all -- the earlier
        // run's templateRevert already cleared the HA marker -- so the debris
        // scan alone must still find what the dead sweep left behind (and
        // still never the pooler). This is the path a re-run takes after a
        // member delete failed transiently.
        let mut resumed =
            revert_sweep_targets(&POSTGRES, &[], &config, "root", &names, &repositories);
        resumed.sort();
        assert_eq!(
            resumed,
            vec![
                ("haproxy".to_string(), "Postgres HA".to_string()),
                ("replica-1".to_string(), "Postgres-2".to_string()),
            ]
        );
    }

    #[test]
    fn revert_sweep_never_reaches_another_engines_cluster() {
        // Reverting a REDIS cluster in this environment. Every orphan here
        // belongs to the Postgres cluster, and a `clusterRole` stamp is all
        // they have in common with Redis debris -- which is exactly why the
        // role stamp alone cannot be the test. Scoped to what the redis-ha
        // companion actually publishes, none of them match.
        let (config, names) = mixed_environment();

        let swept = revert_sweep_targets(
            &REDIS,
            &[],
            &config,
            "redis-root",
            &names,
            &redis_ha_repositories(),
        );

        assert!(
            swept.is_empty(),
            "a redis revert swept the postgres cluster: {swept:?}"
        );
    }

    #[test]
    fn revert_sweep_skips_the_debris_scan_when_the_companion_is_unreadable() {
        // No repositories means the template record was unreachable, so
        // there is no evidence tying any orphan to this cluster. The
        // snapshot members are still provably members and are still swept;
        // the debris scan is skipped rather than widened to everything.
        let (config, names) = mixed_environment();

        let swept = revert_sweep_targets(
            &POSTGRES,
            &[("replica-1".to_string(), "Postgres-2".to_string())],
            &config,
            "root",
            &names,
            &[],
        );

        assert_eq!(
            swept,
            vec![("replica-1".to_string(), "Postgres-2".to_string())]
        );
    }

    #[test]
    fn each_engine_declares_the_switchover_mechanism_its_cluster_actually_speaks() {
        // Postgres nodes run a coordinator with a cluster-wide member API;
        // Redis and MySQL colocate theirs and expose only the per-node
        // contract. Driving one through the other's path reaches nothing.
        assert_eq!(
            POSTGRES.ha.unwrap().switchover,
            SwitchoverMechanism::Patroni
        );
        assert_eq!(
            REDIS.ha.unwrap().switchover,
            SwitchoverMechanism::DeclaredHttp
        );
        assert_eq!(
            MYSQL.ha.unwrap().switchover,
            SwitchoverMechanism::DeclaredHttp
        );
    }
}
