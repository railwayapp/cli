//! Topology / enabled-state detection for the managed database features
//! (PITR, HA, connection pooling) the `railway {postgres,mysql,redis}`
//! command trees operate on. All of it is derived from the same
//! customer-facing `environment.config` blob the dashboard parses client-side
//! -- there is no dedicated "is this enabled" query.
//!
//! Nothing here names an engine. What makes a cluster a cluster
//! (`haActiveVariable`), how its nodes are wired (`clusterWiring`) and which
//! variables carry an archive (the engine's PITR prefix) are all read from
//! declarations: the template's, stamped onto the root service at conversion
//! time, with the engine registry supplying only the fallbacks for services
//! converted before those declarations existed. Adding an engine is a
//! registry entry, never a branch in this file.

use std::collections::{BTreeMap, HashSet};

use super::config::{EnvironmentConfig, ServiceInstance};
use super::database_engines::{DatabaseEngine, PitrSpec, PoolingSpec};

pub const POOL_MODE_VAR: &str = "POOL_MODE";
pub const MAX_CLIENT_CONN_VAR: &str = "MAX_CLIENT_CONN";
pub const DEFAULT_POOL_SIZE_VAR: &str = "DEFAULT_POOL_SIZE";
pub const MAX_PREPARED_STATEMENTS_VAR: &str = "MAX_PREPARED_STATEMENTS";

/// State of PITR on a single database service instance (standalone, or one
/// member of an HA cluster).
///
/// Deliberately carries no verdict about the image: whether an image may
/// adopt the feature is the enable template's own declaration
/// (`adoptionImageEligibility`), evaluated against the fetched template
/// record in `adoption_eligibility` -- not a predicate compiled into the CLI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PitrState {
    /// The archive overlay has been applied (the gate variable is present).
    pub enabled: bool,
    /// The archive bucket variable actually has a value, as opposed to
    /// present-but-empty (a miswired or not-yet-provisioned bucket).
    pub bucket_wired: bool,
    /// The service's current image, for messaging and eligibility checks.
    pub image: Option<String>,
    /// A custom start command overrides the entrypoint that switches
    /// archiving on -- PITR is silently inert while one is set.
    pub has_start_command: bool,
}

/// A single member of an HA cluster (or a standalone root reported as its own
/// one-member "cluster").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaMember {
    pub service_id: String,
    pub service_name: String,
    pub cluster_role: Option<String>,
}

/// HA cluster topology for a service, resolved by walking `parentServiceId`
/// chains up to the root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HaState {
    /// The resolved root is a genuine HA cluster -- its declared HA-active
    /// variable reads "true" -- rather than a standalone root that merely has
    /// an edge child (e.g. a pooler in front of a single database).
    pub is_cluster: bool,
    pub root_service_id: Option<String>,
    pub members: Vec<HaMember>,
}

/// Connection-pooler attachment state for a cluster/standalone root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolingState {
    pub attached: bool,
    pub edge_service_id: Option<String>,
    pub pool_mode: Option<String>,
    pub max_client_conn: Option<i64>,
    pub default_pool_size: Option<i64>,
    pub max_prepared_statements: Option<i64>,
}

fn var_value(service: &ServiceInstance, name: &str) -> Option<String> {
    service.variables.get(name)?.as_ref()?.value.clone()
}

/// Computes PITR enabled-state for a single service instance against the
/// engine's declared archive variable contract.
pub fn compute_pitr_state(service: &ServiceInstance, pitr: &PitrSpec) -> PitrState {
    let gate_var = pitr.archive_gate_variable();
    let bucket_value = var_value(service, &gate_var);
    let start_command = service
        .deploy
        .as_ref()
        .and_then(|d| d.start_command.as_deref());

    PitrState {
        enabled: service.variables.contains_key(&gate_var),
        bucket_wired: bucket_value
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty()),
        image: service.source.as_ref().and_then(|s| s.image.clone()),
        has_start_command: start_command.is_some_and(|c| !c.trim().is_empty()),
    }
}

/// Walks `parentServiceId` up from `service_id` to the ultimate cluster root.
/// Returns `service_id` itself if it has no (resolvable) parent.
fn find_root_id(config: &EnvironmentConfig, service_id: &str) -> String {
    let mut current = service_id.to_string();
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(service) = config.services.get(&current) {
        if !seen.insert(current.clone()) {
            break; // cycle guard
        }
        match &service.parent_service_id {
            Some(parent) if config.services.contains_key(parent) => current = parent.clone(),
            _ => break,
        }
    }
    current
}

/// The variable whose "true" means this root's HA agent is active.
///
/// The root's own `haActiveVariable` is the declaration and always wins.
/// The engine's registry fallback exists only for clusters converted before
/// templates carried the field -- and an engine whose HA companion has always
/// declared it registers no fallback at all, so a missing declaration there
/// correctly reads as "not a cluster" instead of guessing another engine's
/// variable name.
fn ha_active_variable(root: &ServiceInstance, engine: &DatabaseEngine) -> Option<String> {
    root.ha_active_variable
        .clone()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            engine
                .ha
                .and_then(|ha| ha.legacy_active_variable)
                .map(str::to_string)
        })
}

fn ha_active(root: &ServiceInstance, engine: &DatabaseEngine) -> bool {
    ha_active_variable(root, engine)
        .and_then(|name| var_value(root, &name))
        .as_deref()
        == Some("true")
}

/// Resolves HA cluster topology for `service_id`: the root, and every member
/// whose parent chain resolves to it.
pub fn compute_ha_state(
    config: &EnvironmentConfig,
    service_id: &str,
    service_names: &BTreeMap<String, String>,
    engine: &DatabaseEngine,
) -> HaState {
    if !config.services.contains_key(service_id) {
        return HaState::default();
    }

    let root_id = find_root_id(config, service_id);
    let mut members: Vec<HaMember> = config
        .services
        .iter()
        .filter(|(id, _)| id.as_str() == root_id || find_root_id(config, id) == root_id)
        .map(|(id, service)| HaMember {
            service_id: id.clone(),
            service_name: service_names.get(id).cloned().unwrap_or_else(|| id.clone()),
            cluster_role: service.cluster_role.clone(),
        })
        .collect();
    members.sort_by(|a, b| a.service_name.cmp(&b.service_name));

    let is_cluster = config
        .services
        .get(&root_id)
        .is_some_and(|root| ha_active(root, engine));

    HaState {
        is_cluster,
        root_service_id: Some(root_id),
        members,
    }
}

/// Resolves pooler attachment for a cluster/standalone `root_service_id`: the
/// non-deleted child service whose image is the engine's declared pooler.
pub fn compute_pooling_state(
    config: &EnvironmentConfig,
    root_service_id: &str,
    pooling: &PoolingSpec,
) -> PoolingState {
    let edge = config.services.iter().find(|(_, service)| {
        !service.is_deleted.unwrap_or(false)
            && service.parent_service_id.as_deref() == Some(root_service_id)
            && service
                .source
                .as_ref()
                .and_then(|s| s.image.as_deref())
                .is_some_and(|image| {
                    image
                        .to_ascii_lowercase()
                        .contains(pooling.image_identifier)
                })
    });

    let Some((edge_id, edge_service)) = edge else {
        return PoolingState::default();
    };

    PoolingState {
        attached: true,
        edge_service_id: Some(edge_id.clone()),
        pool_mode: var_value(edge_service, POOL_MODE_VAR),
        max_client_conn: var_value(edge_service, MAX_CLIENT_CONN_VAR).and_then(|v| v.parse().ok()),
        default_pool_size: var_value(edge_service, DEFAULT_POOL_SIZE_VAR)
            .and_then(|v| v.parse().ok()),
        max_prepared_statements: var_value(edge_service, MAX_PREPARED_STATEMENTS_VAR)
            .and_then(|v| v.parse().ok()),
    }
}

/// The name a data node registers under with its own coordinator.
///
/// Prefers the node's declared identity variable (the root's
/// `clusterWiring.replicaNodeNameVariable`) read from the DECRYPTED config;
/// falls back to the lowercased service name, which is what conversion and
/// scale stamp for new nodes. The distinction is load-bearing for the ROOT
/// specifically: an HA template authors its root's node name (e.g.
/// `postgres-1`), which never matches the adopted service's own name.
pub fn member_identity_name(
    config: &EnvironmentConfig,
    root_id: &str,
    service_id: &str,
    service_name: &str,
) -> String {
    let identity_var = config
        .services
        .get(root_id)
        .and_then(|root| root.cluster_wiring.as_ref())
        .and_then(|wiring| wiring.replica_node_name_variable.clone());

    identity_var
        .and_then(|var| {
            config
                .services
                .get(service_id)
                .and_then(|service| var_value(service, &var))
        })
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| service_name.to_ascii_lowercase())
}

/// If `service_id` is an edge child (a pooler or router in front of the
/// database), resolves to its parent -- the actual database root -- so a
/// command invoked with `--service` pointed at an edge node still operates on
/// the right root.
pub fn resolve_root_service_id(config: &EnvironmentConfig, service_id: &str) -> String {
    match config.services.get(service_id) {
        Some(service) if service.cluster_role.as_deref() == Some("edge") => service
            .parent_service_id
            .clone()
            .unwrap_or_else(|| service_id.to_string()),
        _ => service_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controllers::config::{DeployConfig, ServiceSource, Variable};
    use crate::controllers::database_engines::{MYSQL, POSTGRES, REDIS};

    fn service_with_image(image: &str) -> ServiceInstance {
        ServiceInstance {
            source: Some(ServiceSource {
                image: Some(image.to_string()),
                ..ServiceSource::default()
            }),
            ..ServiceInstance::default()
        }
    }

    fn with_var(mut service: ServiceInstance, name: &str, value: &str) -> ServiceInstance {
        service.variables.insert(
            name.to_string(),
            Some(Variable {
                value: Some(value.to_string()),
                ..Variable::default()
            }),
        );
        service
    }

    #[test]
    fn pitr_state_reads_the_engines_own_gate_variable() {
        let postgres = with_var(
            service_with_image("ghcr.io/railwayapp-templates/postgres-ssl:16"),
            "WAL_ARCHIVE_BUCKET",
            "my-bucket",
        );
        let state = compute_pitr_state(&postgres, &POSTGRES.pitr.unwrap());
        assert!(state.enabled);
        assert!(state.bucket_wired);

        // The very same service reads as NOT enabled under MySQL's contract:
        // each engine's archive lives under its own prefix.
        assert!(!compute_pitr_state(&postgres, &MYSQL.pitr.unwrap()).enabled);

        let mysql = with_var(
            service_with_image("ghcr.io/railwayapp-templates/mysql-ha/mysql:8.4"),
            "BINLOG_ARCHIVE_BUCKET",
            "binlogs",
        );
        assert!(compute_pitr_state(&mysql, &MYSQL.pitr.unwrap()).enabled);
        assert!(!compute_pitr_state(&mysql, &POSTGRES.pitr.unwrap()).enabled);
    }

    #[test]
    fn pitr_state_bucket_present_but_empty_is_not_wired() {
        let service = with_var(
            service_with_image("ghcr.io/railwayapp-templates/postgres-ssl:16"),
            "WAL_ARCHIVE_BUCKET",
            "",
        );
        let state = compute_pitr_state(&service, &POSTGRES.pitr.unwrap());
        assert!(state.enabled);
        assert!(!state.bucket_wired);
    }

    #[test]
    fn pitr_state_disabled_without_the_gate_variable() {
        let service = service_with_image("ghcr.io/railwayapp-templates/postgres-ssl:16");
        let state = compute_pitr_state(&service, &POSTGRES.pitr.unwrap());
        assert!(!state.enabled);
        assert!(!state.bucket_wired);
        assert!(!state.has_start_command);
    }

    #[test]
    fn pitr_state_whitespace_start_command_does_not_count() {
        let mut service = service_with_image("ghcr.io/railwayapp-templates/postgres-ssl:16");
        service.deploy = Some(DeployConfig {
            start_command: Some("   ".to_string()),
            ..DeployConfig::default()
        });
        assert!(!compute_pitr_state(&service, &POSTGRES.pitr.unwrap()).has_start_command);
    }

    fn config_with(services: Vec<(&str, ServiceInstance)>) -> EnvironmentConfig {
        let mut config = EnvironmentConfig::default();
        for (id, service) in services {
            config.services.insert(id.to_string(), service);
        }
        config
    }

    /// A root that declares its HA-active variable, the way every HA template
    /// stamps it today.
    fn declared_root(active_variable: &str, active: bool) -> ServiceInstance {
        let service = ServiceInstance {
            cluster_role: Some("root".to_string()),
            ha_active_variable: Some(active_variable.to_string()),
            ..ServiceInstance::default()
        };
        if active {
            with_var(service, active_variable, "true")
        } else {
            service
        }
    }

    fn child_service(parent_id: &str, cluster_role: &str) -> ServiceInstance {
        ServiceInstance {
            parent_service_id: Some(parent_id.to_string()),
            cluster_role: Some(cluster_role.to_string()),
            ..ServiceInstance::default()
        }
    }

    #[test]
    fn ha_state_reads_each_engines_declared_active_variable() {
        for (engine, variable) in [
            (&POSTGRES, "PATRONI_ENABLED"),
            (&REDIS, "SENTINEL_ENABLED"),
            (&MYSQL, "GR_ENABLED"),
        ] {
            let config = config_with(vec![
                ("root", declared_root(variable, true)),
                ("replica-1", child_service("root", "replica")),
            ]);
            let state = compute_ha_state(&config, "replica-1", &BTreeMap::new(), engine);
            assert!(state.is_cluster, "{} cluster via {variable}", engine.key);
            assert_eq!(state.root_service_id.as_deref(), Some("root"));
            assert_eq!(state.members.len(), 2);
        }
    }

    #[test]
    fn ha_state_falls_back_to_the_legacy_variable_only_where_registered() {
        // A cluster converted before templates declared haActiveVariable
        // carries the variable but no declaration. Postgres registers that
        // fallback...
        let legacy_root = with_var(
            ServiceInstance {
                cluster_role: Some("root".to_string()),
                ..ServiceInstance::default()
            },
            "PATRONI_ENABLED",
            "true",
        );
        let config = config_with(vec![("root", legacy_root)]);
        assert!(compute_ha_state(&config, "root", &BTreeMap::new(), &POSTGRES).is_cluster);

        // ...and the engines whose companions always declared it register
        // none, so they must not go guessing another engine's variable.
        assert!(!compute_ha_state(&config, "root", &BTreeMap::new(), &REDIS).is_cluster);
        assert!(!compute_ha_state(&config, "root", &BTreeMap::new(), &MYSQL).is_cluster);
    }

    #[test]
    fn ha_state_not_a_cluster_when_the_agent_is_off() {
        // A root with an edge child but the HA agent inactive is a pooled or
        // routed standalone, not a cluster.
        let config = config_with(vec![
            ("root", declared_root("SENTINEL_ENABLED", false)),
            ("edge", child_service("root", "edge")),
        ]);
        let state = compute_ha_state(&config, "root", &BTreeMap::new(), &REDIS);
        assert!(!state.is_cluster);
        assert_eq!(state.root_service_id.as_deref(), Some("root"));
    }

    #[test]
    fn ha_state_unknown_service_returns_default() {
        let config = config_with(vec![("root", declared_root("PATRONI_ENABLED", true))]);
        let state = compute_ha_state(&config, "missing", &BTreeMap::new(), &POSTGRES);
        assert!(!state.is_cluster);
        assert!(state.root_service_id.is_none());
        assert!(state.members.is_empty());
    }

    #[test]
    fn ha_state_survives_a_parent_cycle() {
        let config = config_with(vec![
            ("a", child_service("b", "replica")),
            ("b", child_service("a", "replica")),
        ]);
        let state = compute_ha_state(&config, "a", &BTreeMap::new(), &POSTGRES);
        assert!(!state.is_cluster);
        assert_eq!(state.root_service_id.as_deref(), Some("a"));
    }

    #[test]
    fn ha_state_members_sorted_by_name() {
        let config = config_with(vec![
            ("root", declared_root("PATRONI_ENABLED", true)),
            ("z", child_service("root", "replica")),
            ("a", child_service("root", "replica")),
        ]);
        let names: BTreeMap<String, String> =
            [("root", "db"), ("z", "zz-replica"), ("a", "aa-replica")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        let state = compute_ha_state(&config, "root", &names, &POSTGRES);
        let ordered: Vec<&str> = state
            .members
            .iter()
            .map(|m| m.service_name.as_str())
            .collect();
        assert_eq!(ordered, vec!["aa-replica", "db", "zz-replica"]);
    }

    #[test]
    fn pooling_state_finds_the_attached_pooler_and_its_knobs() {
        let pooling = POSTGRES.pooling.unwrap();
        let mut pgbouncer = child_service("root", "edge");
        pgbouncer.source = Some(ServiceSource {
            image: Some("ghcr.io/railwayapp-templates/pgbouncer:latest".to_string()),
            ..ServiceSource::default()
        });
        pgbouncer = with_var(pgbouncer, POOL_MODE_VAR, "transaction");
        pgbouncer = with_var(pgbouncer, MAX_CLIENT_CONN_VAR, "100");

        let config = config_with(vec![
            ("root", declared_root("PATRONI_ENABLED", false)),
            ("pgbouncer", pgbouncer),
        ]);
        let state = compute_pooling_state(&config, "root", &pooling);
        assert!(state.attached);
        assert_eq!(state.edge_service_id.as_deref(), Some("pgbouncer"));
        assert_eq!(state.pool_mode.as_deref(), Some("transaction"));
        assert_eq!(state.max_client_conn, Some(100));
    }

    #[test]
    fn pooling_state_ignores_deleted_and_non_pooler_children() {
        let pooling = POSTGRES.pooling.unwrap();

        let mut deleted = child_service("root", "edge");
        deleted.source = Some(ServiceSource {
            image: Some("ghcr.io/railwayapp-templates/pgbouncer:latest".to_string()),
            ..ServiceSource::default()
        });
        deleted.is_deleted = Some(true);
        let config = config_with(vec![
            ("root", declared_root("PATRONI_ENABLED", false)),
            ("pgbouncer", deleted),
        ]);
        assert!(!compute_pooling_state(&config, "root", &pooling).attached);

        // The cluster's routing edge is not a pooler.
        let mut haproxy = child_service("root", "edge");
        haproxy.source = Some(ServiceSource {
            image: Some("ghcr.io/railwayapp-templates/postgres-ha/haproxy:3".to_string()),
            ..ServiceSource::default()
        });
        let config = config_with(vec![
            ("root", declared_root("PATRONI_ENABLED", true)),
            ("haproxy", haproxy),
        ]);
        assert!(!compute_pooling_state(&config, "root", &pooling).attached);
    }

    #[test]
    fn pooling_state_tolerates_unparseable_knob_values() {
        let pooling = POSTGRES.pooling.unwrap();
        let mut pgbouncer = child_service("root", "edge");
        pgbouncer.source = Some(ServiceSource {
            image: Some("ghcr.io/railwayapp-templates/pgbouncer:latest".to_string()),
            ..ServiceSource::default()
        });
        pgbouncer = with_var(pgbouncer, MAX_CLIENT_CONN_VAR, "not-a-number");
        let config = config_with(vec![
            ("root", declared_root("PATRONI_ENABLED", false)),
            ("pgbouncer", pgbouncer),
        ]);
        let state = compute_pooling_state(&config, "root", &pooling);
        assert!(state.attached);
        assert_eq!(state.max_client_conn, None);
    }

    #[test]
    fn member_identity_prefers_the_declared_identity_variable() {
        use crate::controllers::config::ClusterWiring;

        let mut root = declared_root("PATRONI_ENABLED", true);
        root.cluster_wiring = Some(ClusterWiring {
            replica_node_name_variable: Some("PATRONI_NAME".to_string()),
            ..ClusterWiring::default()
        });
        let root = with_var(root, "PATRONI_NAME", "postgres-1");
        let replica = with_var(
            child_service("root", "replica"),
            "PATRONI_NAME",
            "postgres-replica-1",
        );
        let config = config_with(vec![("root", root), ("replica-1", replica)]);

        // The template authors the root's node name; the service's own
        // (possibly renamed) display name must not leak into the join.
        assert_eq!(
            member_identity_name(&config, "root", "root", "Postgres"),
            "postgres-1"
        );
        assert_eq!(
            member_identity_name(&config, "root", "replica-1", "postgres-replica-1"),
            "postgres-replica-1"
        );

        // A topology that declares no identity variable (redis-ha, mysql-ha)
        // falls back to the lowercased service name.
        let bare = config_with(vec![("root", declared_root("SENTINEL_ENABLED", true))]);
        assert_eq!(
            member_identity_name(&bare, "root", "root", "Redis-1"),
            "redis-1"
        );
    }

    #[test]
    fn resolve_root_service_id_follows_an_edge_child_to_its_parent() {
        let config = config_with(vec![
            ("root", declared_root("PATRONI_ENABLED", false)),
            ("pgbouncer", child_service("root", "edge")),
        ]);
        assert_eq!(resolve_root_service_id(&config, "pgbouncer"), "root");
        assert_eq!(resolve_root_service_id(&config, "root"), "root");
        assert_eq!(resolve_root_service_id(&config, "unknown"), "unknown");

        // An edge with no parent falls back to itself rather than vanishing.
        let orphan = ServiceInstance {
            cluster_role: Some("edge".to_string()),
            parent_service_id: None,
            ..ServiceInstance::default()
        };
        let config = config_with(vec![("edge", orphan)]);
        assert_eq!(resolve_root_service_id(&config, "edge"), "edge");
    }
}
