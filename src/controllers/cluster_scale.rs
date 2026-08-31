//! Live-scaling controller for the managed-database `ha scale` verb -- mutates an
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
//!     role isn't supported (there's nothing to clone) -- `ha convert
//!     --replicas/--coordinators N` is the way to add the first member of a
//!     role; it already owns the template-fetch/adjust path.
//!   - Because the clone source is a LIVE, already-deployed sibling (not a
//!     raw template), its variable VALUES are already fully-resolved real
//!     references rather than template-relative ones -- so unlike the
//!     frontend's `convertTemplateVariables`, no ref-rewriting pass is
//!     needed. Only the node's own identity variable (from `ClusterWiring`)
//!     gets overwritten, exactly mirroring what
//!     `template_apply::restamp_after_replica_adjust` already does for the
//!     initial-conversion case. This also means the engine's archive
//!     variables (stamped when PITR is enabled after the initial HA
//!     conversion) carry over automatically with the rest of the clone, with
//!     no special-case handling required.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use rand::Rng;
use regex::Regex;

use crate::{
    client::post_graphql,
    controllers::{
        config::{
            ClusterWiring, DeployConfig, EnvironmentConfig, RegionConfig, ServiceInstance,
            Variable, VolumeInstance, VolumeMount, fetch_environment_config,
        },
        database_engines::DatabaseEngine,
        database_plugins,
        project::ServiceContext,
        template_apply::{self, format_data_node_entry, private_domain_ref},
    },
    errors::RailwayError,
    gql::mutations,
};

const PATRONI_ENABLED_VAR: &str = "PATRONI_ENABLED";

/// Requested target counts for one `ha scale` invocation.
/// Any combination of the three may be `Some` at once (clap's `ArgGroup`
/// only requires at least one).
pub struct ScaleClusterParams {
    pub replicas: Option<i64>,
    pub coordinators: Option<i64>,
    pub edge: Option<i64>,
    pub auto_deploy: bool,
    /// The data node currently acting as the cluster's primary, when the
    /// caller could determine it from a live probe. Scale-down never deletes
    /// this node, whatever its number: after a failover the highest-numbered
    /// replica may well BE the acting primary, and deleting it is a write
    /// outage plus whatever acked writes had not replicated yet. `None`
    /// means "could not be determined" -- the caller has already warned.
    pub live_primary_id: Option<String>,
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

#[derive(Debug)]
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

/// In a topology with no separate coordinator tier, the DATA nodes are the
/// voters: the cluster needs an odd number of at least three of them, or it
/// cannot elect a primary after losing one. The fence applies exactly where
/// the template says the data nodes carry the vote -- either by declaring the
/// quorum variable to restamp, or by declaring that its coordinator derives
/// its own majority from live membership.
///
/// `--replicas` counts nodes BESIDE the root, so an even replica count is
/// what makes the total odd. Rejected up front rather than rounded: a
/// silently adjusted cluster size is exactly the kind of surprise that shows
/// up as a failed failover months later.
pub fn validate_data_node_quorum(wiring: &ClusterWiring, replicas_target: i64) -> Result<()> {
    let votes_with_data_nodes =
        wiring.quorum_variable.is_some() || wiring.data_nodes_are_quorum_voters.unwrap_or(false);
    if !votes_with_data_nodes {
        return Ok(());
    }

    let data_nodes = replicas_target + 1;
    if data_nodes < 3 {
        bail!(
            "This cluster's data nodes carry the failover vote, so it needs at least 3 of them: \
             use --replicas 2 or more (got {replicas_target}, for {data_nodes} data node(s))."
        );
    }
    if data_nodes % 2 == 0 {
        bail!(
            "This cluster's data nodes carry the failover vote, so their total must be odd -- \
             an even cluster cannot elect a primary after losing a node. --replicas counts nodes \
             beside the primary, so pass an even number (got {replicas_target}, for {data_nodes} \
             data nodes)."
        );
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
    engine: &DatabaseEngine,
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

    if let Some(target) = params.replicas
        && let Some(wiring) = config
            .services
            .get(root_id)
            .and_then(resolve_cluster_wiring)
    {
        validate_data_node_quorum(&wiring, target)?;
    }

    let mut patch = ScalePatch::default();
    let mut replicas_summary = None;
    let mut coordinators_summary = None;
    let mut edge_summary = None;
    let mut fresh_replica_roster: Option<Vec<(String, String)>> = None;

    if let Some(target) = params.replicas {
        let (summary, roster) = scale_replicas(
            ctx,
            &config,
            engine,
            root_id,
            root_name,
            target,
            names,
            params.live_primary_id.as_deref(),
            &mut patch,
        )
        .await?;
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
        edge_summary = scale_edge(&config, engine, root_id, target, &mut patch.services)?;
    }

    let deployed = if patch.is_empty() {
        false
    } else {
        let env_patch = EnvironmentConfig {
            services: patch.services,
            volumes: patch.volumes,
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

/// The staged patch a scale builds up: the member services it adds, removes
/// or rewires, plus the volume INSTANCES those members mount.
///
/// Volumes ride in the SAME patch as the services on purpose -- backboard
/// sizes a new replica volume off the `clusterRole`/`parentServiceId` of
/// whichever service mounts it, read from the resolved config, so the volume
/// instance has to be created by the patch that stamps them and not ahead of
/// it. See `create_clone_volume`. The member SERVICE instances are created by
/// the patch for the same class of reason -- the patch-apply workflow's
/// instance-create path is the only one that persists the parent link (see
/// `create_clone_service`) -- and deletions are staged rather than issued
/// directly, so the whole scale commits atomically and the platform's
/// cluster-primacy commit guard can inspect it.
#[derive(Default)]
struct ScalePatch {
    services: BTreeMap<String, ServiceInstance>,
    volumes: BTreeMap<String, VolumeInstance>,
}

impl ScalePatch {
    fn is_empty(&self) -> bool {
        self.services.is_empty() && self.volumes.is_empty()
    }
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
    engine: &DatabaseEngine,
    root_id: &str,
    root_name: &str,
    target_count: i64,
    names: &BTreeMap<String, String>,
    live_primary_id: Option<&str>,
    patch: &mut ScalePatch,
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
         routing list stale. The root service declares no `clusterWiring`."
            .to_string()
    })?;
    let routing_edge_id = find_routing_edge_id(config, engine, root_id);

    let summary = if target_count > current_count {
        let Some((source_id, source_name)) = existing.first().cloned() else {
            bail!(
                "Cannot scale up replicas on {root_name}: there is no existing replica to clone \
                 from. Re-run `ha convert --replicas {target_count}` to add the first replica."
            );
        };
        let source = config
            .services
            .get(&source_id)
            .context("Replica disappeared from environment config mid-scale")?;
        source
            .source
            .as_ref()
            .and_then(|s| s.image.as_ref())
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
        let mut added_ids = Vec::with_capacity(to_add as usize);
        for next_number in start_number..start_number + to_add {
            let node_name = format!("{base_name}-{next_number}");
            let node = create_clone_service(ctx, &node_name).await?;
            let volume = create_clone_volume(ctx, &mount_path, &node.name).await?;

            stage_new_member(
                patch,
                root_id,
                root,
                "replica",
                &node,
                source,
                &volume,
                &mount_path,
            );

            added_ids.push(node.id.clone());
            existing.push((node.id.clone(), node.name.clone()));
            added.push(node.name.clone());
        }

        (
            ScaleDimensionSummary {
                added,
                removed: Vec::new(),
            },
            added_ids,
        )
    } else {
        let to_remove = current_count - target_count;
        let base_name = existing
            .first()
            .map(|(_, name)| derive_node_base_name(name, "Replica"))
            .unwrap_or_else(|| "Replica".to_string());

        // Highest-numbered replicas go first -- but never the node currently
        // ACTING as the primary. The root itself is never in this list
        // (replicas only), which covers the healthy case; after a failover
        // the acting primary is one of these replicas, and its number says
        // nothing about its role.
        let mut sorted = existing.clone();
        sorted
            .sort_by_key(|(_, name)| std::cmp::Reverse(node_number(name, &base_name).unwrap_or(0)));
        let removable: Vec<(String, String)> = sorted
            .into_iter()
            .filter(|(id, _)| Some(id.as_str()) != live_primary_id)
            .collect();
        if (removable.len() as i64) < to_remove {
            let primary_name = existing
                .iter()
                .find(|(id, _)| Some(id.as_str()) == live_primary_id)
                .map(|(_, name)| name.as_str())
                .unwrap_or("a replica");
            bail!(
                "Cannot scale down to {target_count} replica(s): {primary_name} is currently \
                 acting as the cluster's primary. Run `ha switchover --to {root_name}` first, \
                 then scale down."
            );
        }
        let to_delete: Vec<(String, String)> =
            removable.into_iter().take(to_remove as usize).collect();

        for (id, _) in &to_delete {
            stage_member_deletion(patch, config, id);
        }

        let removed: Vec<String> = to_delete.iter().map(|(_, name)| name.clone()).collect();
        existing.retain(|(id, _)| !to_delete.iter().any(|(rid, _)| rid == id));

        (
            ScaleDimensionSummary {
                added: Vec::new(),
                removed,
            },
            Vec::new(),
        )
    };

    let (summary, added_ids) = summary;
    restamp_replica_wiring(
        &mut patch.services,
        &wiring,
        root_name,
        routing_edge_id.as_deref(),
        &existing,
        &added_ids,
    );

    Ok((summary, existing))
}

/// Stages one brand-new cluster member and its volume into the patch. Both
/// records were created detached (no environment), so the patch commit is
/// what creates their instances: the volume that way for the sizing pairing
/// described on [`ScalePatch`], and the SERVICE because the patch-apply
/// workflow's instance-create path is the only one that persists
/// `parentServiceId` -- an instance pre-created by `serviceCreate` takes the
/// update path instead, which applies the role but silently drops the parent
/// link, orphaning the member from every parent-chain membership walk.
#[allow(clippy::too_many_arguments)]
fn stage_new_member(
    patch: &mut ScalePatch,
    root_id: &str,
    root: &ServiceInstance,
    role: &str,
    node: &CreatedNode,
    source: &ServiceInstance,
    volume: &CreatedVolume,
    mount_path: &str,
) {
    patch.volumes.insert(
        volume.id.clone(),
        VolumeInstance {
            is_created: Some(true),
            ..VolumeInstance::default()
        },
    );
    patch.services.insert(
        node.id.clone(),
        ServiceInstance {
            is_created: Some(true),
            parent_service_id: Some(root_id.to_string()),
            cluster_role: Some(role.to_string()),
            // Keep the new node in the cluster's canvas group, like the
            // members the conversion itself created.
            group_id: root.group_id.clone(),
            variables: source.variables.clone(),
            // The image rides the patch too now that the service record is
            // created bare.
            source: source.source.clone(),
            volume_mounts: BTreeMap::from([(
                volume.id.clone(),
                VolumeMount {
                    mount_path: Some(mount_path.to_string()),
                    ..VolumeMount::default()
                },
            )]),
            // The sibling's whole deploy config -- healthcheck, region
            // placement, start command -- not just the mount requirement: a
            // node cloned into a different region than the cluster it joins
            // replicates cross-region forever.
            deploy: Some(DeployConfig {
                required_mount_path: Some(mount_path.to_string()),
                ..source.deploy.clone().unwrap_or_default()
            }),
            ..ServiceInstance::default()
        },
    );
}

/// Stages a member's deletion (volumes first, then the service) into the
/// patch, mirroring the frontend's `deleteVolume`/`deleteService` builder
/// calls. Staged rather than deleted directly so the whole scale commits
/// atomically and the platform's cluster-primacy commit guard sees it.
fn stage_member_deletion(patch: &mut ScalePatch, config: &EnvironmentConfig, id: &str) {
    if let Some(service) = config.services.get(id) {
        for volume_id in service.volume_mounts.keys() {
            patch.volumes.insert(
                volume_id.clone(),
                VolumeInstance {
                    is_deleted: Some(true),
                    ..VolumeInstance::default()
                },
            );
        }
    }
    patch.services.insert(
        id.to_string(),
        ServiceInstance {
            is_deleted: Some(true),
            ..ServiceInstance::default()
        },
    );
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
    patch: &mut ScalePatch,
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

    let mut added_ids: Vec<String> = Vec::new();
    let summary = if target_count > current_count {
        let Some((source_id, source_name)) = existing.first().cloned() else {
            bail!(
                "Cannot scale up coordinators on {root_name}: there is no existing coordinator \
                 node to clone from. Re-run `ha convert --coordinators \
                 {target_count}` to add the first one."
            );
        };
        let source = config
            .services
            .get(&source_id)
            .context("Coordinator node disappeared from environment config mid-scale")?;
        source
            .source
            .as_ref()
            .and_then(|s| s.image.as_ref())
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
            let node = create_clone_service(ctx, &node_name).await?;
            let volume = create_clone_volume(ctx, &mount_path, &node.name).await?;

            stage_new_member(
                patch,
                root_id,
                root,
                "internal",
                &node,
                source,
                &volume,
                &mount_path,
            );

            added_ids.push(node.id.clone());
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
            stage_member_deletion(patch, config, id);
        }

        let removed: Vec<String> = to_delete.iter().map(|(_, name)| name.clone()).collect();
        existing.retain(|(id, _)| !to_delete.iter().any(|(rid, _)| rid == id));

        ScaleDimensionSummary {
            added: Vec::new(),
            removed,
        }
    };

    restamp_internal_wiring(
        &mut patch.services,
        &wiring,
        &existing,
        &added_ids,
        &data_node_ids,
    );

    Ok(summary)
}

// --- edge scaling --------------------------------------------------------

/// Plain container-replica-count change on the cluster's routing edge (e.g.
/// HAProxy) -- `deploy.multiRegionConfig[region].numReplicas`. Not a new
/// service case: mirrors `useScaleHACluster.tsx`'s `scaleEdgeNodes`, which
/// finds the one active region entry and sets its replica count directly.
fn scale_edge(
    config: &EnvironmentConfig,
    engine: &DatabaseEngine,
    root_id: &str,
    target_count: i64,
    patch: &mut BTreeMap<String, ServiceInstance>,
) -> Result<Option<EdgeScaleSummary>> {
    let edge_id = find_routing_edge_id(config, engine, root_id)
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

/// Restamps the root's declared wiring after a replica scale up/down: the
/// joining nodes' own identity variable, the routing edge's data-node list,
/// and the peer list and consensus quorum those joining nodes boot against.
/// Mirrors `useScaleHACluster.tsx`'s `scaleReplicaNodes` restamp step against
/// `EnvironmentConfig`.
///
/// Only the routing edge's list is rebuilt fleet-wide; everything else is
/// stamped on JOINING nodes only, so a scale-down restamps nothing at all.
/// See the quorum block below for what stamping a survivor actually costs.
fn restamp_replica_wiring(
    patch: &mut BTreeMap<String, ServiceInstance>,
    wiring: &ClusterWiring,
    root_name: &str,
    routing_edge_id: Option<&str>,
    replicas_after: &[(String, String)],
    newly_added_ids: &[String],
) {
    // A replica's identity is its own name, which a scale never changes, so
    // survivors already carry the right value -- and the one they carry may
    // be a template reference rather than the literal this would overwrite it
    // with. Stamp the joining nodes, which have no value yet.
    if let Some(var_name) = &wiring.replica_node_name_variable {
        for (id, name) in replicas_after {
            if newly_added_ids.iter().any(|added| added == id) {
                set_patch_var(patch, id, var_name, name.to_ascii_lowercase());
            }
        }
    }

    // Topologies whose coordinator is colocated on the data nodes boot each
    // node against a declared peer list. It is stamped on JOINING nodes only:
    // a node coming up now has to know the real membership at first boot,
    // while every existing node already read its own copy -- rewriting theirs
    // would mark the whole cluster stale for a change none of them needs.
    if let (Some(peer_var), Some(entry_format)) =
        (&wiring.peer_hosts_variable, &wiring.peer_hosts_entry_format)
        && !newly_added_ids.is_empty()
    {
        let mut peer_names: Vec<&str> = std::iter::once(root_name)
            .chain(replicas_after.iter().map(|(_, name)| name.as_str()))
            .collect();
        peer_names.sort_unstable();
        let peer_list = peer_names
            .iter()
            .map(|name| format_data_node_entry(entry_format, name, root_name))
            .collect::<Vec<_>>()
            .join(",");
        for id in newly_added_ids {
            set_patch_var(patch, id, peer_var, peer_list.clone());
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

    // Consensus quorum (e.g. SENTINEL_QUORUM), at a majority of the post-scale
    // data-node set -- on the JOINING nodes only, for the same reason the peer
    // list above is. An existing node reads this env exactly once, on the
    // first boot that writes its coordinator config, and never again: stamping
    // it changes nothing functionally, while the variable edit still marks
    // every node stale and restarts the whole fleet at once, racing the
    // coordinator into a spurious failover mid-scale. Survivors converge at
    // runtime through the image's own quorum-sync watcher instead -- which is
    // also why a scale-DOWN stamps nothing here.
    //
    // On a real cluster the survivors' copy is a reference to the root's
    // (`${{Redis-1.SENTINEL_QUORUM}}`), so overwriting it with a literal was
    // not even a no-op edit -- it detached them from the root's value.
    if let Some(quorum_var) = &wiring.quorum_variable
        && !newly_added_ids.is_empty()
    {
        let data_node_count = replicas_after.len() + 1; // + root
        let quorum = (data_node_count / 2 + 1).to_string();
        for id in newly_added_ids {
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
    newly_added_ids: &[String],
    data_node_ids: &[String],
) {
    // Identity on JOINING nodes only, for the same reason the replica path
    // stamps its identity that way: a node's identity is its own name, which
    // a scale never changes, so restamping a running coordinator only marks
    // it stale -- and coordinators restarting together is quorum loss.
    if let Some(var_name) = &wiring.internal_node_name_variable {
        for (id, name) in internal_after {
            if newly_added_ids.iter().any(|added| added == id) {
                set_patch_var(patch, id, var_name, name.to_ascii_lowercase());
            }
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
        ..ClusterWiring::default()
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
/// children -- excludes a stacked pooler edge, which also carries
/// `clusterRole == "edge"` under the same root, identified through the
/// engine's own declared pooling spec rather than an image name compiled in
/// here. An engine that ships no pooler has nothing to exclude. Mirrors
/// `useScaleHACluster.tsx`'s `routingEdgeService` filter.
fn find_routing_edge_id(
    config: &EnvironmentConfig,
    engine: &DatabaseEngine,
    root_id: &str,
) -> Option<String> {
    config
        .services
        .iter()
        .find(|(_, s)| {
            s.parent_service_id.as_deref() == Some(root_id)
                && s.cluster_role.as_deref() == Some("edge")
                && !engine
                    .pooling
                    .is_some_and(|pooling| database_plugins::is_pooler_service(s, &pooling))
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
async fn create_clone_service(ctx: &ServiceContext, base_name: &str) -> Result<CreatedNode> {
    let build_vars = |name: String| mutations::service_create::Variables {
        name: Some(name),
        project_id: ctx.project_id.clone(),
        // A bare Service ROW, deployed to no environment -- the instance is
        // created by the staged patch (`isCreated`), because the patch-apply
        // workflow's instance-create path is the only one that persists
        // `parentServiceId`. An instance pre-created here would take the
        // update path at commit, which applies the role but silently drops
        // the parent link, orphaning the member from every parent-chain
        // membership walk. The image rides the patch for the same reason
        // the dashboard's flow passes no source here.
        environment_id: None,
        source: None,
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

/// Creates the VOLUME RECORD for a new node at `mount_path`, best-effort
/// naming it `<node_name>-volume` (retrying with a short unique suffix on a
/// name clash, and simply leaving it unnamed if that also fails -- volume
/// names are cosmetic).
///
/// `environmentId: null` deliberately means "no environment": the record is
/// created bare, and the caller stages the volume INSTANCE in the same patch
/// that stamps the node's `clusterRole`/`parentServiceId`. Passing this
/// environment instead provisioned the instance right here, ahead of the
/// patch and ahead of the role/parent stamps, which is the whole defect --
/// backboard sizes a new replica volume to hold a full base backup of its
/// primary (`resolveNewVolumeInstanceSizeMB`), keyed on exactly that
/// role/parent pair, and an instance created before either exists reads as an
/// ordinary volume and lands on the flat plan default. It also spent a
/// patch-system redeploy per volume on the way. This is the same split the
/// dashboard's `useScaleHACluster` uses.
async fn create_clone_volume(
    ctx: &ServiceContext,
    mount_path: &str,
    node_name: &str,
) -> Result<CreatedVolume> {
    let created = post_graphql::<mutations::VolumeCreate, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::volume_create::Variables {
            project_id: ctx.project_id.clone(),
            environment_id: None,
            // Detached like the service record: the mount is declared by the
            // staged patch's volumeMounts, alongside the instance creation.
            service_id: None,
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
    fn next_node_number_starts_at_one_from_empty() {
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

        use crate::controllers::database_engines::POSTGRES;
        assert_eq!(
            find_routing_edge_id(&config, &POSTGRES, "root"),
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

    fn replica_scale_wiring() -> ClusterWiring {
        ClusterWiring {
            replica_node_name_variable: Some("PATRONI_NAME".to_string()),
            data_nodes_variable: Some("POSTGRES_NODES".to_string()),
            data_nodes_entry_format: Some("{host}:${{{rootName}.PGPORT}}:8008".to_string()),
            quorum_variable: Some("QUORUM".to_string()),
            ..ClusterWiring::default()
        }
    }

    #[test]
    fn restamp_replica_wiring_stamps_identity_and_quorum_on_joining_nodes_only() {
        let wiring = replica_scale_wiring();
        // 1 -> 2 replicas: r1 survives the scale, r2 is joining.
        let replicas = vec![
            ("r1".to_string(), "postgres-replica-1".to_string()),
            ("r2".to_string(), "postgres-replica-2".to_string()),
        ];
        let mut patch = BTreeMap::new();

        restamp_replica_wiring(
            &mut patch,
            &wiring,
            "db-prod",
            Some("edge"),
            &replicas,
            &["r2".to_string()],
        );

        // The joining node gets its identity and the post-scale quorum.
        assert_eq!(
            patch["r2"].variables["PATRONI_NAME"]
                .as_ref()
                .unwrap()
                .value
                .as_deref(),
            Some("postgres-replica-2")
        );
        assert_eq!(
            patch["r2"].variables["QUORUM"]
                .as_ref()
                .unwrap()
                .value
                .as_deref(),
            Some("2")
        );

        // The survivor and the root are not touched at all. Editing either
        // one's variables marks it stale and restarts it -- a whole-fleet
        // restart mid-scale is what races the coordinator into a spurious
        // failover, and the value it would be "corrected" to is one the
        // running node never reads again anyway.
        assert!(!patch.contains_key("r1"));
        assert!(!patch.contains_key("root"));

        // The routing edge's list is the one thing rebuilt fleet-wide: it is
        // read per connection, not once at boot.
        let edge_list = patch["edge"].variables["POSTGRES_NODES"]
            .as_ref()
            .unwrap()
            .value
            .clone()
            .unwrap();
        assert!(edge_list.contains("db-prod"));
        assert_eq!(edge_list.split(',').count(), 3);
    }

    #[test]
    fn restamp_replica_wiring_touches_no_node_on_a_scale_down() {
        let wiring = replica_scale_wiring();
        // 3 -> 2 replicas: nothing is joining, so nothing is stamped. The
        // survivors' quorum converges through the image's own quorum-sync
        // watcher as the removed nodes drop out.
        let replicas = vec![
            ("r1".to_string(), "postgres-replica-1".to_string()),
            ("r2".to_string(), "postgres-replica-2".to_string()),
        ];
        let mut patch = BTreeMap::new();

        restamp_replica_wiring(&mut patch, &wiring, "db-prod", Some("edge"), &replicas, &[]);

        assert_eq!(patch.keys().collect::<Vec<_>>(), vec!["edge"]);
        assert!(patch["edge"].variables.contains_key("POSTGRES_NODES"));
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

        // e3 is the node this scale-up added; running coordinators keep
        // their identity (restarting them together is quorum loss).
        restamp_internal_wiring(
            &mut patch,
            &wiring,
            &internal,
            &["e3".to_string()],
            &["root".to_string()],
        );

        assert_eq!(
            patch["e3"].variables["ETCD_NAME"]
                .as_ref()
                .unwrap()
                .value
                .as_deref(),
            Some("etcd-3")
        );
        // The coordinators already running keep their identity: restamping
        // them would mark every one stale, and coordinators restarting
        // together is quorum loss.
        assert!(!patch.contains_key("e1"));
        assert!(!patch.contains_key("e2"));
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

        let summary = scale_edge(
            &config,
            &crate::controllers::database_engines::POSTGRES,
            "root",
            3,
            &mut patch,
        )
        .unwrap()
        .unwrap();
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

        let summary = scale_edge(
            &config,
            &crate::controllers::database_engines::POSTGRES,
            "root",
            2,
            &mut patch,
        )
        .unwrap();
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
        let err = scale_edge(
            &config,
            &crate::controllers::database_engines::POSTGRES,
            "root",
            2,
            &mut patch,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no multi-region config"));
    }

    #[test]
    fn scale_edge_errors_without_an_edge_service() {
        let config = config_with(vec![("replica-1", service("replica", "root"))]);
        let mut patch = BTreeMap::new();
        let err = scale_edge(
            &config,
            &crate::controllers::database_engines::POSTGRES,
            "root",
            2,
            &mut patch,
        )
        .unwrap_err();
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
            "db",
            Some("edge"),
            &[("r1".to_string(), "replica-1".to_string())],
            &["r1".to_string()],
        );
        assert!(patch.is_empty());
    }

    #[test]
    fn peer_list_is_stamped_on_joining_nodes_only() {
        let wiring = ClusterWiring {
            peer_hosts_variable: Some("SENTINEL_HOSTS".to_string()),
            peer_hosts_entry_format: Some("{host}:26379".to_string()),
            ..ClusterWiring::default()
        };
        let replicas = vec![
            ("r1".to_string(), "Redis-2".to_string()),
            ("r2".to_string(), "Redis-3".to_string()),
        ];
        let mut patch = BTreeMap::new();

        restamp_replica_wiring(
            &mut patch,
            &wiring,
            "Redis-1",
            None,
            &replicas,
            &["r2".to_string()],
        );

        // The joining node learns the full membership at first boot...
        let peers = patch["r2"].variables["SENTINEL_HOSTS"]
            .as_ref()
            .unwrap()
            .value
            .as_ref()
            .unwrap();
        assert_eq!(peers.split(',').count(), 3);
        assert!(peers.contains("${{Redis-1.RAILWAY_PRIVATE_DOMAIN}}:26379"));

        // ...while the nodes already running are left alone: they read their
        // own copy at their own first boot, and restamping would only mark
        // them stale for a change they do not need to see.
        assert!(
            patch
                .get("r1")
                .is_none_or(|p| !p.variables.contains_key("SENTINEL_HOSTS"))
        );
        assert!(
            patch
                .get("root")
                .is_none_or(|p| !p.variables.contains_key("SENTINEL_HOSTS"))
        );
    }

    #[test]
    fn scale_down_stamps_no_peer_list_at_all() {
        let wiring = ClusterWiring {
            peer_hosts_variable: Some("GR_SEEDS".to_string()),
            peer_hosts_entry_format: Some("{host}:3306".to_string()),
            ..ClusterWiring::default()
        };
        let mut patch = BTreeMap::new();
        restamp_replica_wiring(
            &mut patch,
            &wiring,
            "MySQL-1",
            None,
            &[("r1".to_string(), "MySQL-2".to_string())],
            &[],
        );
        assert!(patch.is_empty());
    }

    #[test]
    fn data_node_quorum_fence_applies_only_where_the_data_nodes_vote() {
        // Declaring the quorum variable to restamp means the data nodes vote.
        let sentinel = ClusterWiring {
            quorum_variable: Some("SENTINEL_QUORUM".to_string()),
            ..ClusterWiring::default()
        };
        // So does declaring that the coordinator derives its own majority.
        let group_replication = ClusterWiring {
            data_nodes_are_quorum_voters: Some(true),
            ..ClusterWiring::default()
        };

        for wiring in [&sentinel, &group_replication] {
            // --replicas counts nodes beside the primary, so even is what
            // makes the cluster odd.
            assert!(validate_data_node_quorum(wiring, 2).is_ok());
            assert!(validate_data_node_quorum(wiring, 4).is_ok());

            // An odd replica count leaves an even cluster, which cannot
            // elect a primary after losing a node.
            assert!(validate_data_node_quorum(wiring, 3).is_err());
            // Two data nodes cannot hold a majority either.
            assert!(validate_data_node_quorum(wiring, 1).is_err());
            assert!(validate_data_node_quorum(wiring, 0).is_err());
        }

        // A cluster with a separate coordinator tier (etcd) carries its
        // quorum there, so its replica count is unconstrained.
        let external_coordinator = ClusterWiring {
            coordinator_hosts_variable: Some("PATRONI_ETCD3_HOSTS".to_string()),
            ..ClusterWiring::default()
        };
        for replicas in [0, 1, 2, 3] {
            assert!(validate_data_node_quorum(&external_coordinator, replicas).is_ok());
        }
    }

    /// A `ServiceContext` pointed at a stub backboard.
    fn mock_context(
        server: &crate::testkit::MockBackboard,
        dir: &tempfile::TempDir,
    ) -> ServiceContext {
        ServiceContext {
            client: reqwest::Client::new(),
            configs: server.configs(dir),
            project: serde_json::from_value(serde_json::json!({
                "id": "proj-1",
                "name": "db",
                "workspaceId": null,
                "deletedAt": null,
                "workspace": null,
                "buckets": { "edges": [] },
                "environments": { "edges": [] },
                "services": { "edges": [] },
            }))
            .unwrap(),
            project_id: "proj-1".to_string(),
            environment_id: "env-1".to_string(),
            environment_name: "production".to_string(),
            service_id: "root".to_string(),
            service_name: "Redis-1".to_string(),
        }
    }

    #[tokio::test]
    async fn scaling_up_stages_the_new_replica_volume_in_the_same_patch_as_its_role_and_parent() {
        let dir = tempfile::tempdir().unwrap();
        let server = crate::testkit::MockBackboard::spawn();

        let environment_config = serde_json::json!({
            "services": {
                "root": {
                    "source": { "image": "ghcr.io/railwayapp-templates/redis-ha/redis-sentinel:8.4" },
                    "clusterRole": "root",
                    "clusterWiring": {
                        "quorumVariable": "SENTINEL_QUORUM",
                        "peerHostsVariable": "SENTINEL_HOSTS",
                        "peerHostsEntryFormat": "{host}:26379",
                    },
                },
                "replica-1": {
                    "source": { "image": "ghcr.io/railwayapp-templates/redis-ha/redis-sentinel:8.4" },
                    "clusterRole": "replica",
                    "parentServiceId": "root",
                    "volumeMounts": { "vol-1": { "mountPath": "/data" } },
                },
                "replica-2": {
                    "source": { "image": "ghcr.io/railwayapp-templates/redis-ha/redis-sentinel:8.4" },
                    "clusterRole": "replica",
                    "parentServiceId": "root",
                    "volumeMounts": { "vol-2": { "mountPath": "/data" } },
                },
            }
        });
        let environment_payload = serde_json::json!({
            "environment": { "id": "env-1", "name": "production", "config": environment_config }
        });

        server.stub("GetEnvironmentConfig", environment_payload.clone());
        // 2 -> 4 replicas: the data nodes carry the failover vote here, so
        // only an odd total (5) clears the quorum fence.
        server.stub(
            "ServiceCreate",
            serde_json::json!({
                "serviceCreate": { "id": "replica-3", "name": "Redis-4" }
            }),
        );
        server.stub(
            "ServiceCreate",
            serde_json::json!({
                "serviceCreate": { "id": "replica-4", "name": "Redis-5" }
            }),
        );
        server.stub(
            "VolumeCreate",
            serde_json::json!({
                "volumeCreate": { "id": "vol-3", "name": "Redis-4-volume" }
            }),
        );
        server.stub(
            "VolumeCreate",
            serde_json::json!({
                "volumeCreate": { "id": "vol-4", "name": "Redis-5-volume" }
            }),
        );
        server.stub(
            "VolumeNameUpdate",
            serde_json::json!({ "volumeUpdate": { "name": "Redis-4-volume" } }),
        );
        server.stub(
            "EnvironmentStagedChanges",
            serde_json::json!({
                "environmentStagedChanges": { "id": "patch-0", "status": "STAGED", "patch": null }
            }),
        );
        server.stub(
            "EnvironmentStageChanges",
            serde_json::json!({
                "environmentStageChanges": { "id": "patch-1", "status": "STAGED" }
            }),
        );
        server.stub(
            "EnvironmentPatchCommitStaged",
            serde_json::json!({
                "environmentPatchCommitStaged": "wf-1"
            }),
        );
        server.stub(
            "WorkflowStatus",
            serde_json::json!({
                "workflowStatus": { "status": "Complete", "error": null }
            }),
        );

        let ctx = mock_context(&server, &dir);
        let names = names(&[
            ("root", "Redis-1"),
            ("replica-1", "Redis-2"),
            ("replica-2", "Redis-3"),
        ]);

        scale_cluster(
            &ctx,
            &crate::controllers::database_engines::REDIS,
            "root",
            "Redis-1",
            &names,
            ScaleClusterParams {
                replicas: Some(4),
                coordinators: None,
                edge: None,
                auto_deploy: false,
                live_primary_id: None,
            },
        )
        .await
        .unwrap();

        // The volume RECORD is created with no environment: creating the
        // instance here would put it outside the patch, ahead of the
        // clusterRole/parentServiceId stamps that size it against the primary.
        let volume_create = server.variables_for("VolumeCreate");
        assert_eq!(volume_create.len(), 2);
        for variables in &volume_create {
            assert_eq!(
                variables.get("environmentId"),
                Some(&serde_json::Value::Null),
                "the volume instance must not be provisioned outside the patch"
            );
            assert_eq!(
                variables.get("serviceId"),
                Some(&serde_json::Value::Null),
                "the mount is declared by the staged patch, not the record"
            );
        }

        // The service RECORD too: an instance pre-created by serviceCreate
        // takes the patch-apply UPDATE path at commit, which applies the
        // role but silently drops parentServiceId -- the member must be
        // created BY the patch for the parent link to persist.
        let service_create = server.variables_for("ServiceCreate");
        assert_eq!(service_create.len(), 2);
        for variables in &service_create {
            assert_eq!(
                variables.get("environmentId"),
                Some(&serde_json::Value::Null),
                "the service instance must be created by the patch, not here"
            );
            assert_eq!(variables.get("source"), Some(&serde_json::Value::Null));
        }

        // The instance is created BY the staged patch instead, in the same
        // input that carries the node's role and parent -- which is what
        // `resolveNewVolumeInstanceSizeMB` reads to match the primary's size.
        let staged = server.variables_for("EnvironmentStageChanges");
        assert_eq!(staged.len(), 1);
        let input = staged[0].get("input").unwrap();
        for volume_id in ["vol-3", "vol-4"] {
            assert_eq!(
                input.pointer(&format!("/volumes/{volume_id}/isCreated")),
                Some(&serde_json::Value::Bool(true)),
                "{volume_id} was not staged for creation by the patch"
            );
        }
        for (service_id, volume_id) in [("replica-3", "vol-3"), ("replica-4", "vol-4")] {
            assert_eq!(
                input.pointer(&format!("/services/{service_id}/isCreated")),
                Some(&serde_json::Value::Bool(true)),
                "{service_id}'s instance was not staged for creation by the patch"
            );
            assert_eq!(
                input.pointer(&format!("/services/{service_id}/source/image")),
                Some(&serde_json::Value::String(
                    "ghcr.io/railwayapp-templates/redis-ha/redis-sentinel:8.4".to_string()
                )),
                "the image rides the patch now that the record is created bare"
            );
            assert_eq!(
                input.pointer(&format!("/services/{service_id}/clusterRole")),
                Some(&serde_json::Value::String("replica".to_string()))
            );
            assert_eq!(
                input.pointer(&format!("/services/{service_id}/parentServiceId")),
                Some(&serde_json::Value::String("root".to_string()))
            );
            assert_eq!(
                input.pointer(&format!(
                    "/services/{service_id}/volumeMounts/{volume_id}/mountPath"
                )),
                Some(&serde_json::Value::String("/data".to_string()))
            );
        }

        // And the surviving nodes are still not restamped (see
        // `restamp_replica_wiring`): only the joining node carries quorum.
        assert_eq!(
            input.pointer("/services/replica-3/variables/SENTINEL_QUORUM/value"),
            Some(&serde_json::Value::String("3".to_string()))
        );
        for survivor in ["replica-1", "replica-2", "root"] {
            assert!(
                input
                    .pointer(&format!("/services/{survivor}/variables/SENTINEL_QUORUM"))
                    .is_none(),
                "{survivor} was restamped and will restart with the rest of the fleet"
            );
        }
    }

    #[tokio::test]
    async fn scaling_down_stages_deletions_and_never_removes_the_acting_primary() {
        let dir = tempfile::tempdir().unwrap();
        let server = crate::testkit::MockBackboard::spawn();

        // A Postgres-shaped cluster (external etcd quorum, so 2 -> 1 replicas
        // clears the fence) whose PRIMARY failed over onto the
        // highest-numbered replica -- the one deletion-by-number picks first.
        let environment_config = serde_json::json!({
            "services": {
                "root": {
                    "source": { "image": "ghcr.io/railwayapp-templates/postgres-ha/postgres-patroni:16" },
                    "clusterRole": "root",
                    "clusterWiring": {
                        "replicaNodeNameVariable": "PATRONI_NAME",
                    },
                },
                "replica-1": {
                    "source": { "image": "ghcr.io/railwayapp-templates/postgres-ha/postgres-patroni:16" },
                    "clusterRole": "replica",
                    "parentServiceId": "root",
                    "volumeMounts": { "vol-1": { "mountPath": "/var/lib/postgresql/data" } },
                },
                "replica-2": {
                    "source": { "image": "ghcr.io/railwayapp-templates/postgres-ha/postgres-patroni:16" },
                    "clusterRole": "replica",
                    "parentServiceId": "root",
                    "volumeMounts": { "vol-2": { "mountPath": "/var/lib/postgresql/data" } },
                },
            }
        });
        server.stub(
            "GetEnvironmentConfig",
            serde_json::json!({
                "environment": { "id": "env-1", "name": "production", "config": environment_config }
            }),
        );
        server.stub(
            "EnvironmentStagedChanges",
            serde_json::json!({
                "environmentStagedChanges": { "id": "patch-0", "status": "STAGED", "patch": null }
            }),
        );
        server.stub(
            "EnvironmentStageChanges",
            serde_json::json!({
                "environmentStageChanges": { "id": "patch-1", "status": "STAGED" }
            }),
        );
        server.stub(
            "EnvironmentPatchCommitStaged",
            serde_json::json!({ "environmentPatchCommitStaged": "wf-1" }),
        );
        server.stub(
            "WorkflowStatus",
            serde_json::json!({
                "workflowStatus": { "status": "Complete", "error": null }
            }),
        );

        let ctx = mock_context(&server, &dir);
        let names = names(&[
            ("root", "Postgres"),
            ("replica-1", "postgres-replica-1"),
            ("replica-2", "postgres-replica-2"),
        ]);

        let result = scale_cluster(
            &ctx,
            &crate::controllers::database_engines::POSTGRES,
            "root",
            "Postgres",
            &names,
            ScaleClusterParams {
                replicas: Some(1),
                coordinators: None,
                edge: None,
                auto_deploy: false,
                // The live probe found the acting primary on replica-2.
                live_primary_id: Some("replica-2".to_string()),
            },
        )
        .await
        .unwrap();

        // Deletion order is by node number, which would pick replica-2 -- but
        // replica-2 is ACTING as the primary, so replica-1 goes instead.
        assert_eq!(
            result.replicas.unwrap().removed,
            vec!["postgres-replica-1".to_string()]
        );

        // The deletion is STAGED (volume first, then the service), never
        // issued as direct ServiceDelete/VolumeDelete calls: the whole scale
        // commits atomically and the platform's cluster-primacy commit guard
        // inspects the patch. An unstubbed direct delete would have failed
        // this test loudly.
        let staged = server.variables_for("EnvironmentStageChanges");
        assert_eq!(staged.len(), 1);
        let input = staged[0].get("input").unwrap();
        assert_eq!(
            input.pointer("/services/replica-1/isDeleted"),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            input.pointer("/volumes/vol-1/isDeleted"),
            Some(&serde_json::Value::Bool(true))
        );
        assert!(input.pointer("/services/replica-2/isDeleted").is_none());
        assert!(input.pointer("/volumes/vol-2").is_none());
    }

    #[tokio::test]
    async fn scaling_down_past_the_acting_primary_is_refused_with_the_switchover_remedy() {
        let dir = tempfile::tempdir().unwrap();
        let server = crate::testkit::MockBackboard::spawn();

        // One replica left, and it is the acting primary: honoring the count
        // would require deleting it, so the scale must refuse instead.
        let environment_config = serde_json::json!({
            "services": {
                "root": {
                    "source": { "image": "ghcr.io/railwayapp-templates/postgres-ha/postgres-patroni:16" },
                    "clusterRole": "root",
                    "clusterWiring": { "replicaNodeNameVariable": "PATRONI_NAME" },
                },
                "replica-1": {
                    "source": { "image": "ghcr.io/railwayapp-templates/postgres-ha/postgres-patroni:16" },
                    "clusterRole": "replica",
                    "parentServiceId": "root",
                    "volumeMounts": { "vol-1": { "mountPath": "/var/lib/postgresql/data" } },
                },
            }
        });
        server.stub(
            "GetEnvironmentConfig",
            serde_json::json!({
                "environment": { "id": "env-1", "name": "production", "config": environment_config }
            }),
        );

        let ctx = mock_context(&server, &dir);
        let names = names(&[("root", "Postgres"), ("replica-1", "postgres-replica-1")]);

        let err = scale_cluster(
            &ctx,
            &crate::controllers::database_engines::POSTGRES,
            "root",
            "Postgres",
            &names,
            ScaleClusterParams {
                replicas: Some(0),
                coordinators: None,
                edge: None,
                auto_deploy: false,
                live_primary_id: Some("replica-1".to_string()),
            },
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("acting as the cluster's primary"));
        assert!(err.contains("ha switchover --to Postgres"));
    }
}
