#![allow(dead_code)]

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;

use crate::{client::post_graphql, config::Configs, gql::queries};

/// Root environment config from `environment.config` GraphQL field
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentConfig {
    #[serde(default)]
    pub services: BTreeMap<String, ServiceInstance>,
    #[serde(default)]
    pub shared_variables: BTreeMap<String, Variable>,
    #[serde(default)]
    pub volumes: BTreeMap<String, VolumeInstance>,
    #[serde(default)]
    pub buckets: BTreeMap<String, BucketInstance>,
    #[serde(default)]
    pub private_network_disabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInstance {
    #[serde(default)]
    pub source: Option<ServiceSource>,
    #[serde(default)]
    pub networking: Option<ServiceNetworking>,
    #[serde(default)]
    pub variables: BTreeMap<String, Variable>,
    #[serde(default)]
    pub deploy: Option<DeployConfig>,
    #[serde(default)]
    pub build: Option<BuildConfig>,
    #[serde(default)]
    pub volume_mounts: BTreeMap<String, VolumeMount>,
    #[serde(default)]
    pub is_deleted: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSource {
    pub image: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub root_directory: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServiceNetworking {
    #[serde(default)]
    pub service_domains: BTreeMap<String, Option<DomainConfig>>,
    #[serde(default)]
    pub custom_domains: BTreeMap<String, Option<DomainConfig>>,
    #[serde(default)]
    pub tcp_proxies: BTreeMap<String, Option<TcpProxyConfig>>,
    pub private_network_endpoint: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DomainConfig {
    pub port: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TcpProxyConfig {}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub value: Option<String>,
    pub default_value: Option<String>,
    pub description: Option<String>,
    pub is_optional: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeployConfig {
    pub start_command: Option<String>,
    pub healthcheck_path: Option<String>,
    pub num_replicas: Option<i64>,
    pub cron_schedule: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BuildConfig {
    pub builder: Option<String>,
    pub build_command: Option<String>,
    pub dockerfile_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInstance {
    pub size_mb: Option<i64>,
    pub region: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BucketInstance {
    pub region: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VolumeMount {
    pub mount_path: Option<String>,
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

    pub fn get_env_vars(&self) -> BTreeMap<String, String> {
        self.variables
            .iter()
            .filter_map(|(k, v)| v.value.clone().map(|val| (k.clone(), val)))
            .collect()
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
