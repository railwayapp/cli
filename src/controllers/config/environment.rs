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
