//! Topology / enabled-state detection for the three Postgres plugin features
//! (PITR, HA, PgBouncer) that `railway postgres {pitr,ha,pgbouncer}` operate
//! on. All of it is derived from the same customer-facing `environment.config`
//! blob the frontend parses client-side (see `ConfigSettings.tsx`,
//! `useClusterState.tsx`) -- there is no dedicated "is this enabled" query.
//!
//! Deliberately scoped to Railway's own built-in templates (`postgres-ha`,
//! `postgres-pitr`, `postgres-with-pgbouncer`): HA detection keys off the
//! historical `PATRONI_ENABLED` variable rather than the more general
//! template-declared `haActiveVariable`/`clusterWiring` metadata the frontend
//! also supports for third-party composable templates, since the CLI only
//! ever drives Railway's own Postgres plugins.

use std::collections::{BTreeMap, HashSet};

use super::config::{EnvironmentConfig, ServiceInstance};

/// A WAL archive variable being present at all (regardless of value) is what
/// the PITR overlay template stamps onto the root service.
const WAL_ARCHIVE_BUCKET_VAR: &str = "WAL_ARCHIVE_BUCKET";
const PATRONI_ENABLED_VAR: &str = "PATRONI_ENABLED";
const PGBOUNCER_IMAGE_IDENTIFIER: &str = "pgbouncer";

pub const POOL_MODE_VAR: &str = "POOL_MODE";
pub const MAX_CLIENT_CONN_VAR: &str = "MAX_CLIENT_CONN";
pub const DEFAULT_POOL_SIZE_VAR: &str = "DEFAULT_POOL_SIZE";
pub const MAX_PREPARED_STATEMENTS_VAR: &str = "MAX_PREPARED_STATEMENTS";

/// State of PITR on a single Postgres service instance (standalone or one
/// member of an HA cluster).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PitrState {
    /// The PITR overlay has been applied (WAL archive vars are present).
    pub enabled: bool,
    /// The archive bucket variable actually has a value (vs. present-but-empty,
    /// e.g. a miswired/not-yet-provisioned bucket).
    pub bucket_wired: bool,
    /// Image is an official Railway Postgres image pinned to a minor version
    /// (e.g. `postgres-ssl:16.10`) -- must be un-pinned before PITR can enable.
    pub minor_pinned: bool,
    /// Image is not an official Railway Postgres image at all (e.g. a custom
    /// image) -- PITR is not supported.
    pub unsupported_image: bool,
    /// A custom start command overrides the entrypoint that turns on WAL
    /// archiving -- PITR is silently inert while one is set.
    pub has_start_command: bool,
}

/// A single member of an HA cluster (or a standalone root reported as its
/// own one-member "cluster").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaMember {
    pub service_id: String,
    pub service_name: String,
    pub cluster_role: Option<String>,
}

/// HA cluster topology for a service, resolved by walking `parentServiceId`
/// chains up to the root, same as the frontend's `useClusterState.tsx`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HaState {
    /// The resolved root is a genuine Patroni HA cluster (root has
    /// `PATRONI_ENABLED=true`), not merely a pooled-standalone root with a
    /// PgBouncer edge child.
    pub is_cluster: bool,
    pub root_service_id: Option<String>,
    pub members: Vec<HaMember>,
}

/// PgBouncer attachment state for a cluster/standalone root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PgBouncerState {
    pub attached: bool,
    pub edge_service_id: Option<String>,
    pub pool_mode: Option<String>,
    pub max_client_conn: Option<i64>,
    pub default_pool_size: Option<i64>,
    pub max_prepared_statements: Option<i64>,
}

/// Ported verbatim from the frontend's `constants.ts`. Postgres-ssl and
/// postgres-ha/postgres-patroni are the only images PITR/HA support; custom
/// images (e.g. PostGIS) never qualify.
pub fn is_official_postgres_image(image: Option<&str>) -> bool {
    match image {
        Some(image) => {
            image.contains("railwayapp-templates/postgres-ssl")
                || image.contains("railwayapp-templates/postgres-ha/postgres-patroni")
        }
        None => false,
    }
}

/// Ported verbatim from the frontend's `constants.ts`. True when `image` is a
/// Railway-owned Postgres image pinned to a minor tag (e.g. `:16.10`); major-only
/// (`:16`) and unversioned/`:latest` tags return false.
pub fn is_minor_pinned_postgres_image(image: Option<&str>) -> bool {
    let Some(image) = image else {
        return false;
    };
    if !is_official_postgres_image(Some(image)) {
        return false;
    }
    let Some(colon_index) = image.rfind(':') else {
        return false;
    };
    let tag = &image[colon_index + 1..];
    let mut parts = tag.split('.');
    let major = parts.next().unwrap_or("");
    let minor = parts.next().unwrap_or("");
    !major.is_empty()
        && major.chars().all(|c| c.is_ascii_digit())
        && !minor.is_empty()
        && minor.chars().all(|c| c.is_ascii_digit())
}

fn var_value(service: &ServiceInstance, name: &str) -> Option<String> {
    service.variables.get(name)?.as_ref()?.value.clone()
}

/// Computes PITR guardrail/enabled state for a single service instance (the
/// standalone root, or one HA member).
pub fn compute_pitr_state(service: &ServiceInstance) -> PitrState {
    let bucket_value = var_value(service, WAL_ARCHIVE_BUCKET_VAR);
    let image = service.source.as_ref().and_then(|s| s.image.as_deref());
    let start_command = service
        .deploy
        .as_ref()
        .and_then(|d| d.start_command.as_deref());

    PitrState {
        enabled: service.variables.contains_key(WAL_ARCHIVE_BUCKET_VAR),
        bucket_wired: bucket_value
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty()),
        minor_pinned: is_minor_pinned_postgres_image(image),
        unsupported_image: !is_official_postgres_image(image),
        has_start_command: start_command.is_some_and(|c| !c.trim().is_empty()),
    }
}

/// Walks `parentServiceId` up from `service_id` to find the ultimate cluster
/// root. Returns `service_id` itself if it has no (resolvable) parent.
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

fn ha_active(root: &ServiceInstance) -> bool {
    var_value(root, PATRONI_ENABLED_VAR).as_deref() == Some("true")
}

/// Resolves HA cluster topology for `service_id`: finds the root and every
/// member whose parent chain resolves to it, using the `cluster_role` field
/// stamped by template conversion where available.
pub fn compute_ha_state(
    config: &EnvironmentConfig,
    service_id: &str,
    service_names: &BTreeMap<String, String>,
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
        .map(ha_active)
        .unwrap_or(false);

    HaState {
        is_cluster,
        root_service_id: Some(root_id),
        members,
    }
}

/// Resolves PgBouncer attachment for a cluster/standalone `root_service_id`:
/// the non-deleted child service whose image is a PgBouncer image.
pub fn compute_pgbouncer_state(
    config: &EnvironmentConfig,
    root_service_id: &str,
) -> PgBouncerState {
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
                        .contains(PGBOUNCER_IMAGE_IDENTIFIER)
                })
    });

    let Some((edge_id, edge_service)) = edge else {
        return PgBouncerState::default();
    };

    PgBouncerState {
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

/// If `service_id` is a PgBouncer/HAProxy edge child, resolves to its parent
/// (the actual database root) -- mirrors `PgBouncerSection.tsx`'s
/// `templateRootServiceId` so commands invoked with `--service` pointed at an
/// edge node still operate on the right root.
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

    fn service_with_image(image: &str) -> ServiceInstance {
        ServiceInstance {
            source: Some(ServiceSource {
                image: Some(image.to_string()),
                ..ServiceSource::default()
            }),
            ..ServiceInstance::default()
        }
    }

    #[test]
    fn official_image_detection() {
        assert!(is_official_postgres_image(Some(
            "ghcr.io/railwayapp-templates/postgres-ssl:16"
        )));
        assert!(is_official_postgres_image(Some(
            "ghcr.io/railwayapp-templates/postgres-ha/postgres-patroni:16.4"
        )));
        assert!(!is_official_postgres_image(Some("postgis/postgis:16")));
        assert!(!is_official_postgres_image(None));
    }

    #[test]
    fn minor_pin_detection() {
        assert!(is_minor_pinned_postgres_image(Some(
            "ghcr.io/railwayapp-templates/postgres-ssl:16.10"
        )));
        assert!(!is_minor_pinned_postgres_image(Some(
            "ghcr.io/railwayapp-templates/postgres-ssl:16"
        )));
        assert!(!is_minor_pinned_postgres_image(Some(
            "ghcr.io/railwayapp-templates/postgres-ssl:latest"
        )));
        assert!(!is_minor_pinned_postgres_image(Some(
            "postgis/postgis:16.1"
        )));
    }

    #[test]
    fn pitr_state_reads_bucket_var_and_guardrails() {
        let mut service = service_with_image("ghcr.io/railwayapp-templates/postgres-ssl:16.10");
        service.variables.insert(
            WAL_ARCHIVE_BUCKET_VAR.to_string(),
            Some(Variable {
                value: Some("my-bucket".to_string()),
                ..Variable::default()
            }),
        );
        service.deploy = Some(DeployConfig {
            start_command: Some("./wrapper.sh".to_string()),
            ..DeployConfig::default()
        });

        let state = compute_pitr_state(&service);
        assert!(state.enabled);
        assert!(state.bucket_wired);
        assert!(state.minor_pinned);
        assert!(!state.unsupported_image);
        assert!(state.has_start_command);
    }

    #[test]
    fn pitr_state_bucket_present_but_empty_is_not_wired() {
        let mut service = service_with_image("ghcr.io/railwayapp-templates/postgres-ssl:16");
        service.variables.insert(
            WAL_ARCHIVE_BUCKET_VAR.to_string(),
            Some(Variable {
                value: Some(String::new()),
                ..Variable::default()
            }),
        );

        let state = compute_pitr_state(&service);
        assert!(state.enabled);
        assert!(!state.bucket_wired);
    }

    #[test]
    fn pitr_state_disabled_when_no_bucket_var() {
        let service = service_with_image("ghcr.io/railwayapp-templates/postgres-ssl:16");
        let state = compute_pitr_state(&service);
        assert!(!state.enabled);
        assert!(!state.bucket_wired);
        assert!(!state.has_start_command);
    }

    fn config_with(services: Vec<(&str, ServiceInstance)>) -> EnvironmentConfig {
        let mut config = EnvironmentConfig::default();
        for (id, service) in services {
            config.services.insert(id.to_string(), service);
        }
        config
    }

    fn root_service(patroni_enabled: bool) -> ServiceInstance {
        let mut service = ServiceInstance {
            cluster_role: Some("root".to_string()),
            ..ServiceInstance::default()
        };
        if patroni_enabled {
            service.variables.insert(
                PATRONI_ENABLED_VAR.to_string(),
                Some(Variable {
                    value: Some("true".to_string()),
                    ..Variable::default()
                }),
            );
        }
        service
    }

    fn child_service(parent_id: &str, cluster_role: &str) -> ServiceInstance {
        ServiceInstance {
            parent_service_id: Some(parent_id.to_string()),
            cluster_role: Some(cluster_role.to_string()),
            ..ServiceInstance::default()
        }
    }

    #[test]
    fn ha_state_resolves_root_and_members_via_parent_chain() {
        let config = config_with(vec![
            ("root", root_service(true)),
            ("replica-1", child_service("root", "replica")),
            ("etcd-1", child_service("root", "internal")),
            ("haproxy", child_service("root", "edge")),
        ]);
        let names: BTreeMap<String, String> = [
            ("root", "postgres"),
            ("replica-1", "postgres-replica-1"),
            ("etcd-1", "etcd-1"),
            ("haproxy", "haproxy"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        // Querying via a non-root member still resolves the same root+members.
        let state = compute_ha_state(&config, "replica-1", &names);
        assert!(state.is_cluster);
        assert_eq!(state.root_service_id.as_deref(), Some("root"));
        assert_eq!(state.members.len(), 4);
        assert!(
            state
                .members
                .iter()
                .any(|m| m.service_id == "root" && m.cluster_role.as_deref() == Some("root"))
        );
    }

    #[test]
    fn ha_state_not_a_cluster_without_patroni_enabled() {
        // Root has a PgBouncer child but PATRONI_ENABLED is not set -- this is
        // a pooled-standalone root, not an HA cluster.
        let config = config_with(vec![
            ("root", root_service(false)),
            ("pgbouncer", child_service("root", "edge")),
        ]);
        let state = compute_ha_state(&config, "root", &BTreeMap::new());
        assert!(!state.is_cluster);
        assert_eq!(state.root_service_id.as_deref(), Some("root"));
    }

    #[test]
    fn ha_state_unknown_service_returns_default() {
        let config = config_with(vec![("root", root_service(true))]);
        let state = compute_ha_state(&config, "missing", &BTreeMap::new());
        assert!(!state.is_cluster);
        assert!(state.root_service_id.is_none());
        assert!(state.members.is_empty());
    }

    #[test]
    fn pgbouncer_state_finds_attached_edge_and_knobs() {
        let mut pgbouncer = child_service("root", "edge");
        pgbouncer.source = Some(ServiceSource {
            image: Some("ghcr.io/railwayapp-templates/pgbouncer:latest".to_string()),
            ..ServiceSource::default()
        });
        pgbouncer.variables.insert(
            POOL_MODE_VAR.to_string(),
            Some(Variable {
                value: Some("transaction".to_string()),
                ..Variable::default()
            }),
        );
        pgbouncer.variables.insert(
            MAX_CLIENT_CONN_VAR.to_string(),
            Some(Variable {
                value: Some("100".to_string()),
                ..Variable::default()
            }),
        );

        let config = config_with(vec![
            ("root", root_service(false)),
            ("pgbouncer", pgbouncer),
        ]);
        let state = compute_pgbouncer_state(&config, "root");
        assert!(state.attached);
        assert_eq!(state.edge_service_id.as_deref(), Some("pgbouncer"));
        assert_eq!(state.pool_mode.as_deref(), Some("transaction"));
        assert_eq!(state.max_client_conn, Some(100));
    }

    #[test]
    fn pgbouncer_state_not_attached_without_matching_child() {
        let config = config_with(vec![("root", root_service(false))]);
        let state = compute_pgbouncer_state(&config, "root");
        assert!(!state.attached);
        assert!(state.edge_service_id.is_none());
    }

    #[test]
    fn resolve_root_service_id_follows_edge_parent() {
        let config = config_with(vec![
            ("root", root_service(false)),
            ("pgbouncer", child_service("root", "edge")),
        ]);
        assert_eq!(resolve_root_service_id(&config, "pgbouncer"), "root");
        assert_eq!(resolve_root_service_id(&config, "root"), "root");
        assert_eq!(resolve_root_service_id(&config, "unknown"), "unknown");
    }

    #[test]
    fn resolve_root_service_id_edge_without_parent_falls_back_to_itself() {
        let mut orphan_edge = ServiceInstance {
            cluster_role: Some("edge".to_string()),
            ..ServiceInstance::default()
        };
        orphan_edge.parent_service_id = None;
        let config = config_with(vec![("edge", orphan_edge)]);
        assert_eq!(resolve_root_service_id(&config, "edge"), "edge");
    }

    #[test]
    fn ha_state_survives_a_parent_cycle() {
        // Corrupt config where two services point at each other as parents --
        // the walk must terminate (cycle guard) instead of hanging.
        let config = config_with(vec![
            ("a", child_service("b", "replica")),
            ("b", child_service("a", "replica")),
        ]);
        let state = compute_ha_state(&config, "a", &BTreeMap::new());
        assert!(!state.is_cluster);
        assert_eq!(state.root_service_id.as_deref(), Some("a"));
    }

    #[test]
    fn pgbouncer_state_skips_deleted_edge_child() {
        let mut pgbouncer = child_service("root", "edge");
        pgbouncer.source = Some(ServiceSource {
            image: Some("ghcr.io/railwayapp-templates/pgbouncer:latest".to_string()),
            ..ServiceSource::default()
        });
        pgbouncer.is_deleted = Some(true);

        let config = config_with(vec![
            ("root", root_service(false)),
            ("pgbouncer", pgbouncer),
        ]);
        assert!(!compute_pgbouncer_state(&config, "root").attached);
    }

    #[test]
    fn pgbouncer_state_ignores_non_pgbouncer_children() {
        let mut haproxy = child_service("root", "edge");
        haproxy.source = Some(ServiceSource {
            image: Some("ghcr.io/railwayapp-templates/haproxy:latest".to_string()),
            ..ServiceSource::default()
        });
        let config = config_with(vec![("root", root_service(true)), ("haproxy", haproxy)]);
        assert!(!compute_pgbouncer_state(&config, "root").attached);
    }

    #[test]
    fn pgbouncer_state_tolerates_unparseable_knob_values() {
        let mut pgbouncer = child_service("root", "edge");
        pgbouncer.source = Some(ServiceSource {
            image: Some("ghcr.io/railwayapp-templates/pgbouncer:latest".to_string()),
            ..ServiceSource::default()
        });
        pgbouncer.variables.insert(
            MAX_CLIENT_CONN_VAR.to_string(),
            Some(Variable {
                value: Some("not-a-number".to_string()),
                ..Variable::default()
            }),
        );
        let config = config_with(vec![
            ("root", root_service(false)),
            ("pgbouncer", pgbouncer),
        ]);
        let state = compute_pgbouncer_state(&config, "root");
        assert!(state.attached);
        assert_eq!(state.max_client_conn, None);
    }

    #[test]
    fn pitr_state_whitespace_start_command_does_not_count() {
        let mut service = service_with_image("ghcr.io/railwayapp-templates/postgres-ssl:16");
        service.deploy = Some(DeployConfig {
            start_command: Some("   ".to_string()),
            ..DeployConfig::default()
        });
        assert!(!compute_pitr_state(&service).has_start_command);
    }

    #[test]
    fn minor_pin_detection_edge_tags() {
        // A suffixed minor tag (e.g. `-alpine`) is not a plain minor pin.
        assert!(!is_minor_pinned_postgres_image(Some(
            "ghcr.io/railwayapp-templates/postgres-ssl:16.10-alpine"
        )));
        // No tag at all.
        assert!(!is_minor_pinned_postgres_image(Some(
            "ghcr.io/railwayapp-templates/postgres-ssl"
        )));
        assert!(!is_minor_pinned_postgres_image(None));
    }

    #[test]
    fn ha_state_members_sorted_by_name() {
        let config = config_with(vec![
            ("root", root_service(true)),
            ("z", child_service("root", "replica")),
            ("a", child_service("root", "replica")),
        ]);
        let names: BTreeMap<String, String> =
            [("root", "db"), ("z", "zz-replica"), ("a", "aa-replica")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        let state = compute_ha_state(&config, "root", &names);
        let ordered: Vec<&str> = state
            .members
            .iter()
            .map(|m| m.service_name.as_str())
            .collect();
        assert_eq!(ordered, vec!["aa-replica", "db", "zz-replica"]);
    }
}
