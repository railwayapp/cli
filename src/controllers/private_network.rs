use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use reqwest::Client;
use serde::Serialize;

use crate::{
    client::post_graphql,
    config::Configs,
    controllers::config::{
        EnvironmentConfig, ServiceInstance, ServiceNetworking,
        environment::fetch_environment_config,
    },
    gql::{mutations, queries},
    util::retry::{RetryConfig, retry_with_backoff},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PatchMode {
    Commit,
    Stage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateNetwork {
    pub public_id: String,
    pub project_id: String,
    pub environment_id: String,
    pub name: String,
    pub dns_name: String,
    pub network_id: i64,
    pub tags: Vec<String>,
    pub supports_ipv4: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateNetworkEndpoint {
    pub public_id: String,
    pub service_instance_id: String,
    pub dns_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_dns_name: Option<String>,
    pub private_ips: Vec<String>,
    pub tags: Vec<String>,
    pub sync_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

pub fn parse_endpoint_prefix(value: &str) -> std::result::Result<String, String> {
    validate_endpoint_prefix(value).map_err(|error| error.to_string())
}

pub fn validate_endpoint_prefix(value: &str) -> Result<String> {
    let prefix = value.trim();

    if prefix.is_empty() {
        bail!("endpoint name cannot be empty");
    }
    if prefix.len() > 63 {
        bail!("endpoint name must be 63 characters or fewer");
    }
    if prefix.starts_with('-') || prefix.ends_with('-') {
        bail!("endpoint name cannot start or end with a hyphen");
    }
    if !prefix
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        bail!("endpoint name must use only lowercase letters, numbers, and hyphens");
    }

    Ok(prefix.to_string())
}

pub async fn fetch_private_networks(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
) -> Result<Vec<PrivateNetwork>> {
    let networks = post_graphql::<queries::PrivateNetworks, _>(
        client,
        configs.get_backboard(),
        queries::private_networks::Variables {
            environment_id: environment_id.to_string(),
        },
    )
    .await?
    .private_networks;

    Ok(networks.iter().map(private_network_output).collect())
}

pub fn private_network_output(
    network: &queries::private_networks::PrivateNetworksPrivateNetworks,
) -> PrivateNetwork {
    PrivateNetwork {
        public_id: network.public_id.clone(),
        project_id: network.project_id.clone(),
        environment_id: network.environment_id.clone(),
        name: network.name.clone(),
        dns_name: network.dns_name.clone(),
        network_id: network.network_id,
        supports_ipv4: supports_ipv4(&network.tags),
        tags: network.tags.clone(),
        created_at: network
            .created_at
            .as_ref()
            .map(chrono::DateTime::to_rfc3339),
        deleted_at: network
            .deleted_at
            .as_ref()
            .map(chrono::DateTime::to_rfc3339),
    }
}

pub fn supports_ipv4(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == "SUPPORTS_IPV4_PRIVNETS")
}

pub fn resolve_default_network(networks: &[PrivateNetwork]) -> Option<&PrivateNetwork> {
    networks
        .iter()
        .find(|network| network.deleted_at.is_none() && network.name == "railway")
        .or_else(|| networks.iter().find(|network| network.deleted_at.is_none()))
        .or_else(|| networks.first())
}

pub fn find_private_network<'a>(
    networks: &'a [PrivateNetwork],
    identifier: &str,
) -> Result<Option<&'a PrivateNetwork>> {
    let identifier = identifier.trim();

    if let Some(network) = networks.iter().find(|network| {
        network.public_id.eq_ignore_ascii_case(identifier)
            || network.name.eq_ignore_ascii_case(identifier)
            || network.dns_name.eq_ignore_ascii_case(identifier)
            || network.network_id.to_string() == identifier
    }) {
        return Ok(Some(network));
    }

    Ok(None)
}

pub fn resolve_private_network<'a>(
    networks: &'a [PrivateNetwork],
    identifier: Option<&str>,
) -> Result<Option<&'a PrivateNetwork>> {
    if let Some(identifier) = identifier {
        return find_private_network(networks, identifier)?
            .map(Some)
            .ok_or_else(|| {
                anyhow!(
                    "Private network '{}' not found in the selected environment",
                    identifier
                )
            });
    }

    Ok(resolve_default_network(networks))
}

pub async fn fetch_private_network_endpoint(
    client: &Client,
    configs: &Configs,
    private_network_id: &str,
    environment_id: &str,
    service_id: &str,
) -> Result<Option<PrivateNetworkEndpoint>> {
    let endpoint = post_graphql::<queries::PrivateNetworkEndpoint, _>(
        client,
        configs.get_backboard(),
        queries::private_network_endpoint::Variables {
            private_network_id: private_network_id.to_string(),
            environment_id: environment_id.to_string(),
            service_id: service_id.to_string(),
        },
    )
    .await?
    .private_network_endpoint;

    Ok(endpoint.as_ref().map(private_network_endpoint_output))
}

pub fn private_network_endpoint_output(
    endpoint: &queries::private_network_endpoint::PrivateNetworkEndpointPrivateNetworkEndpoint,
) -> PrivateNetworkEndpoint {
    PrivateNetworkEndpoint {
        public_id: endpoint.public_id.clone(),
        service_instance_id: endpoint.service_instance_id.clone(),
        dns_name: endpoint.dns_name.clone(),
        new_dns_name: endpoint.new_dns_name.clone(),
        private_ips: endpoint.private_ips.clone(),
        tags: endpoint.tags.clone(),
        sync_status: endpoint_sync_status(&endpoint.sync_status),
        created_at: endpoint
            .created_at
            .as_ref()
            .map(chrono::DateTime::to_rfc3339),
        deleted_at: endpoint
            .deleted_at
            .as_ref()
            .map(chrono::DateTime::to_rfc3339),
    }
}

pub fn endpoint_sync_status(
    value: &queries::private_network_endpoint::PrivateNetworkEndpointSyncStatus,
) -> String {
    match value {
        queries::private_network_endpoint::PrivateNetworkEndpointSyncStatus::ACTIVE => {
            "ACTIVE".to_string()
        }
        queries::private_network_endpoint::PrivateNetworkEndpointSyncStatus::CREATING => {
            "CREATING".to_string()
        }
        queries::private_network_endpoint::PrivateNetworkEndpointSyncStatus::DELETED => {
            "DELETED".to_string()
        }
        queries::private_network_endpoint::PrivateNetworkEndpointSyncStatus::DELETING => {
            "DELETING".to_string()
        }
        queries::private_network_endpoint::PrivateNetworkEndpointSyncStatus::UNSPECIFIED => {
            "UNSPECIFIED".to_string()
        }
        queries::private_network_endpoint::PrivateNetworkEndpointSyncStatus::UPDATING => {
            "UPDATING".to_string()
        }
        queries::private_network_endpoint::PrivateNetworkEndpointSyncStatus::Other(status) => {
            status.clone()
        }
    }
}

pub async fn private_network_endpoint_name_available(
    client: &Client,
    configs: &Configs,
    private_network_id: &str,
    environment_id: &str,
    prefix: &str,
) -> Result<bool> {
    Ok(
        post_graphql::<queries::PrivateNetworkEndpointNameAvailable, _>(
            client,
            configs.get_backboard(),
            queries::private_network_endpoint_name_available::Variables {
                private_network_id: private_network_id.to_string(),
                environment_id: environment_id.to_string(),
                prefix: prefix.to_string(),
            },
        )
        .await?
        .private_network_endpoint_name_available,
    )
}

pub async fn fetch_configured_endpoint_name(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
) -> Result<Option<String>> {
    let config = fetch_environment_config(client, configs, environment_id, false)
        .await?
        .config;

    Ok(config
        .services
        .get(service_id)
        .and_then(|service| service.networking.as_ref())
        .and_then(|networking| networking.private_network_endpoint.clone()))
}

pub async fn apply_enable_patch(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: Option<&str>,
    endpoint: Option<&str>,
    stage: bool,
    commit_message: Option<String>,
) -> Result<PatchMode> {
    let patch = enable_private_network_patch(service_id, endpoint);
    apply_private_network_patch(
        client,
        configs,
        environment_id,
        patch,
        stage,
        commit_message,
    )
    .await
}

pub async fn apply_set_endpoint_patch(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
    endpoint: &str,
    stage: bool,
    commit_message: Option<String>,
) -> Result<PatchMode> {
    let patch = endpoint_patch(service_id, endpoint);
    apply_private_network_patch(
        client,
        configs,
        environment_id,
        patch,
        stage,
        commit_message,
    )
    .await
}

async fn apply_private_network_patch(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    patch: EnvironmentConfig,
    stage: bool,
    commit_message: Option<String>,
) -> Result<PatchMode> {
    if stage {
        post_graphql::<mutations::EnvironmentStageChanges, _>(
            client,
            configs.get_backboard(),
            mutations::environment_stage_changes::Variables {
                environment_id: environment_id.to_string(),
                input: patch,
                merge: Some(true),
            },
        )
        .await?;
        return Ok(PatchMode::Stage);
    }

    post_graphql::<mutations::EnvironmentPatchCommit, _>(
        client,
        configs.get_backboard(),
        mutations::environment_patch_commit::Variables {
            environment_id: environment_id.to_string(),
            patch,
            commit_message,
        },
    )
    .await?;
    Ok(PatchMode::Commit)
}

pub fn enable_private_network_patch(
    service_id: Option<&str>,
    endpoint: Option<&str>,
) -> EnvironmentConfig {
    EnvironmentConfig {
        private_network_disabled: Some(false),
        services: service_id
            .zip(endpoint)
            .map(|(service_id, endpoint)| {
                BTreeMap::from([(
                    service_id.to_string(),
                    ServiceInstance {
                        networking: Some(ServiceNetworking {
                            private_network_endpoint: Some(endpoint.to_string()),
                            ..ServiceNetworking::default()
                        }),
                        ..ServiceInstance::default()
                    },
                )])
            })
            .unwrap_or_default(),
        ..EnvironmentConfig::default()
    }
}

pub fn endpoint_patch(service_id: &str, endpoint: &str) -> EnvironmentConfig {
    EnvironmentConfig {
        services: BTreeMap::from([(
            service_id.to_string(),
            ServiceInstance {
                networking: Some(ServiceNetworking {
                    private_network_endpoint: Some(endpoint.to_string()),
                    ..ServiceNetworking::default()
                }),
                ..ServiceInstance::default()
            },
        )]),
        ..EnvironmentConfig::default()
    }
}

pub async fn verify_private_network_enabled(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
) -> Result<()> {
    retry_with_backoff(verify_retry_config(), || async {
        let config = fetch_environment_config(client, configs, environment_id, false)
            .await?
            .config;

        if config.private_network_disabled == Some(true) {
            bail!(
                "Private networking was requested, but environment config still reports it disabled."
            );
        }

        Ok(())
    })
    .await
}

pub async fn verify_endpoint_configured(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
    endpoint: &str,
) -> Result<()> {
    retry_with_backoff(verify_retry_config(), || async {
        let config = fetch_environment_config(client, configs, environment_id, false)
            .await?
            .config;

        let configured = config
            .services
            .get(service_id)
            .and_then(|service| service.networking.as_ref())
            .and_then(|networking| networking.private_network_endpoint.as_ref())
            .is_some_and(|configured| configured == endpoint);

        if !configured {
            bail!(
                "Private network endpoint '{}' was requested, but it was not present after verification.",
                endpoint
            );
        }

        Ok(())
    })
    .await
}

pub fn internal_url_for_dns_name(network: &PrivateNetwork, dns_name: &str) -> String {
    format!("{}.{}.internal", dns_name, network.dns_name)
}

fn verify_retry_config() -> RetryConfig {
    RetryConfig {
        max_attempts: 6,
        initial_delay_ms: 500,
        max_delay_ms: 3_000,
        backoff_multiplier: 1.8,
        on_retry: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(public_id: &str, name: &str, dns_name: &str, tags: Vec<&str>) -> PrivateNetwork {
        PrivateNetwork {
            public_id: public_id.to_string(),
            project_id: "project_123".to_string(),
            environment_id: "env_123".to_string(),
            name: name.to_string(),
            dns_name: dns_name.to_string(),
            network_id: 42,
            tags: tags.into_iter().map(ToString::to_string).collect(),
            supports_ipv4: false,
            created_at: None,
            deleted_at: None,
        }
    }

    #[test]
    fn default_network_prefers_railway() {
        let fallback = network("pn_fallback", "custom", "custom", vec![]);
        let railway = network("pn_railway", "railway", "railway", vec![]);

        assert_eq!(
            resolve_default_network(&[fallback, railway])
                .unwrap()
                .public_id,
            "pn_railway"
        );
    }

    #[test]
    fn endpoint_prefix_validation_matches_dashboard_label_rules() {
        assert!(validate_endpoint_prefix("api-internal").is_ok());
        assert!(validate_endpoint_prefix("-api").is_err());
        assert!(validate_endpoint_prefix("api-").is_err());
        assert!(validate_endpoint_prefix("API").is_err());
        assert!(validate_endpoint_prefix("api_internal").is_err());
        assert!(validate_endpoint_prefix("").is_err());
    }

    #[test]
    fn supports_ipv4_uses_dashboard_tag() {
        assert!(supports_ipv4(&["SUPPORTS_IPV4_PRIVNETS".to_string()]));
        assert!(!supports_ipv4(&["OTHER".to_string()]));
    }

    #[test]
    fn internal_url_uses_network_dns_suffix() {
        let network = network("pn_railway", "railway", "railway", vec![]);
        let endpoint = PrivateNetworkEndpoint {
            public_id: "pne_123".to_string(),
            service_instance_id: "si_123".to_string(),
            dns_name: "api".to_string(),
            new_dns_name: None,
            private_ips: vec![],
            tags: vec![],
            sync_status: "ACTIVE".to_string(),
            created_at: None,
            deleted_at: None,
        };

        assert_eq!(
            internal_url_for_dns_name(&network, &endpoint.dns_name),
            "api.railway.internal"
        );
        assert_eq!(
            internal_url_for_dns_name(&network, "api-renamed"),
            "api-renamed.railway.internal"
        );
    }

    #[test]
    fn patches_set_dashboard_config_fields() {
        let enable = enable_private_network_patch(Some("svc_123"), Some("api-internal"));
        assert_eq!(enable.private_network_disabled, Some(false));
        assert_eq!(
            enable
                .services
                .get("svc_123")
                .and_then(|service| service.networking.as_ref())
                .and_then(|networking| networking.private_network_endpoint.as_ref()),
            Some(&"api-internal".to_string())
        );

        let endpoint = endpoint_patch("svc_123", "api-renamed");
        assert_eq!(
            endpoint
                .services
                .get("svc_123")
                .and_then(|service| service.networking.as_ref())
                .and_then(|networking| networking.private_network_endpoint.as_ref()),
            Some(&"api-renamed".to_string())
        );
    }
}
