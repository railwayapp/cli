#![allow(dead_code)]

use std::collections::BTreeMap;

use serde::Deserialize;

/// Root environment config from `environment.config` GraphQL field
#[derive(Debug, Deserialize, Default)]
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

#[derive(Debug, Deserialize, Default)]
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

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSource {
    pub image: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub root_directory: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
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

#[derive(Debug, Deserialize, Default)]
pub struct DomainConfig {
    pub port: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TcpProxyConfig {}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub value: Option<String>,
    pub default_value: Option<String>,
    pub description: Option<String>,
    pub is_optional: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeployConfig {
    pub start_command: Option<String>,
    pub healthcheck_path: Option<String>,
    pub num_replicas: Option<i64>,
    pub cron_schedule: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BuildConfig {
    pub builder: Option<String>,
    pub build_command: Option<String>,
    pub dockerfile_path: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInstance {
    pub size_mb: Option<i64>,
    pub region: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BucketInstance {
    pub region: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
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
