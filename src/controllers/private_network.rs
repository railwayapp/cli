use anyhow::{Result, anyhow, bail};
use reqwest::Client;
use serde::Serialize;

use crate::{
    client::post_graphql,
    config::Configs,
    gql::{mutations, queries},
};

const DEFAULT_PRIVATE_NETWORK_NAME: &str = "railway";
const IPV4_PRIVATE_NETWORK_TAG: &str = "SUPPORTS_IPV4_PRIVNETS";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateNetwork {
    pub id: String,
    pub project_id: String,
    pub environment_id: String,
    pub name: String,
    pub dns_name: String,
    pub ip_family: String,
    pub network_id: i64,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateNetworkEndpoint {
    pub id: String,
    pub service_instance_id: String,
    pub dns_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_dns_name: Option<String>,
    pub private_ips: Vec<String>,
    pub sync_status: String,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateNetworkStatus {
    pub network: PrivateNetwork,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<PrivateNetworkEndpoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_hostname: Option<String>,
    pub state: PrivateNetworkState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PrivateNetworkState {
    Ready,
    Creating,
    Updating,
    Deleting,
    Initializing,
    Unknown,
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

    Ok(networks
        .iter()
        .filter(|network| network.deleted_at.is_none())
        .map(private_network_output)
        .collect())
}

pub async fn fetch_private_network_endpoint(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
    private_network_id: &str,
) -> Result<Option<PrivateNetworkEndpoint>> {
    let endpoint = post_graphql::<queries::PrivateNetworkEndpoint, _>(
        client,
        configs.get_backboard(),
        queries::private_network_endpoint::Variables {
            environment_id: environment_id.to_string(),
            service_id: service_id.to_string(),
            private_network_id: private_network_id.to_string(),
        },
    )
    .await?
    .private_network_endpoint;

    Ok(endpoint
        .filter(|endpoint| endpoint.deleted_at.is_none())
        .map(|endpoint| private_network_endpoint_output(&endpoint)))
}

pub async fn fetch_private_network_statuses(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
    network_selector: Option<&str>,
) -> Result<Vec<PrivateNetworkStatus>> {
    let networks = fetch_private_networks(client, configs, environment_id).await?;
    let selected = resolve_networks_for_status(&networks, network_selector)?;
    let mut statuses = Vec::with_capacity(selected.len());

    for network in selected {
        let endpoint = fetch_private_network_endpoint(
            client,
            configs,
            environment_id,
            service_id,
            &network.id,
        )
        .await?;
        statuses.push(private_network_status(network.clone(), endpoint));
    }

    Ok(statuses)
}

pub async fn update_private_network_endpoint_name(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
    network_selector: Option<&str>,
    name: &str,
) -> Result<PrivateNetworkStatus> {
    let networks = fetch_private_networks(client, configs, environment_id).await?;
    let network = resolve_network_for_update(&networks, network_selector)?.clone();
    let name = validate_endpoint_name(name, &endpoint_dns_suffix(&network))?;

    let endpoint = fetch_private_network_endpoint(
        client,
        configs,
        environment_id,
        service_id,
        &network.id,
    )
    .await?
    .ok_or_else(|| {
        anyhow!(
            "Private networking is initializing for this service. Deploy or restart the service, then try again."
        )
    })?;

    if endpoint.dns_name == name {
        return Ok(private_network_status(network, Some(endpoint)));
    }

    if endpoint.sync_status != "ACTIVE" {
        bail!(
            "Cannot update private network endpoint while sync status is {}.",
            endpoint.sync_status
        );
    }

    let available =
        endpoint_name_available(client, configs, environment_id, &network.id, &name).await?;
    if !available {
        bail!("Endpoint name already used: {name}");
    }

    let renamed = post_graphql::<mutations::PrivateNetworkEndpointRename, _>(
        client,
        configs.get_backboard(),
        mutations::private_network_endpoint_rename::Variables {
            id: endpoint.id.clone(),
            dns_name: name,
            private_network_id: network.id.clone(),
        },
    )
    .await?
    .private_network_endpoint_rename;

    if !renamed {
        bail!("Failed to update private network endpoint name.");
    }

    let endpoint =
        fetch_private_network_endpoint(client, configs, environment_id, service_id, &network.id)
            .await?
            .unwrap_or(endpoint);

    Ok(private_network_status(network, Some(endpoint)))
}

pub async fn endpoint_name_available(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    private_network_id: &str,
    name: &str,
) -> Result<bool> {
    Ok(
        post_graphql::<queries::PrivateNetworkEndpointNameAvailable, _>(
            client,
            configs.get_backboard(),
            queries::private_network_endpoint_name_available::Variables {
                environment_id: environment_id.to_string(),
                private_network_id: private_network_id.to_string(),
                prefix: name.to_string(),
            },
        )
        .await?
        .private_network_endpoint_name_available,
    )
}

pub fn resolve_networks_for_status<'a>(
    networks: &'a [PrivateNetwork],
    selector: Option<&str>,
) -> Result<Vec<&'a PrivateNetwork>> {
    if let Some(selector) = selector {
        return Ok(vec![resolve_network(networks, selector)?]);
    }

    Ok(networks.iter().collect())
}

pub fn resolve_network_for_update<'a>(
    networks: &'a [PrivateNetwork],
    selector: Option<&str>,
) -> Result<&'a PrivateNetwork> {
    if let Some(selector) = selector {
        return resolve_network(networks, selector);
    }

    match networks.len() {
        0 => bail!("No private networks found for this environment."),
        1 => Ok(&networks[0]),
        _ => {
            let railway_matches = networks
                .iter()
                .filter(|network| {
                    network
                        .name
                        .eq_ignore_ascii_case(DEFAULT_PRIVATE_NETWORK_NAME)
                })
                .collect::<Vec<_>>();

            match railway_matches.len() {
                1 => Ok(railway_matches[0]),
                0 => bail!(
                    "Multiple private networks found. Use --network to select one: {}",
                    format_network_choices(networks)
                ),
                _ => bail!(
                    "Multiple private networks named '{}'. Use --network with a private network ID: {}",
                    DEFAULT_PRIVATE_NETWORK_NAME,
                    format_network_choices(networks)
                ),
            }
        }
    }
}

pub fn resolve_network<'a>(
    networks: &'a [PrivateNetwork],
    selector: &str,
) -> Result<&'a PrivateNetwork> {
    if let Some(network) = networks
        .iter()
        .find(|network| network.id.eq_ignore_ascii_case(selector))
    {
        return Ok(network);
    }

    let matches = networks
        .iter()
        .filter(|network| {
            network.name.eq_ignore_ascii_case(selector)
                || network.dns_name.eq_ignore_ascii_case(selector)
        })
        .collect::<Vec<_>>();

    match matches.len() {
        0 => bail!("Private network '{selector}' not found."),
        1 => Ok(matches[0]),
        _ => bail!(
            "Private network selector '{}' matched multiple networks. Use a private network ID: {}",
            selector,
            format_network_choices(networks)
        ),
    }
}

pub fn validate_endpoint_name(name: &str, dns_suffix: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("Enter your endpoint name.");
    }

    let suffix = dns_suffix.trim_start_matches('.');
    if name.contains('.') || name.eq_ignore_ascii_case(suffix) {
        bail!("Endpoint name must be the short prefix, not a full .internal hostname.");
    }

    if name.starts_with('-') || name.ends_with('-') {
        bail!("Malformed endpoint name: cannot start or end with '-'.");
    }

    if name.len() > 63 {
        bail!("Malformed endpoint name: must be 63 characters or fewer.");
    }

    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        bail!("Malformed endpoint name.");
    }

    Ok(name.to_ascii_lowercase())
}

pub fn private_network_status(
    network: PrivateNetwork,
    endpoint: Option<PrivateNetworkEndpoint>,
) -> PrivateNetworkStatus {
    let state = endpoint
        .as_ref()
        .map(endpoint_state)
        .unwrap_or(PrivateNetworkState::Initializing);
    let hostname = endpoint
        .as_ref()
        .map(|endpoint| full_hostname(&endpoint.dns_name, &network));
    let short_name = endpoint.as_ref().map(|endpoint| endpoint.dns_name.clone());
    let pending_hostname = endpoint
        .as_ref()
        .and_then(|endpoint| endpoint.new_dns_name.as_ref())
        .map(|name| full_hostname(name, &network));

    PrivateNetworkStatus {
        network,
        endpoint,
        full_hostname: hostname,
        short_name,
        pending_hostname,
        state,
    }
}

pub fn full_hostname(endpoint_name: &str, network: &PrivateNetwork) -> String {
    format!("{}.{}", endpoint_name, endpoint_dns_suffix(network))
}

pub fn endpoint_dns_suffix(network: &PrivateNetwork) -> String {
    format!("{}.internal", network.dns_name)
}

fn private_network_output(
    network: &queries::private_networks::PrivateNetworksPrivateNetworks,
) -> PrivateNetwork {
    PrivateNetwork {
        id: network.public_id.clone(),
        project_id: network.project_id.clone(),
        environment_id: network.environment_id.clone(),
        name: network.name.clone(),
        dns_name: network.dns_name.clone(),
        ip_family: ip_family_label(&network.tags),
        network_id: network.network_id,
        tags: network.tags.clone(),
        created_at: network
            .created_at
            .as_ref()
            .map(chrono::DateTime::to_rfc3339),
    }
}

fn private_network_endpoint_output(
    endpoint: &queries::private_network_endpoint::PrivateNetworkEndpointPrivateNetworkEndpoint,
) -> PrivateNetworkEndpoint {
    PrivateNetworkEndpoint {
        id: endpoint.public_id.clone(),
        service_instance_id: endpoint.service_instance_id.clone(),
        dns_name: endpoint.dns_name.clone(),
        new_dns_name: endpoint.new_dns_name.clone(),
        private_ips: endpoint.private_ips.clone(),
        sync_status: private_network_endpoint_sync_status(&endpoint.sync_status),
        tags: endpoint.tags.clone(),
        created_at: endpoint
            .created_at
            .as_ref()
            .map(chrono::DateTime::to_rfc3339),
    }
}

fn ip_family_label(tags: &[String]) -> String {
    if tags.iter().any(|tag| tag == IPV4_PRIVATE_NETWORK_TAG) {
        "IPv4 & IPv6".to_string()
    } else {
        "IPv6".to_string()
    }
}

fn endpoint_state(endpoint: &PrivateNetworkEndpoint) -> PrivateNetworkState {
    match endpoint.sync_status.as_str() {
        "ACTIVE" => PrivateNetworkState::Ready,
        "CREATING" => PrivateNetworkState::Creating,
        "UPDATING" => PrivateNetworkState::Updating,
        "DELETING" | "DELETED" => PrivateNetworkState::Deleting,
        _ => PrivateNetworkState::Unknown,
    }
}

fn private_network_endpoint_sync_status(
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

fn format_network_choices(networks: &[PrivateNetwork]) -> String {
    networks
        .iter()
        .map(|network| {
            format!(
                "{} (id: {}, dns: {})",
                network.name, network.id, network.dns_name
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(id: &str, name: &str, dns_name: &str, tags: Vec<&str>) -> PrivateNetwork {
        PrivateNetwork {
            id: id.to_string(),
            project_id: "project".to_string(),
            environment_id: "environment".to_string(),
            name: name.to_string(),
            dns_name: dns_name.to_string(),
            ip_family: ip_family_label(
                &tags
                    .into_iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>(),
            ),
            network_id: 1,
            tags: vec![],
            created_at: None,
        }
    }

    #[test]
    fn resolves_network_for_update() {
        let networks = vec![network("pn_1", "railway", "railway", vec![])];
        assert_eq!(
            resolve_network_for_update(&networks, None).unwrap().id,
            "pn_1"
        );

        let networks = vec![
            network("pn_1", "custom", "custom", vec![]),
            network("pn_2", "railway", "railway", vec![]),
        ];
        assert_eq!(
            resolve_network_for_update(&networks, None).unwrap().id,
            "pn_2"
        );
        assert_eq!(
            resolve_network_for_update(&networks, Some("custom"))
                .unwrap()
                .id,
            "pn_1"
        );
        assert_eq!(
            resolve_network_for_update(&networks, Some("pn_2"))
                .unwrap()
                .id,
            "pn_2"
        );
        assert_eq!(
            resolve_network_for_update(&networks, Some("railway"))
                .unwrap()
                .id,
            "pn_2"
        );
    }

    #[test]
    fn update_errors_when_multiple_networks_have_no_default() {
        let networks = vec![
            network("pn_1", "alpha", "alpha", vec![]),
            network("pn_2", "beta", "beta", vec![]),
        ];

        let err = resolve_network_for_update(&networks, None).unwrap_err();
        assert!(err.to_string().contains("Multiple private networks found"));
        assert!(err.to_string().contains("--network"));
    }

    #[test]
    fn status_selects_all_networks_without_selector() {
        let networks = vec![
            network("pn_1", "alpha", "alpha", vec![]),
            network("pn_2", "beta", "beta", vec![]),
        ];
        assert_eq!(
            resolve_networks_for_status(&networks, None).unwrap().len(),
            2
        );
        assert!(resolve_networks_for_status(&networks, Some("beta")).is_ok());
    }

    #[test]
    fn validates_endpoint_name() {
        assert_eq!(
            validate_endpoint_name("api-1", "railway.internal").unwrap(),
            "api-1"
        );
        assert_eq!(
            validate_endpoint_name("API", "railway.internal").unwrap(),
            "api"
        );
        assert!(validate_endpoint_name("", "railway.internal").is_err());
        assert!(validate_endpoint_name("-api", "railway.internal").is_err());
        assert!(validate_endpoint_name("api-", "railway.internal").is_err());
        assert!(validate_endpoint_name("api.railway.internal", "railway.internal").is_err());
        assert!(validate_endpoint_name("api.example", "railway.internal").is_err());
        assert!(validate_endpoint_name("api_name", "railway.internal").is_err());
    }

    #[test]
    fn shapes_status_data() {
        let network = network("pn_1", "railway", "railway", vec![IPV4_PRIVATE_NETWORK_TAG]);
        let endpoint = PrivateNetworkEndpoint {
            id: "pne_1".to_string(),
            service_instance_id: "si_1".to_string(),
            dns_name: "api".to_string(),
            new_dns_name: None,
            private_ips: vec![],
            sync_status: "ACTIVE".to_string(),
            tags: vec![],
            created_at: None,
        };

        let status = private_network_status(network, Some(endpoint));

        assert_eq!(
            status.full_hostname.as_deref(),
            Some("api.railway.internal")
        );
        assert_eq!(status.short_name.as_deref(), Some("api"));
        assert_eq!(status.network.ip_family, "IPv4 & IPv6");
        assert_eq!(status.state, PrivateNetworkState::Ready);
    }

    #[test]
    fn missing_endpoint_is_initializing() {
        let status = private_network_status(network("pn_1", "railway", "railway", vec![]), None);

        assert_eq!(status.state, PrivateNetworkState::Initializing);
        assert!(status.full_hostname.is_none());
    }
}
