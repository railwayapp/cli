// Fields on deserialization structs may not all be read
#![allow(dead_code)]

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use reqwest::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::{client::post_graphql, config::Configs, gql::queries};

/// Root environment config from `environment.config` GraphQL field
#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct EnvironmentConfig {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub services: BTreeMap<String, ServiceInstance>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub shared_variables: BTreeMap<String, Option<Variable>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub volumes: BTreeMap<String, VolumeInstance>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub buckets: BTreeMap<String, BucketInstance>,
    pub private_network_disabled: Option<bool>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct ServiceInstance {
    pub source: Option<ServiceSource>,
    pub networking: Option<ServiceNetworking>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub variables: BTreeMap<String, Option<Variable>>,
    pub config_file: Option<String>,
    pub deploy: Option<DeployConfig>,
    pub build: Option<BuildConfig>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub volume_mounts: BTreeMap<String, VolumeMount>,
    pub is_deleted: Option<bool>,
    pub is_created: Option<bool>,
    pub parent_service_id: Option<String>,
    /// Cluster membership role stamped by template conversion/scaling:
    /// "root" | "replica" | "internal" | "edge". `None` for services that
    /// aren't part of a cluster (or predate this field).
    pub cluster_role: Option<String>,
    /// Canvas group id, used to keep live-scaled replica/internal nodes
    /// visually grouped with their cluster root (`groupSet`).
    pub group_id: Option<String>,
    /// Template code of the HA companion to deploy when converting this
    /// standalone service, declared by its origin template. `None` for
    /// services provisioned before templates carried the field, and for
    /// legacy deploys with no template link at all -- those fall back to the
    /// engine registry's companion (see
    /// `database_engines::DatabaseEngine::ha_template_code_for`).
    pub ha_template_code: Option<String>,
    /// The inverse of `ha_template_code`: the standalone template the root
    /// falls back to when the cluster is reverted. Set on a cluster root.
    pub reverts_to_template_code: Option<String>,
    /// Name of the variable the HA agent sets to "true" when the cluster is
    /// active (e.g. `PATRONI_ENABLED`, `SENTINEL_ENABLED`, `GR_ENABLED`). This
    /// is what makes "is this actually a cluster?" a declared question rather
    /// than a per-engine hardcode.
    pub ha_active_variable: Option<String>,
    /// Template-authored bounds for the HA conversion flow -- which roles the
    /// engine's cluster even has, the counts each accepts, and the image
    /// majors its companion publishes data-node images for.
    pub ha_conversion_config: Option<HaConversionConfig>,
    /// Template-declared coordination-variable wiring for HA scale helpers,
    /// stamped on the root service at conversion time. `None` for legacy
    /// (pre-`clusterWiring`) Patroni clusters -- callers fall back to the
    /// historical hardcoded Patroni wiring in that case (see
    /// `cluster_scale::resolve_cluster_wiring`).
    pub cluster_wiring: Option<ClusterWiring>,
}

/// Template-authored configuration for the HA conversion flow, mirroring
/// `haConversionConfigSchema` in
/// `common/javascript/models/src/environment/schema.ts`.
///
/// The CLI reads this rather than hardcoding per-engine topology: a cluster
/// whose template declares no `internal` selector has no coordinator nodes at
/// all (Redis colocates Sentinel, MySQL's Group Replication is built in), so
/// `--coordinators` is refused for it, and `supportedImageMajorVersions` is
/// what the conversion gate accepts -- shipping a new major stays a template
/// update rather than a CLI release.
#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct HaConversionConfig {
    pub description: Option<String>,
    pub replica: Option<HaConversionRoleSelector>,
    pub internal: Option<HaConversionRoleSelector>,
    pub edge: Option<HaConversionRoleSelector>,
    /// Image majors the HA companion publishes data-node images for.
    pub supported_image_major_versions: Option<Vec<i64>>,
    /// Pin conversion to the source's exact `major.minor` rather than the bare
    /// major. Declared where the HA repo publishes minor alias tags and the
    /// engine's replication is not minor-agnostic.
    pub pin_to_minor_version: Option<bool>,
}

/// One role's selector in the conversion flow: what to call it, and which
/// counts it accepts.
#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct HaConversionRoleSelector {
    pub label: Option<String>,
    pub description: Option<String>,
    /// Singular display noun for a node of this role (e.g. "Redis", "etcd").
    pub node_label: Option<String>,
    /// The counts the template allows for this role. Empty/absent means the
    /// template declares no bound and any non-negative count is accepted.
    pub options: Option<Vec<i64>>,
    pub default_value: Option<i64>,
}

/// Coordinates of a declared HTTP probe against a node's own private address:
/// `GET <path>` on `<port>`, any 2xx meaning healthy. The platform carries no
/// knowledge of what is listening -- it probes what the template points it at.
#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct HttpEndpoint {
    pub port: Option<i64>,
    pub path: Option<String>,
}

/// A rich member-status API the data nodes expose, when the topology has one.
/// Unlike the plain HTTP probes, speaking it needs a protocol client, so the
/// CLI acts on this only when `protocol` names one it implements -- an unknown
/// protocol is treated exactly like no declaration at all.
#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct MemberStatusApi {
    pub protocol: Option<String>,
    pub port: Option<i64>,
}

/// Template-declared wiring map for HA scale helpers, mirroring
/// `clusterWiringSchema` in `common/javascript/models/src/environment/schema.ts`.
/// Lets `railway postgres ha scale` re-stamp coordination variables on an
/// already-converted cluster without hardcoding any database-specific
/// variable names. Set on the root service's `environment.config` entry.
///
/// In each format string, `{host}` is substituted with the node's
/// private-domain reference and `{rootName}` with the cluster root's actual
/// (possibly customer-renamed) service name.
#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ClusterWiring {
    /// Variable on each internal service that holds that node's own identity
    /// (e.g. `ETCD_NAME`). Stamped per node on internal scale.
    pub internal_node_name_variable: Option<String>,
    /// Variable on root + replica services that holds the comma-separated
    /// coordinator hostname list (e.g. `PATRONI_ETCD3_HOSTS`).
    pub coordinator_hosts_variable: Option<String>,
    /// Port appended to each coordinator host (e.g. 2379 for etcd).
    pub coordinator_port: Option<i64>,
    /// Variable on each replica service that holds that replica's own
    /// identity (e.g. `PATRONI_NAME`). Stamped per replica on clone.
    pub replica_node_name_variable: Option<String>,
    /// Variable on the edge service that holds the comma-separated data-node
    /// endpoint list (e.g. `POSTGRES_NODES`).
    pub data_nodes_variable: Option<String>,
    /// Format for each data-node entry. `{host}`/`{rootName}` are
    /// substituted as described above; the rest is literal.
    pub data_nodes_entry_format: Option<String>,
    /// Variable on root + replica services holding the consensus quorum.
    /// Restamped to a majority (`floor(dataNodes / 2) + 1`) on replica scale.
    pub quorum_variable: Option<String>,
    /// The data nodes host their own consensus voters but expose no quorum
    /// variable to restamp -- the coordinator derives its majority from live
    /// membership (MySQL Group Replication). Drives the same
    /// odd-count-of-at-least-three fence `quorum_variable` does, with no
    /// stamping. Declaring `quorum_variable` already implies this.
    pub data_nodes_are_quorum_voters: Option<bool>,
    /// Variable on root + replica services holding the comma-separated peer
    /// list each node's own colocated coordinator boots against (e.g.
    /// `SENTINEL_HOSTS`, `GR_SEEDS`). Unlike `data_nodes_variable` this lands
    /// on the data nodes rather than the edge, and scale stamps it on newly
    /// added nodes ONLY: a node that joins later must know the real membership
    /// at first boot, while an existing node already read its own copy and
    /// restamping it would only mark the whole fleet stale.
    pub peer_hosts_variable: Option<String>,
    /// Format for each peer entry (e.g. `{host}:26379`). `{host}`/`{rootName}`
    /// are substituted as described above; the rest is literal.
    pub peer_hosts_entry_format: Option<String>,
    /// Health probe against the cluster's routing edge node.
    pub edge_health_check: Option<HttpEndpoint>,
    /// Health probe against each data (root/replica) node -- the generic
    /// equivalent of a coordinator API for topologies that expose none.
    pub data_node_health_check: Option<HttpEndpoint>,
    /// Health probe against each internal (coordinator) node.
    pub internal_node_health_check: Option<HttpEndpoint>,
    /// Role probe against each data node: 200 = this node is the one its own
    /// coordinator currently treats as primary, 503 = it is not, anything else
    /// = unknown.
    pub data_node_role_check: Option<HttpEndpoint>,
    /// Rich member-status API the data nodes expose, when the topology has one
    /// (e.g. Patroni's REST API).
    pub member_status_api: Option<MemberStatusApi>,
    /// Same transport as `data_node_role_check`, but an ACTION: POST against a
    /// data node asks that node's own colocated coordinator to make THAT node
    /// the primary. 2xx means the handoff was accepted -- confirmation comes
    /// from `data_node_role_check` flipping, never from this response.
    pub data_node_switchover: Option<HttpEndpoint>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct ServiceSource {
    pub image: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub upstream_url: Option<String>,
    pub root_directory: Option<String>,
    pub check_suites: Option<bool>,
    pub auto_updates: Option<AutoUpdates>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct AutoUpdates {
    pub r#type: Option<String>, // disabled | patch | minor
    pub schedule: Option<Vec<AutoUpdateSchedule>>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct AutoUpdateSchedule {
    pub day: Option<i64>,
    pub start_hour: Option<i64>,
    pub end_hour: Option<i64>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct ServiceNetworking {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub service_domains: BTreeMap<String, Option<DomainConfig>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_domains: BTreeMap<String, Option<DomainConfig>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tcp_proxies: BTreeMap<String, Option<TcpProxyConfig>>,
    pub private_network_endpoint: Option<String>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default)]
pub struct DomainConfig {
    pub port: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default)]
pub struct TcpProxyConfig {}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct Variable {
    pub value: Option<String>,
    pub default_value: Option<String>,
    pub description: Option<String>,
    pub is_optional: Option<bool>,
    pub is_sealed: Option<bool>,
    pub generator: Option<String>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct RegistryCredentials {
    pub username: Option<String>,
    pub password: Option<String>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct LimitOverride {
    pub containers: Option<ContainerLimits>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct ContainerLimits {
    pub cpu: Option<f64>,
    pub memory_bytes: Option<i64>,
    pub disk_bytes: Option<i64>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct DeployConfig {
    pub start_command: Option<String>,
    pub pre_deploy_command: Option<serde_json::Value>, // string or [string]
    pub healthcheck_path: Option<String>,
    pub healthcheck_timeout: Option<i64>,
    pub ipv6_egress_enabled: Option<bool>,
    pub num_replicas: Option<i64>,
    pub multi_region_config: Option<BTreeMap<String, Option<RegionConfig>>>,
    pub cron_schedule: Option<String>,
    pub restart_policy_type: Option<String>, // ON_FAILURE | ALWAYS | NEVER
    pub restart_policy_max_retries: Option<i64>,
    pub sleep_application: Option<bool>,
    pub registry_credentials: Option<RegistryCredentials>,
    pub limit_override: Option<LimitOverride>,
    pub required_mount_path: Option<String>,
    pub overlap_seconds: Option<i64>,
    pub draining_seconds: Option<i64>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct RegionConfig {
    pub num_replicas: Option<i64>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct BuildConfig {
    pub builder: Option<String>, // NIXPACKS | DOCKERFILE | RAILPACK
    pub build_command: Option<String>,
    pub build_environment: Option<String>, // V2 | V3
    pub dockerfile_path: Option<String>,
    pub watch_patterns: Option<Vec<String>>,
    pub nixpacks_config_path: Option<String>,
    pub nixpacks_plan: Option<serde_json::Value>,
    pub nixpacks_version: Option<String>,
    pub railpack_version: Option<String>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct VolumeInstance {
    pub size_mb: Option<i64>,
    pub region: Option<String>,
    pub alerts: Option<serde_json::Value>,
    pub is_deleted: Option<bool>,
    pub is_created: Option<bool>,
    pub allow_online_resize: Option<bool>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct BucketInstance {
    pub region: Option<String>,
    pub is_deleted: Option<bool>,
    pub is_created: Option<bool>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct VolumeMount {
    pub mount_path: Option<String>,
    pub backup_schedules: Option<Vec<String>>, // DAILY | WEEKLY | MONTHLY
}

impl ServiceInstance {
    pub fn is_image_based(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|s| s.image.is_some() && s.repo.is_none())
    }

    pub fn is_code_based(&self) -> bool {
        self.source.as_ref().is_none_or(|s| s.image.is_none())
    }

    pub fn get_ports(&self) -> Vec<i64> {
        let mut ports = Vec::new();
        if let Some(networking) = &self.networking {
            for config in networking.service_domains.values().flatten() {
                if let Some(port) = config.port {
                    if !ports.contains(&port) {
                        ports.push(port);
                    }
                }
            }
            for port_str in networking.tcp_proxies.keys() {
                if let Ok(port) = port_str.parse::<i64>() {
                    if !ports.contains(&port) {
                        ports.push(port);
                    }
                }
            }
        }
        ports
    }
}

/// Response from fetch_environment_config containing config and metadata
pub struct EnvironmentConfigResponse {
    pub config: EnvironmentConfig,
    pub name: String,
}

/// Fetch environment config from Railway API
pub async fn fetch_environment_config(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    decrypt_variables: bool,
) -> Result<EnvironmentConfigResponse> {
    let vars = queries::get_environment_config::Variables {
        id: environment_id.to_string(),
        decrypt_variables: Some(decrypt_variables),
    };

    let data =
        post_graphql::<queries::GetEnvironmentConfig, _>(client, configs.get_backboard(), vars)
            .await?;

    let config: EnvironmentConfig = serde_json::from_value(data.environment.config)
        .context("Failed to parse environment config")?;

    Ok(EnvironmentConfigResponse {
        config,
        name: data.environment.name,
    })
}

/// Prepare an environment config for duplication by marking all services and volumes
/// as needing creation in the target environment.
pub fn prepare_config_for_duplication(mut config: EnvironmentConfig) -> EnvironmentConfig {
    // Mark all services as needing creation
    for service in config.services.values_mut() {
        service.is_created = Some(true);
    }

    // Mark all volumes as needing creation
    for volume in config.volumes.values_mut() {
        volume.is_created = Some(true);
    }

    // Mark all buckets as needing creation
    for bucket in config.buckets.values_mut() {
        bucket.is_created = Some(true);
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MockBackboard;
    use serde_json::json;

    fn environment_payload(config: serde_json::Value) -> serde_json::Value {
        json!({
            "environment": {
                "id": "env-1",
                "name": "production",
                "config": config,
            }
        })
    }

    #[tokio::test]
    async fn parses_services_and_tolerates_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub(
            "GetEnvironmentConfig",
            environment_payload(json!({
                "services": {
                    "svc-1": {
                        "source": { "image": "ghcr.io/railwayapp-templates/postgres-ssl:16" },
                        "variables": { "PGDATA": { "value": "/var/lib/postgresql/data" } },
                        "parentServiceId": "svc-0",
                        // Fields newer than this build must be ignored, not
                        // fail the parse -- the blob evolves server-side.
                        "someFutureField": { "nested": true },
                    }
                },
                "unknownTopLevelSection": [1, 2, 3],
            })),
        );

        let configs = server.configs(&dir);
        let client = reqwest::Client::new();
        let response = fetch_environment_config(&client, &configs, "env-1", false)
            .await
            .unwrap();

        assert_eq!(response.name, "production");
        let service = response.config.services.get("svc-1").unwrap();
        assert_eq!(
            service.source.as_ref().unwrap().image.as_deref(),
            Some("ghcr.io/railwayapp-templates/postgres-ssl:16")
        );
        assert_eq!(service.parent_service_id.as_deref(), Some("svc-0"));

        // The decrypt flag must reach the wire.
        assert_eq!(
            server.variables_for("GetEnvironmentConfig"),
            vec![json!({ "id": "env-1", "decryptVariables": false })]
        );
    }

    #[tokio::test]
    async fn decrypt_flag_is_passed_through() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub("GetEnvironmentConfig", environment_payload(json!({})));

        let configs = server.configs(&dir);
        let client = reqwest::Client::new();
        fetch_environment_config(&client, &configs, "env-1", true)
            .await
            .unwrap();
        assert_eq!(
            server.variables_for("GetEnvironmentConfig")[0]["decryptVariables"],
            json!(true)
        );
    }

    #[tokio::test]
    async fn malformed_config_blob_fails_with_context() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub(
            "GetEnvironmentConfig",
            environment_payload(json!({ "services": "not-an-object" })),
        );

        let configs = server.configs(&dir);
        let client = reqwest::Client::new();
        let Err(err) = fetch_environment_config(&client, &configs, "env-1", false).await else {
            panic!("malformed config must fail to parse");
        };
        assert!(
            format!("{err:#}").contains("Failed to parse environment config"),
            "unexpected error: {err:#}"
        );
    }
}
