//! Template apply/revert controller shared by `railway postgres
//! {pitr,ha,pgbouncer} {enable,disable,convert,revert,add,remove}`.
//!
//! Ports the reference logic in the frontend's
//! `src/hooks/useApplyComposableTemplate.tsx` (roughly lines 174-499 and
//! 777-830 at the time this was written): fetch a template's
//! `serializedConfig`, adjust replica/coordinator/edge counts and inject edge
//! variables *inside that JSON* before submitting, then deploy via
//! `templateDeployV2`/`templateRevert` (always `stageOnly: true`, matching
//! frontend behavior) and auto-deploy by committing the resulting staged
//! patch unless the caller asked to skip that.
//!
//! Skipping the count/variable adjustment step would silently deploy the
//! template's authored default topology instead of what the user asked for
//! (e.g. `railway postgres ha convert --replicas 3` would ignore `--replicas`
//! entirely), so every `apply_composable_template` caller that cares about
//! topology must pass the relevant `Some(count)`.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::{
    client::post_graphql,
    controllers::{config::EnvironmentConfig, project::ServiceContext},
    gql::{mutations, queries},
};

/// Built-in Railway template codes the three `railway postgres` features
/// deploy/revert.
pub const PITR_TEMPLATE_CODE: &str = "postgres-pitr";
pub const HA_TEMPLATE_CODE: &str = "postgres-ha";
pub const PGBOUNCER_TEMPLATE_CODE: &str = "postgres-with-pgbouncer";

/// Distinguishes a true cluster conversion (HA) from a config-only overlay
/// (PITR: env vars + a bucket, no new services) and an additive edge stack
/// (PgBouncer in front of the database). Only affects whether the
/// pre-apply/pre-conversion safety backup is taken -- mirrors the frontend's
/// `kind` parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyKind {
    Conversion,
    Overlay,
    Stacking,
}

/// Parameters for [`apply_composable_template`].
pub struct ApplyTemplateParams {
    pub template_code: String,
    /// The existing service to convert into (or overlay onto) the cluster root.
    pub service_id: String,
    /// The service's volume instance id, used only for the pre-conversion
    /// safety backup (`kind == Conversion`). `None` skips the backup (e.g. the
    /// service has no volume yet).
    pub volume_instance_id: Option<String>,
    /// Omit (`None`) to leave the template's authored replica count untouched.
    pub replica_count: Option<i64>,
    /// Omit (`None`) to leave the template's authored coordinator/internal
    /// node count untouched.
    pub internal_count: Option<i64>,
    /// Omit (`None`) to leave the template's authored edge replica count
    /// untouched.
    pub edge_count: Option<i64>,
    /// Variable overrides stamped onto every `edge`-role service before
    /// deploy (e.g. `POOL_MODE` for PgBouncer).
    pub edge_variables: Option<BTreeMap<String, String>>,
    pub kind: ApplyKind,
    /// Commit and deploy the resulting staged patch immediately. When
    /// `false`, the patch is still committed (so the topology change lands)
    /// but deploys are skipped -- matches `environmentPatchCommitStaged`'s
    /// `skipDeploys` toggle.
    pub auto_deploy: bool,
}

/// Parameters for [`revert_template`].
pub struct RevertTemplateParams {
    pub template_code: String,
    pub root_service_id: String,
    pub auto_deploy: bool,
}

pub struct ApplyTemplateResult {
    pub project_id: String,
    /// `true` if the staged patch was committed with deploys enabled.
    pub deployed: bool,
}

/// True when a staged environment patch actually carries changes (vs. the
/// empty placeholder patch `environmentStagedChanges` returns when nothing
/// is staged).
pub(crate) fn staged_patch_is_nonempty(patch: &Value) -> bool {
    match patch {
        Value::Object(map) => map.values().any(|v| match v {
            Value::Object(m) => !m.is_empty(),
            Value::Null => false,
            _ => true,
        }),
        _ => false,
    }
}

/// Every mutating `railway postgres` action ends by committing the
/// environment's WHOLE staged patch (same semantics as the dashboard's
/// "Apply" button) -- so changes someone staged earlier (dashboard, another
/// CLI session) get committed and deployed together with this one. This
/// best-effort warning makes that visible up front; it never blocks the
/// command (the backend query only returns STAGED/APPLYING patches, so a
/// non-empty result is always genuinely pending work).
pub(crate) async fn warn_if_preexisting_staged_changes(ctx: &ServiceContext) {
    let response = post_graphql::<queries::EnvironmentStagedChanges, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::environment_staged_changes::Variables {
            environment_id: ctx.environment_id.clone(),
        },
    )
    .await;

    if let Ok(response) = response
        && staged_patch_is_nonempty(&response.environment_staged_changes.patch)
    {
        eprintln!(
            "Warning: environment {} already has staged changes; this command commits the environment's full staged patch, so those pre-existing changes will be applied (and deployed) together with this one.",
            ctx.environment_name
        );
    }
}

/// Fetches a template by code, adjusts its `serializedConfig` for the
/// requested replica/internal/edge counts and edge-variable overrides, then
/// deploys it onto `params.service_id` as the existing cluster root.
pub async fn apply_composable_template(
    ctx: &ServiceContext,
    params: ApplyTemplateParams,
) -> Result<ApplyTemplateResult> {
    warn_if_preexisting_staged_changes(ctx).await;

    // Best-effort pre-conversion safety backup via the public backup-create
    // mutation (the dashboard's dedicated `volumeInstanceBackupCreateForHaConversion`
    // is Internal-subgraph only; a plain named on-demand backup covers the
    // same "escape hatch before topology surgery" purpose).
    if params.kind == ApplyKind::Conversion
        && let Some(volume_instance_id) = params.volume_instance_id.clone()
    {
        if let Err(err) = post_graphql::<mutations::VolumeInstanceBackupCreate, _>(
            &ctx.client,
            ctx.configs.get_backboard(),
            mutations::volume_instance_backup_create::Variables {
                volume_instance_id,
                name: Some("pre-ha-conversion".to_string()),
            },
        )
        .await
        {
            eprintln!(
                "Warning: could not create pre-conversion backup: {err:#}. Proceeding with conversion."
            );
        }
    }

    let detail = post_graphql::<queries::TemplateDetail, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::template_detail::Variables {
            code: params.template_code.clone(),
        },
    )
    .await
    .with_context(|| format!("Failed to fetch template \"{}\"", params.template_code))?;

    let mut config =
        detail.template.serialized_config.clone().with_context(|| {
            format!("Template \"{}\" has no configuration", params.template_code)
        })?;

    if let Some(target) = params.replica_count {
        adjust_replica_count(&mut config, target);
    }
    if let Some(target) = params.internal_count {
        adjust_internal_count(&mut config, target);
    }
    if let Some(target) = params.edge_count {
        adjust_edge_count(&mut config, target);
    }
    if let Some(overrides) = &params.edge_variables {
        apply_edge_variables(&mut config, overrides);
    }

    let response = post_graphql::<mutations::TemplateDeployV2, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::template_deploy_v2::Variables {
            input: mutations::template_deploy_v2::TemplateDeployV2Input {
                environment_id: Some(ctx.environment_id.clone()),
                existing_root_service_id: Some(params.service_id.clone()),
                group_id: None,
                project_id: Some(ctx.project_id.clone()),
                project_name: None,
                serialized_config: config,
                stage_only: Some(true),
                template_id: detail.template.id.clone(),
                workspace_id: ctx.project.workspace_id.clone(),
            },
        },
    )
    .await
    .with_context(|| format!("Failed to deploy template \"{}\"", params.template_code))?;

    if let Some(workflow_id) = response.template_deploy_v2.workflow_id.clone() {
        crate::controllers::workflow::wait_for_workflow(&ctx.client, &ctx.configs, workflow_id)
            .await?;
    }

    let deployed = commit_staged_patch(ctx, params.auto_deploy).await?;

    Ok(ApplyTemplateResult {
        project_id: response.template_deploy_v2.project_id,
        deployed,
    })
}

/// Reverts a cluster/overlay/stack to standalone via `templateRevert`, using
/// template metadata server-side to derive which variables/services to
/// remove.
pub async fn revert_template(
    ctx: &ServiceContext,
    params: RevertTemplateParams,
) -> Result<ApplyTemplateResult> {
    warn_if_preexisting_staged_changes(ctx).await;

    let response = post_graphql::<mutations::TemplateRevert, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::template_revert::Variables {
            input: mutations::template_revert::TemplateRevertInput {
                environment_id: ctx.environment_id.clone(),
                group_id: None,
                project_id: ctx.project_id.clone(),
                root_service_id: params.root_service_id,
                stage_only: Some(true),
                template_code: params.template_code.clone(),
            },
        },
    )
    .await
    .with_context(|| format!("Failed to revert template \"{}\"", params.template_code))?;

    if let Some(workflow_id) = response.template_revert.workflow_id.clone() {
        crate::controllers::workflow::wait_for_workflow(&ctx.client, &ctx.configs, workflow_id)
            .await?;
    }

    let deployed = commit_staged_patch(ctx, params.auto_deploy).await?;

    Ok(ApplyTemplateResult {
        project_id: response.template_revert.project_id,
        deployed,
    })
}

/// Stages an arbitrary `EnvironmentConfig` patch (merged into whatever's
/// already staged, if anything) and commits it, honoring `auto_deploy` the
/// same way [`apply_composable_template`]/[`revert_template`] do. Used by
/// `railway postgres pgbouncer {configure,scale}` -- both PgBouncer knob
/// edits (`variables`) and replica-count edits (`deploy.multiRegionConfig`)
/// go through this same two-call stage-then-commit mechanism, so one code
/// path handles both consistently and respects `--no-deploy`.
pub async fn stage_and_commit_patch(
    ctx: &ServiceContext,
    patch: EnvironmentConfig,
    auto_deploy: bool,
) -> Result<bool> {
    warn_if_preexisting_staged_changes(ctx).await;

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
    .context("Failed to stage changes")?;

    commit_staged_patch(ctx, auto_deploy).await
}

/// Commits the environment's currently-staged patch (created by the
/// `templateDeployV2`/`templateRevert` workflow above, by
/// [`stage_and_commit_patch`]'s explicit stage call, or by `cluster_scale`'s
/// own `environmentStageChanges` call), optionally skipping the deploy
/// trigger. Returns whether deploys ran.
pub(crate) async fn commit_staged_patch(ctx: &ServiceContext, auto_deploy: bool) -> Result<bool> {
    post_graphql::<mutations::EnvironmentPatchCommitStaged, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::environment_patch_commit_staged::Variables {
            environment_id: ctx.environment_id.clone(),
            commit_message: None,
            skip_deploys: Some(!auto_deploy),
        },
    )
    .await
    .context("Failed to commit staged changes")?;
    Ok(auto_deploy)
}

// --- serializedConfig JSON manipulation -------------------------------------
//
// `serializedConfig` is an opaque JSON blob (`SerializedTemplateConfig`) with
// many fields the CLI never needs to understand (source, networking, build,
// icon, ...). Everything below operates directly on `serde_json::Value`
// rather than deserializing into a strict struct, so untouched fields
// round-trip byte-for-byte instead of being silently dropped.

struct ClusterWiring {
    internal_node_name_variable: Option<String>,
    coordinator_hosts_variable: Option<String>,
    coordinator_port: Option<i64>,
    replica_node_name_variable: Option<String>,
    data_nodes_variable: Option<String>,
    data_nodes_entry_format: Option<String>,
    quorum_variable: Option<String>,
}

fn services_obj(config: &Value) -> Option<&Map<String, Value>> {
    config.get("services").and_then(Value::as_object)
}

fn services_obj_mut(config: &mut Value) -> Option<&mut Map<String, Value>> {
    config.get_mut("services").and_then(Value::as_object_mut)
}

fn role_of(service: &Value) -> Option<&str> {
    service.get("clusterRole").and_then(Value::as_str)
}

fn name_of(service: &Value) -> Option<&str> {
    service.get("name").and_then(Value::as_str)
}

/// Trailing digits at the end of a name, e.g. `"postgres-replica-2"` -> `2`.
fn trailing_number(name: &str) -> i64 {
    let re = Regex::new(r"(\d+)$").expect("valid regex");
    re.captures(name)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0)
}

/// Strips a trailing `-<digits>` suffix, e.g. `"postgres-replica-2"` ->
/// `"postgres-replica"`.
fn strip_trailing_dash_number(name: &str) -> String {
    let re = Regex::new(r"-\d+$").expect("valid regex");
    re.replace(name, "").to_string()
}

/// `${{<name>.RAILWAY_PRIVATE_DOMAIN}}` variable reference syntax.
pub(crate) fn private_domain_ref(name: &str) -> String {
    let mut out = String::from("${{");
    out.push_str(name);
    out.push_str(".RAILWAY_PRIVATE_DOMAIN}}");
    out
}

/// Substitutes `{host}`/`{rootName}` placeholders in a template-declared
/// data-node entry format. Shared verbatim with `cluster_scale` (live
/// replica/coordinator scaling) -- the substitution rules are identical
/// whether the wiring came from a freshly-fetched template's JSON or an
/// already-converted cluster's live `ClusterWiring`.
pub(crate) fn format_data_node_entry(format: &str, service_name: &str, root_name: &str) -> String {
    format
        .replace("{host}", &private_domain_ref(service_name))
        .replace("{rootName}", root_name)
}

/// Clones a template service with a new name suffix, rekeying its volume
/// mounts so cloned services don't reuse the same volume id.
fn clone_service(service: &Value, name_suffix: &str) -> Value {
    let mut cloned = service.clone();
    let Some(obj) = cloned.as_object_mut() else {
        return cloned;
    };

    if let Some(name) = obj.get("name").and_then(Value::as_str) {
        let base = strip_trailing_dash_number(name);
        obj.insert("name".to_string(), json!(format!("{base}-{name_suffix}")));
    }

    if let Some(volume_mounts) = obj.get("volumeMounts").and_then(Value::as_object).cloned() {
        let mut rekeyed = Map::new();
        for (volume_id, mount) in volume_mounts {
            rekeyed.insert(format!("{volume_id}-{name_suffix}"), mount);
        }
        obj.insert("volumeMounts".to_string(), Value::Object(rekeyed));
    }

    cloned
}

fn set_template_var(service: &mut Value, name: &str, value: String) {
    let Some(obj) = service.as_object_mut() else {
        return;
    };
    let vars = obj
        .entry("variables")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(vars_obj) = vars.as_object_mut() {
        vars_obj.insert(name.to_string(), json!({ "defaultValue": value }));
    }
}

/// Root's declared `clusterWiring`, falling back to the historical hardcoded
/// Patroni wiring for legacy clusters that only set `PATRONI_ENABLED`.
fn resolve_cluster_wiring(root: &Value) -> Option<ClusterWiring> {
    if let Some(wiring) = root.get("clusterWiring").filter(|v| !v.is_null()) {
        return Some(ClusterWiring {
            internal_node_name_variable: wiring
                .get("internalNodeNameVariable")
                .and_then(Value::as_str)
                .map(String::from),
            coordinator_hosts_variable: wiring
                .get("coordinatorHostsVariable")
                .and_then(Value::as_str)
                .map(String::from),
            coordinator_port: wiring.get("coordinatorPort").and_then(Value::as_i64),
            replica_node_name_variable: wiring
                .get("replicaNodeNameVariable")
                .and_then(Value::as_str)
                .map(String::from),
            data_nodes_variable: wiring
                .get("dataNodesVariable")
                .and_then(Value::as_str)
                .map(String::from),
            data_nodes_entry_format: wiring
                .get("dataNodesEntryFormat")
                .and_then(Value::as_str)
                .map(String::from),
            quorum_variable: wiring
                .get("quorumVariable")
                .and_then(Value::as_str)
                .map(String::from),
        });
    }

    let has_patroni_var = root
        .get("variables")
        .and_then(Value::as_object)
        .is_some_and(|vars| vars.contains_key("PATRONI_ENABLED"));
    if !has_patroni_var {
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

fn find_root(config: &Value) -> Option<(String, Value)> {
    services_obj(config)?
        .iter()
        .find(|(_, s)| role_of(s) == Some("root"))
        .map(|(id, s)| (id.clone(), s.clone()))
}

/// Scales the `replica` role to `target_count` (excluding the root), cloning
/// or removing the lowest-numbered replicas, then restamps the root's
/// declared wiring (per-replica identity var, the edge's data-nodes list,
/// and consensus quorum) if any is declared. No-ops if the template has no
/// `replica`-role service.
pub fn adjust_replica_count(config: &mut Value, target_count: i64) {
    let Some((template_service, mut existing, current_count, existing_numbers)) = (|| {
        let services = services_obj(config)?;
        let (_, template_service) = services
            .iter()
            .find(|(_, s)| role_of(s) == Some("replica"))?;
        let template_service = template_service.clone();
        let existing: Vec<(String, Value)> = services
            .iter()
            .filter(|(_, s)| role_of(s) == Some("replica"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let current_count = existing.len() as i64;
        let existing_numbers: Vec<i64> = existing
            .iter()
            .filter_map(|(_, s)| name_of(s))
            .map(trailing_number)
            .filter(|n| *n > 0)
            .collect();
        Some((template_service, existing, current_count, existing_numbers))
    })() else {
        return;
    };

    {
        let Some(services_mut) = services_obj_mut(config) else {
            return;
        };
        if target_count <= current_count {
            existing.sort_by_key(|(_, s)| name_of(s).map(trailing_number).unwrap_or(0));
            for (id, _) in existing.into_iter().skip(target_count.max(0) as usize) {
                services_mut.remove(&id);
            }
        } else {
            let start_number = existing_numbers.into_iter().max().unwrap_or(0) + 1;
            let to_add = target_count - current_count;
            let template_base = name_of(&template_service)
                .map(strip_trailing_dash_number)
                .map(|s| s.to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "replica".to_string());
            let id_prefix = if template_base.contains("replica") {
                template_base
            } else {
                format!("{template_base}-replica")
            };
            for next_number in start_number..start_number + to_add {
                let new_id = format!("{id_prefix}{next_number}");
                services_mut.insert(
                    new_id,
                    clone_service(&template_service, &next_number.to_string()),
                );
            }
        }
    }

    restamp_after_replica_adjust(config);
}

fn restamp_after_replica_adjust(config: &mut Value) {
    let Some((_, root_service)) = find_root(config) else {
        return;
    };
    let Some(wiring) = resolve_cluster_wiring(&root_service) else {
        return;
    };
    let root_name = name_of(&root_service).unwrap_or_default().to_string();

    let Some(services_mut) = services_obj_mut(config) else {
        return;
    };

    if let Some(var_name) = &wiring.replica_node_name_variable {
        let ids: Vec<String> = services_mut
            .iter()
            .filter(|(_, s)| role_of(s) == Some("replica"))
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            let svc_name = services_mut
                .get(&id)
                .and_then(name_of)
                .unwrap_or(&id)
                .to_ascii_lowercase();
            if let Some(svc) = services_mut.get_mut(&id) {
                set_template_var(svc, var_name, svc_name);
            }
        }
    }

    if let (Some(data_var), Some(entry_format)) =
        (&wiring.data_nodes_variable, &wiring.data_nodes_entry_format)
    {
        let edge_id = services_mut
            .iter()
            .find(|(_, s)| role_of(s) == Some("edge"))
            .map(|(k, _)| k.clone());
        if let Some(edge_id) = edge_id {
            let mut data_node_names: Vec<String> = services_mut
                .iter()
                .filter(|(_, s)| matches!(role_of(s), Some("root") | Some("replica")))
                .map(|(_, s)| name_of(s).unwrap_or_default().to_string())
                .collect();
            data_node_names.sort();
            let nodes_list = data_node_names
                .iter()
                .map(|name| format_data_node_entry(entry_format, name, &root_name))
                .collect::<Vec<_>>()
                .join(",");
            if let Some(edge) = services_mut.get_mut(&edge_id) {
                set_template_var(edge, data_var, nodes_list);
            }
        }
    }

    if let Some(quorum_var) = &wiring.quorum_variable {
        let data_node_count = services_mut
            .values()
            .filter(|s| matches!(role_of(s), Some("root") | Some("replica")))
            .count();
        let quorum = (data_node_count / 2 + 1).to_string();
        let ids: Vec<String> = services_mut
            .iter()
            .filter(|(_, s)| matches!(role_of(s), Some("root") | Some("replica")))
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            if let Some(svc) = services_mut.get_mut(&id) {
                set_template_var(svc, quorum_var, quorum.clone());
            }
        }
    }
}

/// Scales the `internal` role (coordinator nodes, e.g. etcd) to
/// `target_count`, then restamps the root's declared per-node identity
/// variable and coordinator hostname list. No-ops if the template has no
/// `internal`-role service.
pub fn adjust_internal_count(config: &mut Value, target_count: i64) {
    let Some((template_service, mut existing, current_count, existing_numbers)) = (|| {
        let services = services_obj(config)?;
        let (_, template_service) = services
            .iter()
            .find(|(_, s)| role_of(s) == Some("internal"))?;
        let template_service = template_service.clone();
        let existing: Vec<(String, Value)> = services
            .iter()
            .filter(|(_, s)| role_of(s) == Some("internal"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let current_count = existing.len() as i64;
        let existing_numbers: Vec<i64> = existing
            .iter()
            .filter_map(|(_, s)| name_of(s))
            .map(trailing_number)
            .filter(|n| *n > 0)
            .collect();
        Some((template_service, existing, current_count, existing_numbers))
    })() else {
        return;
    };

    {
        let Some(services_mut) = services_obj_mut(config) else {
            return;
        };
        if target_count <= current_count {
            existing.sort_by_key(|(_, s)| name_of(s).map(trailing_number).unwrap_or(0));
            for (id, _) in existing.into_iter().skip(target_count.max(0) as usize) {
                services_mut.remove(&id);
            }
        } else {
            let start_number = existing_numbers.into_iter().max().unwrap_or(0) + 1;
            let to_add = target_count - current_count;
            let template_base = name_of(&template_service)
                .map(strip_trailing_dash_number)
                .map(|s| s.to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "internal".to_string());
            for next_number in start_number..start_number + to_add {
                let new_id = format!("{template_base}{next_number}");
                services_mut.insert(
                    new_id,
                    clone_service(&template_service, &next_number.to_string()),
                );
            }
        }
    }

    restamp_after_internal_adjust(config);
}

fn restamp_after_internal_adjust(config: &mut Value) {
    let Some((_, root_service)) = find_root(config) else {
        return;
    };
    let Some(wiring) = resolve_cluster_wiring(&root_service) else {
        return;
    };

    let Some(services_mut) = services_obj_mut(config) else {
        return;
    };

    let mut internal_ids: Vec<String> = services_mut
        .iter()
        .filter(|(_, s)| role_of(s) == Some("internal"))
        .map(|(k, _)| k.clone())
        .collect();
    internal_ids.sort_by_key(|id| {
        services_mut
            .get(id)
            .and_then(name_of)
            .unwrap_or(id)
            .to_string()
    });

    if let Some(var_name) = &wiring.internal_node_name_variable {
        for id in &internal_ids {
            let svc_name = services_mut
                .get(id)
                .and_then(name_of)
                .unwrap_or(id)
                .to_ascii_lowercase();
            if let Some(svc) = services_mut.get_mut(id) {
                set_template_var(svc, var_name, svc_name);
            }
        }
    }

    if let (Some(coordinator_var), Some(port)) =
        (&wiring.coordinator_hosts_variable, wiring.coordinator_port)
    {
        let hosts = internal_ids
            .iter()
            .map(|id| {
                let name = services_mut.get(id).and_then(name_of).unwrap_or(id);
                format!("{}:{port}", private_domain_ref(name))
            })
            .collect::<Vec<_>>()
            .join(",");

        let data_node_ids: Vec<String> = services_mut
            .iter()
            .filter(|(_, s)| matches!(role_of(s), Some("root") | Some("replica")))
            .map(|(k, _)| k.clone())
            .collect();
        for id in data_node_ids {
            if let Some(svc) = services_mut.get_mut(&id) {
                set_template_var(svc, coordinator_var, hosts.clone());
            }
        }
    }
}

/// Sets `numReplicas` on the template's `edge`-role service (e.g. HAProxy) --
/// a single service scaled via Railway's regular replica mechanism, not
/// cloned like `replica`/`internal` roles. No-ops if the template has no
/// `edge`-role service.
pub fn adjust_edge_count(config: &mut Value, target_count: i64) {
    let Some(services_mut) = services_obj_mut(config) else {
        return;
    };
    let Some(edge_id) = services_mut
        .iter()
        .find(|(_, s)| role_of(s) == Some("edge"))
        .map(|(k, _)| k.clone())
    else {
        return;
    };
    let Some(edge) = services_mut.get_mut(&edge_id) else {
        return;
    };
    let Some(obj) = edge.as_object_mut() else {
        return;
    };
    let deploy = obj
        .entry("deploy")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(deploy_obj) = deploy.as_object_mut() {
        deploy_obj.insert("numReplicas".to_string(), json!(target_count));
    }
}

/// Applies variable overrides to every `edge`-role service (e.g. PgBouncer's
/// `POOL_MODE` chosen at `add` time).
pub fn apply_edge_variables(config: &mut Value, overrides: &BTreeMap<String, String>) {
    let Some(services_mut) = services_obj_mut(config) else {
        return;
    };
    let edge_ids: Vec<String> = services_mut
        .iter()
        .filter(|(_, s)| role_of(s) == Some("edge"))
        .map(|(k, _)| k.clone())
        .collect();
    for id in edge_ids {
        if let Some(svc) = services_mut.get_mut(&id) {
            for (name, value) in overrides {
                set_template_var(svc, name, value.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(role: &str, name: &str) -> Value {
        json!({ "clusterRole": role, "name": name })
    }

    fn config_with(services: Vec<(&str, Value)>) -> Value {
        let mut map = Map::new();
        for (id, service) in services {
            map.insert(id.to_string(), service);
        }
        json!({ "services": Value::Object(map) })
    }

    #[test]
    fn adjust_replica_count_scales_up_and_stamps_wiring() {
        let mut root = service("root", "postgres-1");
        root["variables"] = json!({ "PATRONI_ENABLED": { "defaultValue": "true" } });
        let mut config = config_with(vec![
            ("root", root),
            ("replica", service("replica", "postgres-replica")),
            ("edge", service("edge", "haproxy")),
        ]);

        adjust_replica_count(&mut config, 2);

        let services = services_obj(&config).unwrap();
        let replica_count = services
            .values()
            .filter(|s| role_of(s) == Some("replica"))
            .count();
        assert_eq!(replica_count, 2);

        // The edge's data-nodes list is rebuilt from root + all replicas.
        let edge = services.get("edge").unwrap();
        let data_nodes = edge["variables"]["POSTGRES_NODES"]["defaultValue"]
            .as_str()
            .unwrap();
        assert!(data_nodes.contains("postgres-1"));
        assert_eq!(data_nodes.split(',').count(), 3);
    }

    #[test]
    fn adjust_replica_count_scales_down_keeps_lowest_numbered() {
        let mut config = config_with(vec![
            ("root", service("root", "postgres-1")),
            ("replica-1", service("replica", "postgres-replica-1")),
            ("replica-2", service("replica", "postgres-replica-2")),
            ("replica-3", service("replica", "postgres-replica-3")),
        ]);

        adjust_replica_count(&mut config, 1);

        let services = services_obj(&config).unwrap();
        assert!(services.contains_key("replica-1"));
        assert!(!services.contains_key("replica-2"));
        assert!(!services.contains_key("replica-3"));
    }

    #[test]
    fn adjust_replica_count_noop_without_replica_role() {
        let mut config = config_with(vec![("root", service("root", "postgres-1"))]);
        let before = config.clone();
        adjust_replica_count(&mut config, 3);
        assert_eq!(config, before);
    }

    #[test]
    fn adjust_internal_count_scales_and_stamps_coordinator_hosts() {
        let mut root = service("root", "postgres-1");
        root["variables"] = json!({ "PATRONI_ENABLED": { "defaultValue": "true" } });
        let mut config = config_with(vec![("root", root), ("etcd", service("internal", "etcd"))]);

        adjust_internal_count(&mut config, 3);

        let services = services_obj(&config).unwrap();
        let internal_count = services
            .values()
            .filter(|s| role_of(s) == Some("internal"))
            .count();
        assert_eq!(internal_count, 3);

        let root = services.get("root").unwrap();
        let hosts = root["variables"]["PATRONI_ETCD3_HOSTS"]["defaultValue"]
            .as_str()
            .unwrap();
        assert_eq!(hosts.split(',').count(), 3);
        assert!(hosts.contains(":2379"));
    }

    #[test]
    fn adjust_edge_count_sets_num_replicas() {
        let mut config = config_with(vec![("edge", service("edge", "haproxy"))]);
        adjust_edge_count(&mut config, 4);
        let services = services_obj(&config).unwrap();
        assert_eq!(services["edge"]["deploy"]["numReplicas"], json!(4));
    }

    #[test]
    fn adjust_edge_count_noop_without_edge_role() {
        let mut config = config_with(vec![("root", service("root", "postgres-1"))]);
        let before = config.clone();
        adjust_edge_count(&mut config, 4);
        assert_eq!(config, before);
    }

    #[test]
    fn apply_edge_variables_stamps_every_edge_service() {
        let mut config = config_with(vec![("edge", service("edge", "haproxy"))]);
        let mut overrides = BTreeMap::new();
        overrides.insert("POOL_MODE".to_string(), "transaction".to_string());

        apply_edge_variables(&mut config, &overrides);

        let services = services_obj(&config).unwrap();
        assert_eq!(
            services["edge"]["variables"]["POOL_MODE"]["defaultValue"],
            json!("transaction")
        );
    }

    #[test]
    fn clone_service_renames_and_rekeys_volume_mounts() {
        let original = json!({
            "clusterRole": "replica",
            "name": "postgres-replica-1",
            "volumeMounts": { "vol-a": { "mountPath": "/data" } }
        });
        let cloned = clone_service(&original, "2");
        assert_eq!(cloned["name"], json!("postgres-replica-2"));
        assert!(cloned["volumeMounts"].get("vol-a-2").is_some());
        assert!(cloned["volumeMounts"].get("vol-a").is_none());
    }

    #[test]
    fn trailing_number_and_strip_suffix() {
        assert_eq!(trailing_number("postgres-replica-12"), 12);
        assert_eq!(trailing_number("postgres-replica"), 0);
        assert_eq!(
            strip_trailing_dash_number("postgres-replica-2"),
            "postgres-replica"
        );
        assert_eq!(
            strip_trailing_dash_number("postgres-replica"),
            "postgres-replica"
        );
    }

    #[test]
    fn staged_patch_is_nonempty_detects_real_changes() {
        assert!(!staged_patch_is_nonempty(&Value::Null));
        assert!(!staged_patch_is_nonempty(&json!({})));
        assert!(!staged_patch_is_nonempty(&json!({ "services": {} })));
        assert!(!staged_patch_is_nonempty(&json!({ "services": null })));
        assert!(staged_patch_is_nonempty(
            &json!({ "services": { "svc": { "variables": { "FOO": null } } } })
        ));
        // Non-object top-level values (e.g. a scalar field) count as content.
        assert!(staged_patch_is_nonempty(&json!({ "name": "production" })));
    }

    #[test]
    fn adjust_replica_count_scale_to_zero_removes_all_and_restamps_root_only() {
        let mut root = service("root", "postgres-1");
        root["variables"] = json!({ "PATRONI_ENABLED": { "defaultValue": "true" } });
        let mut config = config_with(vec![
            ("root", root),
            ("replica-1", service("replica", "postgres-replica-1")),
            ("replica-2", service("replica", "postgres-replica-2")),
            ("edge", service("edge", "haproxy")),
        ]);

        adjust_replica_count(&mut config, 0);

        let services = services_obj(&config).unwrap();
        assert!(services.values().all(|s| role_of(s) != Some("replica")));

        // The edge's data-node list shrinks to just the root.
        let data_nodes = services["edge"]["variables"]["POSTGRES_NODES"]["defaultValue"]
            .as_str()
            .unwrap();
        assert_eq!(data_nodes.split(',').count(), 1);
        assert!(data_nodes.contains("postgres-1"));
    }

    #[test]
    fn adjust_internal_count_scale_down_keeps_lowest_numbered() {
        let mut root = service("root", "postgres-1");
        root["variables"] = json!({ "PATRONI_ENABLED": { "defaultValue": "true" } });
        let mut config = config_with(vec![
            ("root", root),
            ("etcd-1", service("internal", "etcd-1")),
            ("etcd-2", service("internal", "etcd-2")),
            ("etcd-3", service("internal", "etcd-3")),
        ]);

        adjust_internal_count(&mut config, 1);

        let services = services_obj(&config).unwrap();
        assert!(services.contains_key("etcd-1"));
        assert!(!services.contains_key("etcd-2"));
        assert!(!services.contains_key("etcd-3"));

        // Coordinator hosts restamped down to the single survivor.
        let hosts = services["root"]["variables"]["PATRONI_ETCD3_HOSTS"]["defaultValue"]
            .as_str()
            .unwrap();
        assert_eq!(hosts.split(',').count(), 1);
        assert!(hosts.contains("etcd-1"));
    }

    #[test]
    fn clone_service_without_volume_mounts_only_renames() {
        let original = json!({ "clusterRole": "internal", "name": "etcd-1" });
        let cloned = clone_service(&original, "4");
        assert_eq!(cloned["name"], json!("etcd-4"));
        assert!(cloned.get("volumeMounts").is_none());
    }

    #[test]
    fn adjust_edge_count_preserves_other_deploy_fields() {
        let mut edge = service("edge", "haproxy");
        edge["deploy"] = json!({ "startCommand": "haproxy -f /cfg" });
        let mut config = config_with(vec![("edge", edge)]);

        adjust_edge_count(&mut config, 2);

        let services = services_obj(&config).unwrap();
        assert_eq!(services["edge"]["deploy"]["numReplicas"], json!(2));
        assert_eq!(
            services["edge"]["deploy"]["startCommand"],
            json!("haproxy -f /cfg")
        );
    }

    #[test]
    fn apply_edge_variables_stamps_every_edge_service_when_multiple() {
        let mut config = config_with(vec![
            ("edge-1", service("edge", "haproxy")),
            ("edge-2", service("edge", "pgbouncer")),
            ("root", service("root", "postgres-1")),
        ]);
        let mut overrides = BTreeMap::new();
        overrides.insert("POOL_MODE".to_string(), "session".to_string());

        apply_edge_variables(&mut config, &overrides);

        let services = services_obj(&config).unwrap();
        for edge_id in ["edge-1", "edge-2"] {
            assert_eq!(
                services[edge_id]["variables"]["POOL_MODE"]["defaultValue"],
                json!("session")
            );
        }
        assert!(services["root"].get("variables").is_none());
    }

    #[test]
    fn format_data_node_entry_substitutes_host_and_root_name() {
        assert_eq!(
            format_data_node_entry("{host}:${{{rootName}.PGPORT}}:8008", "r1", "db-prod"),
            "${{r1.RAILWAY_PRIVATE_DOMAIN}}:${{db-prod.PGPORT}}:8008"
        );
    }

    #[test]
    fn resolve_cluster_wiring_reads_declared_json_over_legacy() {
        let root = json!({
            "clusterRole": "root",
            "name": "pg",
            "variables": { "PATRONI_ENABLED": { "defaultValue": "true" } },
            "clusterWiring": {
                "internalNodeNameVariable": "NODE_NAME",
                "coordinatorHostsVariable": "HOSTS",
                "coordinatorPort": 1234,
                "replicaNodeNameVariable": "REPLICA_NAME",
                "dataNodesVariable": "NODES",
                "dataNodesEntryFormat": "{host}",
                "quorumVariable": "QUORUM"
            }
        });
        let wiring = resolve_cluster_wiring(&root).unwrap();
        assert_eq!(wiring.coordinator_port, Some(1234));
        assert_eq!(wiring.quorum_variable.as_deref(), Some("QUORUM"));
        assert_eq!(wiring.data_nodes_variable.as_deref(), Some("NODES"));
    }

    #[test]
    fn resolve_cluster_wiring_none_without_wiring_or_patroni() {
        let root = json!({ "clusterRole": "root", "name": "pg" });
        assert!(resolve_cluster_wiring(&root).is_none());
    }
}
