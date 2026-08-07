//! Live-scaling controller for `railway postgres ha scale` -- mutates an
//! **already-converted** HA cluster's live topology by creating/deleting
//! whole replica/coordinator services (each is its own Railway service+volume
//! in this architecture) and restamping the cluster's declared wiring on
//! survivors. Ports `packages/frontend/src/hooks/cluster/useScaleHACluster.tsx`
//! against this CLI's `EnvironmentConfig`/`ServiceInstance` shape rather than
//! the frontend's `SerializedTemplate` shape `template_apply.rs` uses for
//! *initial* conversion -- same wiring concept, re-derived against the live
//! config type rather than force-converted from the JSON one.
//!
//! Deliberate simplifications relative to the frontend hook (rationale
//! inline at each site below):
//!   - Scaling up clones an EXISTING live sibling replica/coordinator's own
//!     shape (source image, variables, volume mount path) rather than
//!     re-fetching the original template. Scaling up from zero members of a
//!     role isn't supported (there's nothing to clone) -- `railway postgres
//!     ha convert --replicas/--coordinators N` is the way to add the first
//!     member of a role; it already owns the template-fetch/adjust path.
//!   - Because the clone source is a LIVE, already-deployed sibling (not a
//!     raw template), its variable VALUES are already fully-resolved real
//!     references rather than template-relative ones -- so unlike the
//!     frontend's `convertTemplateVariables`, no ref-rewriting pass is
//!     needed. Only the node's own identity variable (from `ClusterWiring`)
//!     gets overwritten, exactly mirroring what
//!     `template_apply::restamp_after_replica_adjust` already does for the
//!     initial-conversion case. This also means `WAL_ARCHIVE_*` variables
//!     (stamped when PITR is enabled after the initial HA conversion) carry
//!     over automatically with the rest of the clone, with no special-case
//!     handling required.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use rand::Rng;
use regex::Regex;

use crate::{
    client::post_graphql,
    controllers::{
        config::{
            ClusterWiring, DeployConfig, EnvironmentConfig, RegionConfig, ServiceInstance,
            Variable, VolumeMount, fetch_environment_config,
        },
        project::ServiceContext,
        template_apply::{self, format_data_node_entry, private_domain_ref},
    },
    errors::RailwayError,
    gql::mutations,
};

const PATRONI_ENABLED_VAR: &str = "PATRONI_ENABLED";

/// Requested target counts for one `railway postgres ha scale` invocation.
/// Any combination of the three may be `Some` at once (clap's `ArgGroup`
/// only requires at least one).
pub struct ScaleClusterParams {
    pub replicas: Option<i64>,
    pub coordinators: Option<i64>,
    pub edge: Option<i64>,
    pub auto_deploy: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScaleDimensionSummary {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl ScaleDimensionSummary {
    pub fn is_noop(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeScaleSummary {
    pub region: String,
    pub previous_replicas: i64,
    pub target_replicas: i64,
}

pub struct ScaleClusterResult {
    /// `true` if the staged patch was committed with deploys enabled.
    pub deployed: bool,
    pub replicas: Option<ScaleDimensionSummary>,
    pub coordinators: Option<ScaleDimensionSummary>,
    pub edge: Option<EdgeScaleSummary>,
}

/// Coordinator/consensus node counts must stay odd for quorum. Validated
/// up front and rejected outright -- never silently rounded.
pub fn validate_odd_coordinator_count(target: i64) -> Result<()> {
    if target % 2 == 0 {
        bail!("--coordinators must be an odd number for consensus quorum (got {target})");
    }
    Ok(())
}

/// Scales an already-converted HA cluster's replica/coordinator/edge counts
/// per `params`, creating/deleting whole services as needed and restamping
/// the cluster's declared wiring on survivors. `names` is a service id ->
/// name lookup for the ROOT project's already-existing services (from
/// `super::service_name_map`); brand-new nodes created during this call are
/// tracked by the name returned from their own `serviceCreate` response, not
/// this map.
pub async fn scale_cluster(
    ctx: &ServiceContext,
    root_id: &str,
    root_name: &str,
    names: &BTreeMap<String, String>,
    params: ScaleClusterParams,
) -> Result<ScaleClusterResult> {
    if let Some(target) = params.coordinators {
        validate_odd_coordinator_count(target)?;
    }

    // Decrypted fetch -- new replica/coordinator clones need the sibling's
    // REAL variable values (e.g. sealed credentials), not the masked
    // placeholders a non-decrypted fetch returns. Mirrors `environment/new.rs`'s
    // environment-duplication flow, the other place this codebase creates
    // real services by cloning another service's live variables.
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;

    let mut patch: BTreeMap<String, ServiceInstance> = BTreeMap::new();
    let mut replicas_summary = None;
    let mut coordinators_summary = None;
    let mut edge_summary = None;
    let mut fresh_replica_roster: Option<Vec<(String, String)>> = None;

    if let Some(target) = params.replicas {
        let (summary, roster) =
            scale_replicas(ctx, &config, root_id, root_name, target, names, &mut patch).await?;
        replicas_summary = Some(summary);
        fresh_replica_roster = Some(roster);
    }

    if let Some(target) = params.coordinators {
        coordinators_summary = Some(
            scale_internal(
                ctx,
                &config,
                root_id,
                root_name,
                target,
                names,
                fresh_replica_roster.as_deref(),
                &mut patch,
            )
            .await?,
        );
    }

    if let Some(target) = params.edge {
        edge_summary = scale_edge(&config, root_id, target, &mut patch)?;
    }

    let deployed = if patch.is_empty() {
        false
    } else {
        let env_patch = EnvironmentConfig {
            services: patch,
            ..EnvironmentConfig::default()
        };
        stage_and_commit(ctx, env_patch, params.auto_deploy).await?
    };

    Ok(ScaleClusterResult {
        deployed,
        replicas: replicas_summary,
        coordinators: coordinators_summary,
        edge: edge_summary,
    })
}

async fn stage_and_commit(
    ctx: &ServiceContext,
    patch: EnvironmentConfig,
    auto_deploy: bool,
) -> Result<bool> {
    template_apply::warn_if_preexisting_staged_changes(ctx).await;

    post_graphql::<mutations::EnvironmentStageChanges, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::environment_stage_changes::Variables {
            environment_id: ctx.environment_id.clone(),
            input: patch,
            merge: Some(true),
        },
    )
    .await
    .context("Failed to stage cluster scale changes")?;

    template_apply::commit_staged_patch(ctx, auto_deploy).await
}

// --- replica scaling ---------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn scale_replicas(
    ctx: &ServiceContext,
    config: &EnvironmentConfig,
    root_id: &str,
    root_name: &str,
    target_count: i64,
    names: &BTreeMap<String, String>,
    patch: &mut BTreeMap<String, ServiceInstance>,
) -> Result<(ScaleDimensionSummary, Vec<(String, String)>)> {
    let root = config
        .services
        .get(root_id)
        .with_context(|| format!("Service \"{root_name}\" not found in environment config"))?;

    let mut existing = members_of_role(config, root_id, "replica", names);
    let current_count = existing.len() as i64;
    if target_count == current_count {
        return Ok((ScaleDimensionSummary::default(), existing));
    }

    let wiring = resolve_cluster_wiring(root).with_context(|| {
        "Could not resolve this cluster's scale wiring -- scaling would leave the connection \
         routing list stale. The root service is missing both `clusterWiring` and the legacy \
         `PATRONI_ENABLED` variable."
            .to_string()
    })?;
    let routing_edge_id = find_routing_edge_id(config, root_id);

    let summary = if target_count > current_count {
        let Some((source_id, source_name)) = existing.first().cloned() else {
            bail!(
                "Cannot scale up replicas on {root_name}: there is no existing replica to clone \
                 from. Use `railway postgres ha convert --replicas {target_count}` to add the \
                 first replica."
            );
        };
        let source = config
            .services
            .get(&source_id)
            .context("Replica disappeared from environment config mid-scale")?;
        let source_image = source
            .source
            .as_ref()
            .and_then(|s| s.image.clone())
            .context("Replica has no source image to clone")?;
        let mount_path = source
            .volume_mounts
            .values()
            .find_map(|m| m.mount_path.clone())
            .context("Replica has no volume mount path to clone")?;

        let base_name = derive_node_base_name(&source_name, "Replica");
        let existing_names: Vec<String> = existing.iter().map(|(_, name)| name.clone()).collect();
        let start_number = next_node_number(&existing_names, &base_name);

        let to_add = target_count - current_count;
        let mut added = Vec::with_capacity(to_add as usize);
        for next_number in start_number..start_number + to_add {
            let node_name = format!("{base_name}-{next_number}");
            let node = create_clone_service(ctx, &node_name, &source_image).await?;
            let volume = create_clone_volume(ctx, &node.id, &mount_path, &node.name).await?;

            patch.insert(
                node.id.clone(),
                ServiceInstance {
                    parent_service_id: Some(root_id.to_string()),
                    cluster_role: Some("replica".to_string()),
                    variables: source.variables.clone(),
                    volume_mounts: BTreeMap::from([(
                        volume.id,
                        VolumeMount {
                            mount_path: Some(mount_path.clone()),
                            ..VolumeMount::default()
                        },
                    )]),
                    deploy: Some(DeployConfig {
                        required_mount_path: Some(mount_path.clone()),
                        ..DeployConfig::default()
                    }),
                    ..ServiceInstance::default()
                },
            );

            existing.push((node.id.clone(), node.name.clone()));
            added.push(node.name.clone());
        }

        ScaleDimensionSummary {
            added,
            removed: Vec::new(),
        }
    } else {
        let to_remove = current_count - target_count;
        let base_name = existing
            .first()
            .map(|(_, name)| derive_node_base_name(name, "Replica"))
            .unwrap_or_else(|| "Replica".to_string());

        // Highest-numbered replicas go first -- the root itself is never in
        // this list (replicas only), so no separate "never the primary"
        // exclusion is needed here (unlike coordinator scale-down below).
        let mut sorted = existing.clone();
        sorted
            .sort_by_key(|(_, name)| std::cmp::Reverse(node_number(name, &base_name).unwrap_or(0)));
        let to_delete: Vec<(String, String)> =
            sorted.into_iter().take(to_remove as usize).collect();

        for (id, _) in &to_delete {
            delete_member(ctx, config, id).await?;
        }

        let removed: Vec<String> = to_delete.iter().map(|(_, name)| name.clone()).collect();
        existing.retain(|(id, _)| !to_delete.iter().any(|(rid, _)| rid == id));

        ScaleDimensionSummary {
            added: Vec::new(),
            removed,
        }
    };

    restamp_replica_wiring(
        patch,
        &wiring,
        root_id,
        root_name,
        routing_edge_id.as_deref(),
        &existing,
    );

    Ok((summary, existing))
}

// --- coordinator (internal) scaling -------------------------------------

#[allow(clippy::too_many_arguments)]
async fn scale_internal(
    ctx: &ServiceContext,
    config: &EnvironmentConfig,
    root_id: &str,
    root_name: &str,
    target_count: i64,
    names: &BTreeMap<String, String>,
    fresh_replica_roster: Option<&[(String, String)]>,
    patch: &mut BTreeMap<String, ServiceInstance>,
) -> Result<ScaleDimensionSummary> {
    let root = config
        .services
        .get(root_id)
        .with_context(|| format!("Service \"{root_name}\" not found in environment config"))?;

    let mut existing = members_of_role(config, root_id, "internal", names);
    let current_count = existing.len() as i64;
    if target_count == current_count {
        return Ok(ScaleDimensionSummary::default());
    }

    let wiring = resolve_cluster_wiring(root).with_context(|| {
        "Could not resolve this cluster's scale wiring -- scaling would leave the coordinator \
         host list stale. The root service is missing both `clusterWiring` and the legacy \
         `PATRONI_ENABLED` variable."
            .to_string()
    })?;

    // Root + every replica carry the coordinator hosts variable. Use the
    // FRESH replica roster from a `--replicas` change in this same
    // invocation when there is one, rather than re-deriving it from `config`
    // (fetched before that change) -- otherwise a combined `--replicas
    // N --coordinators M` call would stamp the rebuilt coordinator host list
    // onto the pre-scale replica set and miss brand-new replicas.
    let replica_ids: Vec<String> = match fresh_replica_roster {
        Some(roster) => roster.iter().map(|(id, _)| id.clone()).collect(),
        None => members_of_role(config, root_id, "replica", names)
            .into_iter()
            .map(|(id, _)| id)
            .collect(),
    };
    let data_node_ids: Vec<String> = std::iter::once(root_id.to_string())
        .chain(replica_ids)
        .collect();

    let summary = if target_count > current_count {
        let Some((source_id, source_name)) = existing.first().cloned() else {
            bail!(
                "Cannot scale up coordinators on {root_name}: there is no existing coordinator \
                 node to clone from. Use `railway postgres ha convert --coordinators \
                 {target_count}` to add the first one."
            );
        };
        let source = config
            .services
            .get(&source_id)
            .context("Coordinator node disappeared from environment config mid-scale")?;
        let source_image = source
            .source
            .as_ref()
            .and_then(|s| s.image.clone())
            .context("Coordinator node has no source image to clone")?;
        let mount_path = source
            .volume_mounts
            .values()
            .find_map(|m| m.mount_path.clone())
            .context("Coordinator node has no volume mount path to clone")?;

        let base_name = derive_node_base_name(&source_name, "internal");
        let existing_names: Vec<String> = existing.iter().map(|(_, name)| name.clone()).collect();
        let start_number = next_node_number(&existing_names, &base_name);

        let to_add = target_count - current_count;
        let mut added = Vec::with_capacity(to_add as usize);
        for next_number in start_number..start_number + to_add {
            let node_name = format!("{base_name}-{next_number}");
            let node = create_clone_service(ctx, &node_name, &source_image).await?;
            let volume = create_clone_volume(ctx, &node.id, &mount_path, &node.name).await?;

            patch.insert(
                node.id.clone(),
                ServiceInstance {
                    parent_service_id: Some(root_id.to_string()),
                    cluster_role: Some("internal".to_string()),
                    variables: source.variables.clone(),
                    volume_mounts: BTreeMap::from([(
                        volume.id,
                        VolumeMount {
                            mount_path: Some(mount_path.clone()),
                            ..VolumeMount::default()
                        },
                    )]),
                    deploy: Some(DeployConfig {
                        required_mount_path: Some(mount_path.clone()),
                        ..DeployConfig::default()
                    }),
                    ..ServiceInstance::default()
                },
            );

            existing.push((node.id.clone(), node.name.clone()));
            added.push(node.name.clone());
        }

        ScaleDimensionSummary {
            added,
            removed: Vec::new(),
        }
    } else {
        let to_remove = current_count - target_count;
        let base_name = existing
            .first()
            .map(|(_, name)| derive_node_base_name(name, "internal"))
            .unwrap_or_else(|| "internal".to_string());

        // Never remove the primary (lowest-numbered) coordinator node --
        // there's no single fixed "root" id for this role, so the exclusion
        // is by naming convention instead, mirroring
        // `haClusterUtils.ts`'s `isPrimaryInternalNode`.
        let primary_id = find_primary_internal(&existing, &base_name).map(|(id, _)| id.clone());

        let mut sorted = existing.clone();
        sorted
            .sort_by_key(|(_, name)| std::cmp::Reverse(node_number(name, &base_name).unwrap_or(0)));
        let removable: Vec<(String, String)> = sorted
            .into_iter()
            .filter(|(id, _)| Some(id) != primary_id.as_ref())
            .collect();

        if (removable.len() as i64) < to_remove {
            bail!("Cannot remove the primary coordinator node on {root_name}.");
        }
        let to_delete: Vec<(String, String)> =
            removable.into_iter().take(to_remove as usize).collect();

        for (id, _) in &to_delete {
            delete_member(ctx, config, id).await?;
        }

        let removed: Vec<String> = to_delete.iter().map(|(_, name)| name.clone()).collect();
        existing.retain(|(id, _)| !to_delete.iter().any(|(rid, _)| rid == id));

        ScaleDimensionSummary {
            added: Vec::new(),
            removed,
        }
    };

    restamp_internal_wiring(patch, &wiring, &existing, &data_node_ids);

    Ok(summary)
}

// --- edge scaling --------------------------------------------------------

/// Plain container-replica-count change on the cluster's routing edge (e.g.
/// HAProxy) -- `deploy.multiRegionConfig[region].numReplicas`. Not a new
/// service case: mirrors `useScaleHACluster.tsx`'s `scaleEdgeNodes`, which
/// finds the one active region entry and sets its replica count directly.
fn scale_edge(
    config: &EnvironmentConfig,
    root_id: &str,
    target_count: i64,
    patch: &mut BTreeMap<String, ServiceInstance>,
) -> Result<Option<EdgeScaleSummary>> {
    let edge_id = find_routing_edge_id(config, root_id)
        .context("Routing edge service (e.g. HAProxy) not found in this cluster")?;
    let edge = config
        .services
        .get(&edge_id)
        .context("Edge service disappeared from environment config")?;
    let mrc = edge
        .deploy
        .as_ref()
        .and_then(|d| d.multi_region_config.as_ref())
        .context("Edge service has no multi-region config to scale")?;
    let (region, current) = mrc
        .iter()
        .find(|(_, v)| v.is_some())
        .map(|(region, v)| {
            (
                region.clone(),
                v.as_ref().and_then(|r| r.num_replicas).unwrap_or(1),
            )
        })
        .context("Edge service region config not found")?;

    if current == target_count {
        return Ok(None);
    }

    let mut updated_mrc = mrc.clone();
    updated_mrc.insert(
        region.clone(),
        Some(RegionConfig {
            num_replicas: Some(target_count),
        }),
    );

    // Merge into any existing patch entry for this service rather than
    // overwriting it outright -- when `--edge` is combined with
    // `--replicas`/`--coordinators` in the same invocation, the routing edge
    // may already carry a staged `dataNodesVariable` restamp from
    // `restamp_replica_wiring` above, which must survive here.
    let entry = patch.entry(edge_id).or_default();
    let mut deploy = entry.deploy.clone().unwrap_or_default();
    deploy.multi_region_config = Some(updated_mrc);
    entry.deploy = Some(deploy);

    Ok(Some(EdgeScaleSummary {
        region,
        previous_replicas: current,
        target_replicas: target_count,
    }))
}

// --- wiring restamp ------------------------------------------------------

fn set_patch_var(
    patch: &mut BTreeMap<String, ServiceInstance>,
    service_id: &str,
    var_name: &str,
    value: String,
) {
    let entry = patch.entry(service_id.to_string()).or_default();
    entry.variables.insert(
        var_name.to_string(),
        Some(Variable {
            value: Some(value),
            ..Variable::default()
        }),
    );
}

/// Restamps the root's declared wiring after a replica scale up/down: each
/// replica's own identity variable, the routing edge's data-node list, and
/// consensus quorum on root + replicas. Mirrors
/// `useScaleHACluster.tsx`'s `scaleReplicaNodes` restamp step /
/// `template_apply::restamp_after_replica_adjust`, against `EnvironmentConfig`.
fn restamp_replica_wiring(
    patch: &mut BTreeMap<String, ServiceInstance>,
    wiring: &ClusterWiring,
    root_id: &str,
    root_name: &str,
    routing_edge_id: Option<&str>,
    replicas_after: &[(String, String)],
) {
    if let Some(var_name) = &wiring.replica_node_name_variable {
        for (id, name) in replicas_after {
            set_patch_var(patch, id, var_name, name.to_ascii_lowercase());
        }
    }

    if let (Some(edge_id), Some(data_var), Some(entry_format)) = (
        routing_edge_id,
        &wiring.data_nodes_variable,
        &wiring.data_nodes_entry_format,
    ) {
        let mut data_node_names: Vec<&str> = std::iter::once(root_name)
            .chain(replicas_after.iter().map(|(_, name)| name.as_str()))
            .collect();
        data_node_names.sort_unstable();
        let list = data_node_names
            .iter()
            .map(|name| format_data_node_entry(entry_format, name, root_name))
            .collect::<Vec<_>>()
            .join(",");
        set_patch_var(patch, edge_id, data_var, list);
    }

    if let Some(quorum_var) = &wiring.quorum_variable {
        let data_node_count = replicas_after.len() + 1; // + root
        let quorum = (data_node_count / 2 + 1).to_string();
        set_patch_var(patch, root_id, quorum_var, quorum.clone());
        for (id, _) in replicas_after {
            set_patch_var(patch, id, quorum_var, quorum.clone());
        }
    }
}

/// Restamps the root's declared coordinator wiring after an internal
/// (coordinator) scale up/down: each node's own identity variable and the
/// coordinator hostname list on every data node (root + replicas). Mirrors
/// `useScaleHACluster.tsx`'s `scaleInternalNodes` restamp step /
/// `template_apply::restamp_after_internal_adjust`, against
/// `EnvironmentConfig`.
fn restamp_internal_wiring(
    patch: &mut BTreeMap<String, ServiceInstance>,
    wiring: &ClusterWiring,
    internal_after: &[(String, String)],
    data_node_ids: &[String],
) {
    if let Some(var_name) = &wiring.internal_node_name_variable {
        for (id, name) in internal_after {
            set_patch_var(patch, id, var_name, name.to_ascii_lowercase());
        }
    }

    if let (Some(coordinator_var), Some(port)) =
        (&wiring.coordinator_hosts_variable, wiring.coordinator_port)
    {
        let mut sorted_names: Vec<&(String, String)> = internal_after.iter().collect();
        sorted_names.sort_by(|a, b| a.1.cmp(&b.1));
        let hosts = sorted_names
            .iter()
            .map(|(_, name)| format!("{}:{port}", private_domain_ref(name)))
            .collect::<Vec<_>>()
            .join(",");
        for id in data_node_ids {
            set_patch_var(patch, id, coordinator_var, hosts.clone());
        }
    }
}

// --- live-config lookups --------------------------------------------------

/// Root's declared `clusterWiring`, falling back to the historical hardcoded
/// Patroni wiring for legacy clusters that only set `PATRONI_ENABLED`.
/// Mirrors `template_apply::resolve_cluster_wiring`'s fallback, re-derived
/// against the live `ServiceInstance` shape instead of a template's raw
/// JSON -- same wiring concept, different Rust type, so this is a
/// re-derivation rather than a shared helper (see module doc comment).
fn resolve_cluster_wiring(root: &ServiceInstance) -> Option<ClusterWiring> {
    if let Some(wiring) = &root.cluster_wiring {
        return Some(wiring.clone());
    }

    if !root.variables.contains_key(PATRONI_ENABLED_VAR) {
        return None;
    }

    Some(ClusterWiring {
        internal_node_name_variable: Some("ETCD_NAME".to_string()),
        coordinator_hosts_variable: Some("PATRONI_ETCD3_HOSTS".to_string()),
        coordinator_port: Some(2379),
        replica_node_name_variable: Some("PATRONI_NAME".to_string()),
        data_nodes_variable: Some("POSTGRES_NODES".to_string()),
        data_nodes_entry_format: Some("{host}:${{{rootName}.PGPORT}}:8008".to_string()),
        quorum_variable: None,
    })
}

/// Live children of `root_id` carrying `cluster_role == role`, resolved to
/// `(id, name)` pairs via `names` (falling back to the bare id if unknown).
fn members_of_role(
    config: &EnvironmentConfig,
    root_id: &str,
    role: &str,
    names: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    config
        .services
        .iter()
        .filter(|(_, s)| {
            s.parent_service_id.as_deref() == Some(root_id)
                && s.cluster_role.as_deref() == Some(role)
        })
        .map(|(id, _)| {
            (
                id.clone(),
                names.get(id).cloned().unwrap_or_else(|| id.clone()),
            )
        })
        .collect()
}

/// The cluster's non-pooling routing edge (e.g. HAProxy) among `root_id`'s
/// children -- excludes a stacked PgBouncer edge, which also carries
/// `clusterRole == "edge"` under the same root. Mirrors
/// `useScaleHACluster.tsx`'s `routingEdgeService` filter.
fn find_routing_edge_id(config: &EnvironmentConfig, root_id: &str) -> Option<String> {
    config
        .services
        .iter()
        .find(|(_, s)| {
            s.parent_service_id.as_deref() == Some(root_id)
                && s.cluster_role.as_deref() == Some("edge")
                && !s
                    .source
                    .as_ref()
                    .and_then(|src| src.image.as_deref())
                    .is_some_and(|image| image.to_ascii_lowercase().contains("pgbouncer"))
        })
        .map(|(id, _)| id.clone())
}

// --- naming helpers (ported from haClusterUtils.ts) -----------------------

/// Derives the node-naming base ("Postgres", "etcd", ...) from a node's own
/// name, by taking everything up to its FIRST "-<digits>" group. Ported from
/// `haClusterUtils.ts`'s `deriveNodeBaseName`. NOT anchored to the end of
/// the string (unlike `template_apply.rs`'s `strip_trailing_dash_number`,
/// which only needs to handle fresh template names) because a live-scaled
/// node can carry a collision-avoidance suffix from an earlier scale-up
/// (`createServiceWithRetry` appends `-<suffix>` on a name clash), leaving a
/// name like `etcd-1-xK2p` where the number isn't the last token.
fn derive_node_base_name(any_node_name: &str, fallback: &str) -> String {
    let re = Regex::new(r"^(.+?)-\d+").expect("valid regex");
    if let Some(caps) = re.captures(any_node_name) {
        return caps[1].to_string();
    }
    if any_node_name.is_empty() || any_node_name.chars().all(|c| c.is_ascii_digit()) {
        fallback.to_string()
    } else {
        any_node_name.to_string()
    }
}

/// First `<base_name>-<digits>` match anywhere in `name` (case-insensitive),
/// mirroring `haClusterUtils.ts`'s `buildNodeNumberRegex`.
fn node_number(name: &str, base_name: &str) -> Option<i64> {
    let pattern = format!("(?i){}-(\\d+)", regex::escape(base_name));
    let re = Regex::new(&pattern).expect("valid regex");
    re.captures(name)?.get(1)?.as_str().parse().ok()
}

/// Next free `<base>-N` number for a new cluster node. Ported from
/// `haClusterUtils.ts`'s `nextNodeNumber`.
fn next_node_number(existing_names: &[String], base_name: &str) -> i64 {
    let existing_numbers = existing_names
        .iter()
        .filter_map(|name| node_number(name, base_name))
        .filter(|n| *n > 0);
    existing_numbers
        .chain(std::iter::once(existing_names.len() as i64))
        .max()
        .unwrap_or(0)
        + 1
}

/// Lowest-numbered node in `nodes` -- the "primary" coordinator/internal
/// node by convention, protected from scale-down removal. Mirrors
/// `haClusterUtils.ts`'s `findPrimaryInternalNode`.
fn find_primary_internal<'a>(
    nodes: &'a [(String, String)],
    base_name: &str,
) -> Option<&'a (String, String)> {
    nodes
        .iter()
        .filter(|(_, name)| node_number(name, base_name).is_some())
        .min_by_key(|(_, name)| node_number(name, base_name).unwrap_or(i64::MAX))
}

/// 4-character random alphanumeric suffix for duplicate-name retry, mirroring
/// `haClusterUtils.ts`'s `generateSuffix` (`nanoid(4)`).
fn generate_suffix() -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..4)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}

/// True when a GraphQL error looks like a unique-name-collision rejection,
/// mirroring `haClusterUtils.ts`'s `isDuplicateNameError`.
fn is_duplicate_name_error(err: &RailwayError) -> bool {
    let message = err.to_string().to_ascii_lowercase();
    message.contains("unique") || message.contains("already exists")
}

// --- mutation helpers ------------------------------------------------------

struct CreatedNode {
    id: String,
    name: String,
}

/// Creates one new replica/coordinator service cloning `source_image`,
/// retrying once with a random suffix on a name collision. Mirrors
/// `useScaleHACluster.tsx`'s `createServiceWithRetry`.
async fn create_clone_service(
    ctx: &ServiceContext,
    base_name: &str,
    source_image: &str,
) -> Result<CreatedNode> {
    let build_vars = |name: String| mutations::service_create::Variables {
        name: Some(name),
        project_id: ctx.project_id.clone(),
        environment_id: ctx.environment_id.clone(),
        source: Some(mutations::service_create::ServiceSourceInput {
            image: Some(source_image.to_string()),
            repo: None,
        }),
        variables: None,
        branch: None,
    };

    let result = post_graphql::<mutations::ServiceCreate, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        build_vars(base_name.to_string()),
    )
    .await;

    let created = match result {
        Ok(r) => r,
        Err(err) if is_duplicate_name_error(&err) => {
            let retried_name = format!("{base_name}-{}", generate_suffix());
            post_graphql::<mutations::ServiceCreate, _>(
                &ctx.client,
                ctx.configs.get_backboard(),
                build_vars(retried_name),
            )
            .await
            .context("Failed to create cluster node service (after retrying a duplicate name)")?
        }
        Err(err) => {
            return Err(err).context("Failed to create cluster node service");
        }
    };

    Ok(CreatedNode {
        id: created.service_create.id,
        name: created.service_create.name,
    })
}

struct CreatedVolume {
    id: String,
}

/// Creates a volume for a new node at `mount_path`, best-effort naming it
/// `<node_name>-volume` (retrying with a short unique suffix on a name
/// clash, and simply leaving it unnamed if that also fails -- volume names
/// are cosmetic).
async fn create_clone_volume(
    ctx: &ServiceContext,
    service_id: &str,
    mount_path: &str,
    node_name: &str,
) -> Result<CreatedVolume> {
    let created = post_graphql::<mutations::VolumeCreate, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::volume_create::Variables {
            project_id: ctx.project_id.clone(),
            environment_id: ctx.environment_id.clone(),
            service_id: service_id.to_string(),
            mount_path: mount_path.to_string(),
        },
    )
    .await
    .context("Failed to create volume for new cluster node")?;

    let volume_id = created.volume_create.id.clone();
    let name_result = post_graphql::<mutations::VolumeNameUpdate, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::volume_name_update::Variables {
            volume_id: volume_id.clone(),
            name: format!("{node_name}-volume"),
        },
    )
    .await;

    if let Err(err) = name_result
        && is_duplicate_name_error(&err)
    {
        let unique_suffix = &volume_id[..8.min(volume_id.len())];
        let _ = post_graphql::<mutations::VolumeNameUpdate, _>(
            &ctx.client,
            ctx.configs.get_backboard(),
            mutations::volume_name_update::Variables {
                volume_id: volume_id.clone(),
                name: format!("{node_name}-volume-{unique_suffix}"),
            },
        )
        .await;
    }

    Ok(CreatedVolume { id: volume_id })
}

/// Deletes a cluster member's volume(s) then the service itself. Volumes are
/// deleted first (matches the frontend's own ordering note: deleting the
/// service first would clear `volumeMounts`, losing the volume ids to clean
/// up).
pub(crate) async fn delete_member(
    ctx: &ServiceContext,
    config: &EnvironmentConfig,
    service_id: &str,
) -> Result<()> {
    if let Some(service) = config.services.get(service_id) {
        for volume_id in service.volume_mounts.keys() {
            post_graphql::<mutations::VolumeDelete, _>(
                &ctx.client,
                ctx.configs.get_backboard(),
                mutations::volume_delete::Variables {
                    id: volume_id.clone(),
                },
            )
            .await
            .with_context(|| format!("Failed to delete volume {volume_id}"))?;
        }
    }

    post_graphql::<mutations::ServiceDelete, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::service_delete::Variables {
            service_id: service_id.to_string(),
            environment_id: ctx.environment_id.clone(),
        },
    )
    .await
    .with_context(|| format!("Failed to delete service {service_id}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(role: &str, parent: &str) -> ServiceInstance {
        ServiceInstance {
            cluster_role: Some(role.to_string()),
            parent_service_id: Some(parent.to_string()),
            ..ServiceInstance::default()
        }
    }

    fn config_with(services: Vec<(&str, ServiceInstance)>) -> EnvironmentConfig {
        let mut config = EnvironmentConfig::default();
        for (id, s) in services {
            config.services.insert(id.to_string(), s);
        }
        config
    }

    fn names(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(id, name)| (id.to_string(), name.to_string()))
            .collect()
    }

    #[test]
    fn validate_odd_coordinator_count_rejects_even() {
        assert!(validate_odd_coordinator_count(4).is_err());
        assert!(validate_odd_coordinator_count(3).is_ok());
        assert!(validate_odd_coordinator_count(1).is_ok());
    }

    #[test]
    fn derive_node_base_name_strips_first_number_group() {
        assert_eq!(
            derive_node_base_name("postgres-replica-2", "Replica"),
            "postgres-replica"
        );
        assert_eq!(derive_node_base_name("etcd-1-xK2p", "internal"), "etcd");
        assert_eq!(derive_node_base_name("db-prod", "Replica"), "db-prod");
        assert_eq!(derive_node_base_name("42", "Replica"), "Replica");
    }

    #[test]
    fn node_number_matches_first_group_case_insensitively() {
        assert_eq!(
            node_number("postgres-replica-2", "postgres-replica"),
            Some(2)
        );
        assert_eq!(node_number("ETCD-3-xyz", "etcd"), Some(3));
        assert_eq!(node_number("db-prod", "postgres-replica"), None);
    }

    #[test]
    fn next_node_number_continues_after_highest_existing() {
        let names = vec![
            "postgres-replica-1".to_string(),
            "postgres-replica-3".to_string(),
        ];
        assert_eq!(next_node_number(&names, "postgres-replica"), 4);
    }

    #[test]
    fn next_node_number_starts_at_two_from_empty() {
        let names: Vec<String> = vec![];
        assert_eq!(next_node_number(&names, "postgres-replica"), 1);
    }

    #[test]
    fn find_primary_internal_picks_lowest_number() {
        let nodes = vec![
            ("id-3".to_string(), "etcd-3".to_string()),
            ("id-1".to_string(), "etcd-1".to_string()),
            ("id-2".to_string(), "etcd-2".to_string()),
        ];
        let primary = find_primary_internal(&nodes, "etcd").unwrap();
        assert_eq!(primary.0, "id-1");
    }

    #[test]
    fn resolve_cluster_wiring_prefers_declared_over_legacy() {
        let declared = ClusterWiring {
            data_nodes_variable: Some("REDIS_NODES".to_string()),
            ..ClusterWiring::default()
        };
        let mut root = ServiceInstance {
            cluster_wiring: Some(declared),
            ..ServiceInstance::default()
        };
        root.variables.insert(
            PATRONI_ENABLED_VAR.to_string(),
            Some(Variable {
                value: Some("true".to_string()),
                ..Variable::default()
            }),
        );

        let wiring = resolve_cluster_wiring(&root).unwrap();
        assert_eq!(wiring.data_nodes_variable.as_deref(), Some("REDIS_NODES"));
    }

    #[test]
    fn resolve_cluster_wiring_falls_back_to_legacy_patroni() {
        let mut root = ServiceInstance::default();
        root.variables.insert(
            PATRONI_ENABLED_VAR.to_string(),
            Some(Variable {
                value: Some("true".to_string()),
                ..Variable::default()
            }),
        );

        let wiring = resolve_cluster_wiring(&root).unwrap();
        assert_eq!(
            wiring.data_nodes_variable.as_deref(),
            Some("POSTGRES_NODES")
        );
        assert_eq!(wiring.coordinator_port, Some(2379));
    }

    #[test]
    fn resolve_cluster_wiring_none_without_wiring_or_patroni() {
        let root = ServiceInstance::default();
        assert!(resolve_cluster_wiring(&root).is_none());
    }

    #[test]
    fn find_routing_edge_id_skips_pgbouncer_edge() {
        use crate::controllers::config::ServiceSource;

        let pgbouncer = ServiceInstance {
            source: Some(ServiceSource {
                image: Some("ghcr.io/railwayapp-templates/pgbouncer:latest".to_string()),
                ..ServiceSource::default()
            }),
            ..service("edge", "root")
        };
        let haproxy = ServiceInstance {
            source: Some(ServiceSource {
                image: Some("ghcr.io/railwayapp-templates/haproxy:latest".to_string()),
                ..ServiceSource::default()
            }),
            ..service("edge", "root")
        };
        let config = config_with(vec![("pgbouncer", pgbouncer), ("haproxy", haproxy)]);

        assert_eq!(
            find_routing_edge_id(&config, "root"),
            Some("haproxy".to_string())
        );
    }

    #[test]
    fn members_of_role_filters_by_parent_and_role() {
        let config = config_with(vec![
            ("replica-1", service("replica", "root")),
            ("etcd-1", service("internal", "root")),
            ("other-replica", service("replica", "some-other-root")),
        ]);
        let names = names(&[("replica-1", "postgres-replica-1")]);
        let members = members_of_role(&config, "root", "replica", &names);
        assert_eq!(
            members,
            vec![("replica-1".to_string(), "postgres-replica-1".to_string())]
        );
    }

    #[test]
    fn restamp_replica_wiring_stamps_identity_edge_list_and_quorum() {
        let wiring = ClusterWiring {
            replica_node_name_variable: Some("PATRONI_NAME".to_string()),
            data_nodes_variable: Some("POSTGRES_NODES".to_string()),
            data_nodes_entry_format: Some("{host}:${{{rootName}.PGPORT}}:8008".to_string()),
            quorum_variable: Some("QUORUM".to_string()),
            ..ClusterWiring::default()
        };
        let replicas = vec![
            ("r1".to_string(), "postgres-replica-1".to_string()),
            ("r2".to_string(), "postgres-replica-2".to_string()),
        ];
        let mut patch = BTreeMap::new();

        restamp_replica_wiring(
            &mut patch,
            &wiring,
            "root",
            "db-prod",
            Some("edge"),
            &replicas,
        );

        assert_eq!(
            patch["r1"].variables["PATRONI_NAME"]
                .as_ref()
                .unwrap()
                .value
                .as_deref(),
            Some("postgres-replica-1")
        );
        let edge_list = patch["edge"].variables["POSTGRES_NODES"]
            .as_ref()
            .unwrap()
            .value
            .clone()
            .unwrap();
        assert!(edge_list.contains("db-prod"));
        assert_eq!(edge_list.split(',').count(), 3);
        assert_eq!(
            patch["root"].variables["QUORUM"]
                .as_ref()
                .unwrap()
                .value
                .as_deref(),
            Some("2")
        );
    }

    #[test]
    fn restamp_internal_wiring_stamps_identity_and_coordinator_hosts() {
        let wiring = ClusterWiring {
            internal_node_name_variable: Some("ETCD_NAME".to_string()),
            coordinator_hosts_variable: Some("PATRONI_ETCD3_HOSTS".to_string()),
            coordinator_port: Some(2379),
            ..ClusterWiring::default()
        };
        let internal = vec![
            ("e1".to_string(), "etcd-1".to_string()),
            ("e2".to_string(), "etcd-2".to_string()),
            ("e3".to_string(), "etcd-3".to_string()),
        ];
        let mut patch = BTreeMap::new();

        restamp_internal_wiring(&mut patch, &wiring, &internal, &["root".to_string()]);

        assert_eq!(
            patch["e2"].variables["ETCD_NAME"]
                .as_ref()
                .unwrap()
                .value
                .as_deref(),
            Some("etcd-2")
        );
        let hosts = patch["root"].variables["PATRONI_ETCD3_HOSTS"]
            .as_ref()
            .unwrap()
            .value
            .clone()
            .unwrap();
        assert_eq!(hosts.split(',').count(), 3);
        assert!(hosts.contains(":2379"));
    }

    #[test]
    fn scale_edge_merges_into_existing_patch_entry_for_same_service() {
        let mut mrc = BTreeMap::new();
        mrc.insert(
            "us-west".to_string(),
            Some(RegionConfig {
                num_replicas: Some(1),
            }),
        );
        let edge = ServiceInstance {
            deploy: Some(DeployConfig {
                multi_region_config: Some(mrc),
                ..DeployConfig::default()
            }),
            ..service("edge", "root")
        };
        let config = config_with(vec![("edge-id", edge)]);

        let mut patch = BTreeMap::new();
        // Simulate a prior replica-wiring restamp already having touched
        // this same edge service's variables.
        set_patch_var(
            &mut patch,
            "edge-id",
            "POSTGRES_NODES",
            "existing-list".to_string(),
        );

        let summary = scale_edge(&config, "root", 3, &mut patch).unwrap().unwrap();
        assert_eq!(summary.previous_replicas, 1);
        assert_eq!(summary.target_replicas, 3);

        // The pre-existing variable restamp must survive the deploy patch.
        assert_eq!(
            patch["edge-id"].variables["POSTGRES_NODES"]
                .as_ref()
                .unwrap()
                .value
                .as_deref(),
            Some("existing-list")
        );
        let mrc = patch["edge-id"]
            .deploy
            .as_ref()
            .unwrap()
            .multi_region_config
            .as_ref()
            .unwrap();
        assert_eq!(mrc["us-west"].as_ref().unwrap().num_replicas, Some(3));
    }

    #[test]
    fn scale_edge_noop_when_already_at_target() {
        let mut mrc = BTreeMap::new();
        mrc.insert(
            "us-west".to_string(),
            Some(RegionConfig {
                num_replicas: Some(2),
            }),
        );
        let edge = ServiceInstance {
            deploy: Some(DeployConfig {
                multi_region_config: Some(mrc),
                ..DeployConfig::default()
            }),
            ..service("edge", "root")
        };
        let config = config_with(vec![("edge-id", edge)]);
        let mut patch = BTreeMap::new();

        let summary = scale_edge(&config, "root", 2, &mut patch).unwrap();
        assert!(summary.is_none());
        assert!(patch.is_empty());
    }

    #[test]
    fn scale_dimension_summary_is_noop_when_empty() {
        assert!(ScaleDimensionSummary::default().is_noop());
        assert!(
            !ScaleDimensionSummary {
                added: vec!["x".to_string()],
                removed: vec![],
            }
            .is_noop()
        );
    }

    #[test]
    fn scale_edge_errors_without_multi_region_config() {
        let edge = service("edge", "root");
        let config = config_with(vec![("edge-id", edge)]);
        let mut patch = BTreeMap::new();
        let err = scale_edge(&config, "root", 2, &mut patch).unwrap_err();
        assert!(err.to_string().contains("no multi-region config"));
    }

    #[test]
    fn scale_edge_errors_without_an_edge_service() {
        let config = config_with(vec![("replica-1", service("replica", "root"))]);
        let mut patch = BTreeMap::new();
        let err = scale_edge(&config, "root", 2, &mut patch).unwrap_err();
        assert!(err.to_string().contains("edge service"));
    }

    #[test]
    fn next_node_number_falls_back_to_count_when_names_are_unnumbered() {
        // One existing member whose name doesn't match `<base>-<n>` still
        // advances numbering past the member count (mirrors
        // haClusterUtils.ts's nextNodeNumber).
        let names = vec!["etcd".to_string()];
        assert_eq!(next_node_number(&names, "etcd"), 2);
    }

    #[test]
    fn find_primary_internal_none_when_no_numbered_nodes() {
        let nodes = vec![("id-a".to_string(), "etcd".to_string())];
        assert!(find_primary_internal(&nodes, "etcd").is_none());
    }

    #[test]
    fn duplicate_name_error_detection() {
        assert!(is_duplicate_name_error(&RailwayError::GraphQLError(
            "Service name must be unique within a project".to_string()
        )));
        assert!(is_duplicate_name_error(&RailwayError::GraphQLError(
            "A service with that name already exists".to_string()
        )));
        assert!(!is_duplicate_name_error(&RailwayError::GraphQLError(
            "Internal server error".to_string()
        )));
    }

    #[test]
    fn generate_suffix_is_four_lowercase_alphanumerics() {
        let suffix = generate_suffix();
        assert_eq!(suffix.len(), 4);
        assert!(
            suffix
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
    }

    #[test]
    fn members_of_role_falls_back_to_id_when_name_unknown() {
        let config = config_with(vec![("replica-1", service("replica", "root"))]);
        let members = members_of_role(&config, "root", "replica", &BTreeMap::new());
        assert_eq!(
            members,
            vec![("replica-1".to_string(), "replica-1".to_string())]
        );
    }

    #[test]
    fn restamp_replica_wiring_skips_undeclared_fields() {
        // Wiring with nothing declared: restamp must not invent any patch
        // entries (a cluster whose template opted out of a given mechanism
        // keeps its own hand-managed variables untouched).
        let wiring = ClusterWiring::default();
        let mut patch = BTreeMap::new();
        restamp_replica_wiring(
            &mut patch,
            &wiring,
            "root",
            "db",
            Some("edge"),
            &[("r1".to_string(), "replica-1".to_string())],
        );
        assert!(patch.is_empty());
    }
}
