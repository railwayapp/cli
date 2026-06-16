use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use reqwest::Client;
use serde::Serialize;

use crate::{
    client::post_graphql,
    config::Configs,
    controllers::config::{EnvironmentConfig, ServiceInstance, ServiceNetworking, TcpProxyConfig},
    gql::{mutations, queries},
};

const VERIFY_ATTEMPTS: usize = 8;
const VERIFY_DELAY_MS: u64 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PatchMode {
    Commit,
    Stage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TcpProxy {
    pub id: String,
    pub domain: String,
    pub proxy_port: i64,
    pub application_port: i64,
    pub endpoint: String,
    pub sync_status: String,
    #[serde(skip_serializing)]
    pub service_id: String,
    #[serde(skip_serializing)]
    pub environment_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedProxyIdentifier {
    pub host: String,
    pub port: Option<i64>,
}

pub fn parse_port(value: &str) -> std::result::Result<u16, String> {
    let port = value
        .parse::<u16>()
        .map_err(|_| "port must be a number from 1 to 65535".to_string())?;

    if port == 0 {
        return Err("port must be a number from 1 to 65535".to_string());
    }

    Ok(port)
}

pub fn validate_application_port(port: i64) -> std::result::Result<u16, String> {
    if !(1..=65535).contains(&port) {
        return Err("application_port must be a number from 1 to 65535".to_string());
    }

    Ok(port as u16)
}

pub async fn fetch_tcp_proxies(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
) -> Result<Vec<TcpProxy>> {
    let proxies = post_graphql::<queries::TcpProxies, _>(
        client,
        configs.get_backboard(),
        queries::tcp_proxies::Variables {
            environment_id: environment_id.to_string(),
            service_id: service_id.to_string(),
        },
    )
    .await?
    .tcp_proxies;

    Ok(active_tcp_proxies(&proxies))
}

pub fn active_tcp_proxies(proxies: &[queries::tcp_proxies::TcpProxiesTcpProxies]) -> Vec<TcpProxy> {
    proxies
        .iter()
        .filter(|proxy| is_live_tcp_proxy(proxy))
        .map(tcp_proxy_output)
        .collect()
}

fn is_live_tcp_proxy(proxy: &queries::tcp_proxies::TcpProxiesTcpProxies) -> bool {
    proxy.deleted_at.is_none()
        && !matches!(
            proxy.sync_status,
            queries::tcp_proxies::TCPProxySyncStatus::DELETED
                | queries::tcp_proxies::TCPProxySyncStatus::DELETING
        )
}

pub fn tcp_proxy_output(proxy: &queries::tcp_proxies::TcpProxiesTcpProxies) -> TcpProxy {
    TcpProxy {
        id: proxy.id.clone(),
        domain: proxy.domain.clone(),
        proxy_port: proxy.proxy_port,
        application_port: proxy.application_port,
        endpoint: format!("{}:{}", proxy.domain, proxy.proxy_port),
        sync_status: tcp_proxy_sync_status(&proxy.sync_status),
        service_id: proxy.service_id.clone(),
        environment_id: proxy.environment_id.clone(),
        created_at: proxy.created_at.as_ref().map(chrono::DateTime::to_rfc3339),
        updated_at: proxy.updated_at.as_ref().map(chrono::DateTime::to_rfc3339),
    }
}

pub fn existing_proxy_for_create(proxies: &[TcpProxy], port: u16) -> Result<Option<&TcpProxy>> {
    if let Some(proxy) = proxies
        .iter()
        .find(|proxy| proxy.application_port == i64::from(port))
    {
        return Ok(Some(proxy));
    }

    let Some(proxy) = proxies.first() else {
        return Ok(None);
    };

    bail!(
        "A TCP proxy already exists for application port {} ({}). Only one TCP proxy is allowed per service instance. Delete it before creating a TCP proxy for application port {}.",
        proxy.application_port,
        proxy.endpoint,
        port
    );
}

pub async fn resolve_tcp_proxy(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
    identifier: &str,
) -> Result<TcpProxy> {
    let proxies = fetch_tcp_proxies(client, configs, environment_id, service_id).await?;

    find_tcp_proxy(&proxies, identifier)?
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "TCP proxy '{}' not found on the selected service",
                identifier
            )
        })
}

pub fn find_tcp_proxy<'a>(
    proxies: &'a [TcpProxy],
    identifier: &str,
) -> Result<Option<&'a TcpProxy>> {
    if let Some(proxy) = proxies
        .iter()
        .find(|proxy| proxy.id.eq_ignore_ascii_case(identifier))
    {
        return Ok(Some(proxy));
    }

    let normalized = normalize_proxy_identifier(identifier);
    let matches = matching_proxies(proxies, &normalized);

    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches[0])),
        _ => bail!(
            "TCP proxy selector '{}' matched multiple proxies: {}. Use a TCP proxy ID or full endpoint.",
            identifier,
            format_proxy_choices(&matches)
        ),
    }
}

pub fn normalize_proxy_identifier(identifier: &str) -> NormalizedProxyIdentifier {
    let trimmed = identifier.trim();
    let without_scheme = trimmed
        .strip_prefix("tcp://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let endpoint = without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .trim_end_matches('.');

    if let Some((host, port)) = endpoint.rsplit_once(':') {
        if let Ok(port) = port.parse::<i64>() {
            return NormalizedProxyIdentifier {
                host: host.trim_end_matches('.').to_string(),
                port: Some(port),
            };
        }
    }

    NormalizedProxyIdentifier {
        host: endpoint.to_string(),
        port: None,
    }
}

pub async fn apply_tcp_proxy_patch(
    client: &Client,
    configs: &Configs,
    project: &queries::RailwayProject,
    environment_id: &str,
    service_id: &str,
    service_name: &str,
    port: u16,
) -> Result<PatchMode> {
    let patch = tcp_proxy_patch(service_id, port);
    let mode = patch_mode(project, environment_id);

    match mode {
        PatchMode::Commit => {
            post_graphql::<mutations::EnvironmentPatchCommit, _>(
                client,
                configs.get_backboard(),
                mutations::environment_patch_commit::Variables {
                    environment_id: environment_id.to_string(),
                    patch,
                    commit_message: Some(format!(
                        "Create TCP proxy for {} on port {}",
                        service_name, port
                    )),
                },
            )
            .await?;
        }
        PatchMode::Stage => {
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
        }
    }

    Ok(mode)
}

pub fn tcp_proxy_patch(service_id: &str, port: u16) -> EnvironmentConfig {
    EnvironmentConfig {
        services: BTreeMap::from([(
            service_id.to_string(),
            ServiceInstance {
                networking: Some(ServiceNetworking {
                    tcp_proxies: BTreeMap::from([(
                        port.to_string(),
                        Some(TcpProxyConfig::default()),
                    )]),
                    ..ServiceNetworking::default()
                }),
                ..ServiceInstance::default()
            },
        )]),
        ..EnvironmentConfig::default()
    }
}

pub fn patch_mode(project: &queries::RailwayProject, environment_id: &str) -> PatchMode {
    let unmerged_changes = project
        .environments
        .edges
        .iter()
        .find(|env| env.node.id == environment_id)
        .and_then(|env| env.node.unmerged_changes_count)
        .unwrap_or_default();

    if unmerged_changes > 0 {
        PatchMode::Stage
    } else {
        PatchMode::Commit
    }
}

pub async fn verify_tcp_proxy_configured(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
    port: u16,
) -> Result<()> {
    for attempt in 0..VERIFY_ATTEMPTS {
        let proxies = fetch_tcp_proxies(client, configs, environment_id, service_id).await?;
        if proxies
            .iter()
            .any(|proxy| proxy.application_port == i64::from(port))
        {
            return Ok(());
        }

        if attempt + 1 < VERIFY_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(VERIFY_DELAY_MS)).await;
        }
    }

    bail!(
        "TCP proxy configuration was requested, but port {} was not present after verification.",
        port
    );
}

pub async fn delete_tcp_proxy(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
    proxy: &TcpProxy,
) -> Result<()> {
    let deleted = post_graphql::<mutations::TcpProxyDelete, _>(
        client,
        configs.get_backboard(),
        mutations::tcp_proxy_delete::Variables {
            id: proxy.id.clone(),
        },
    )
    .await?
    .tcp_proxy_delete;

    if !deleted {
        bail!("Failed to delete TCP proxy {}", proxy.id);
    }

    let remaining = fetch_tcp_proxies(client, configs, environment_id, service_id).await?;
    if remaining.iter().any(|item| item.id == proxy.id) {
        bail!(
            "TCP proxy deletion was requested, but {} still exists after verification.",
            proxy.id
        );
    }

    Ok(())
}

pub fn tcp_proxy_sync_status(value: &queries::tcp_proxies::TCPProxySyncStatus) -> String {
    match value {
        queries::tcp_proxies::TCPProxySyncStatus::ACTIVE => "ACTIVE".to_string(),
        queries::tcp_proxies::TCPProxySyncStatus::CREATING => "CREATING".to_string(),
        queries::tcp_proxies::TCPProxySyncStatus::DELETED => "DELETED".to_string(),
        queries::tcp_proxies::TCPProxySyncStatus::DELETING => "DELETING".to_string(),
        queries::tcp_proxies::TCPProxySyncStatus::UNSPECIFIED => "UNSPECIFIED".to_string(),
        queries::tcp_proxies::TCPProxySyncStatus::UPDATING => "UPDATING".to_string(),
        queries::tcp_proxies::TCPProxySyncStatus::Other(status) => status.clone(),
    }
}

fn matching_proxies<'a>(
    proxies: &'a [TcpProxy],
    normalized: &NormalizedProxyIdentifier,
) -> Vec<&'a TcpProxy> {
    if let Some(port) = normalized.port {
        return proxies
            .iter()
            .filter(|proxy| {
                proxy.domain.eq_ignore_ascii_case(&normalized.host) && proxy.proxy_port == port
            })
            .collect();
    }

    if let Ok(numeric) = normalized.host.parse::<i64>() {
        return proxies
            .iter()
            .filter(|proxy| proxy.proxy_port == numeric || proxy.application_port == numeric)
            .collect();
    }

    proxies
        .iter()
        .filter(|proxy| proxy.domain.eq_ignore_ascii_case(&normalized.host))
        .collect()
}

fn format_proxy_choices(proxies: &[&TcpProxy]) -> String {
    proxies
        .iter()
        .map(|proxy| {
            format!(
                "{} (id: {}, app port: {})",
                proxy.endpoint, proxy.id, proxy.application_port
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_proxy() -> TcpProxy {
        TcpProxy {
            id: "tcp_123".to_string(),
            domain: "containers-us-west.railway.app".to_string(),
            proxy_port: 15432,
            application_port: 5432,
            endpoint: "containers-us-west.railway.app:15432".to_string(),
            sync_status: "ACTIVE".to_string(),
            service_id: "svc_123".to_string(),
            environment_id: "env_123".to_string(),
            created_at: None,
            updated_at: None,
        }
    }

    fn sample_proxy_with_ports(id: &str, proxy_port: i64, application_port: i64) -> TcpProxy {
        TcpProxy {
            id: id.to_string(),
            domain: "containers-us-west.railway.app".to_string(),
            proxy_port,
            application_port,
            endpoint: format!("containers-us-west.railway.app:{proxy_port}"),
            sync_status: "ACTIVE".to_string(),
            service_id: "svc_123".to_string(),
            environment_id: "env_123".to_string(),
            created_at: None,
            updated_at: None,
        }
    }

    fn sample_raw_proxy(
        id: &str,
        sync_status: queries::tcp_proxies::TCPProxySyncStatus,
    ) -> queries::tcp_proxies::TcpProxiesTcpProxies {
        queries::tcp_proxies::TcpProxiesTcpProxies {
            id: id.to_string(),
            domain: "containers-us-west.railway.app".to_string(),
            proxy_port: 15432,
            application_port: 5432,
            service_id: "svc_123".to_string(),
            environment_id: "env_123".to_string(),
            sync_status,
            created_at: None,
            updated_at: None,
            deleted_at: None,
        }
    }

    #[test]
    fn validates_port_range() {
        assert_eq!(parse_port("1").unwrap(), 1);
        assert_eq!(parse_port("65535").unwrap(), 65535);
        assert_eq!(validate_application_port(1).unwrap(), 1);
        assert_eq!(validate_application_port(65535).unwrap(), 65535);
        assert!(parse_port("0").is_err());
        assert!(parse_port("65536").is_err());
        assert!(validate_application_port(0).is_err());
        assert!(validate_application_port(65536).is_err());
    }

    #[test]
    fn normalizes_proxy_identifier() {
        assert_eq!(
            normalize_proxy_identifier("tcp://containers-us-west.railway.app:15432/path"),
            NormalizedProxyIdentifier {
                host: "containers-us-west.railway.app".to_string(),
                port: Some(15432),
            }
        );
        assert_eq!(
            normalize_proxy_identifier("containers-us-west.railway.app."),
            NormalizedProxyIdentifier {
                host: "containers-us-west.railway.app".to_string(),
                port: None,
            }
        );
    }

    #[test]
    fn finds_proxy_by_id_endpoint_domain_or_port() {
        let proxy = sample_proxy();
        let proxies = vec![proxy];

        assert!(find_tcp_proxy(&proxies, "tcp_123").unwrap().is_some());
        assert!(
            find_tcp_proxy(&proxies, "containers-us-west.railway.app:15432")
                .unwrap()
                .is_some()
        );
        assert!(
            find_tcp_proxy(&proxies, "containers-us-west.railway.app")
                .unwrap()
                .is_some()
        );
        assert!(find_tcp_proxy(&proxies, "15432").unwrap().is_some());
        assert!(find_tcp_proxy(&proxies, "5432").unwrap().is_some());
        assert!(find_tcp_proxy(&proxies, "missing").unwrap().is_none());
    }

    #[test]
    fn create_allows_existing_proxy_only_for_same_application_port() {
        let proxy = sample_proxy();
        let proxies = vec![proxy];

        let existing = existing_proxy_for_create(&proxies, 5432).unwrap().unwrap();
        assert_eq!(existing.id, "tcp_123");

        let err = existing_proxy_for_create(&proxies, 6380).unwrap_err();
        assert!(err.to_string().contains("Only one TCP proxy is allowed"));
        assert!(err.to_string().contains("application port 5432"));
        assert!(err.to_string().contains("application port 6380"));
    }

    #[test]
    fn create_idempotency_is_not_dependent_on_proxy_order() {
        let proxies = vec![
            sample_proxy_with_ports("tcp_6379", 16379, 6379),
            sample_proxy_with_ports("tcp_5432", 15432, 5432),
        ];

        let existing = existing_proxy_for_create(&proxies, 5432).unwrap().unwrap();

        assert_eq!(existing.id, "tcp_5432");
    }

    #[test]
    fn id_lookup_wins_even_when_numeric_selector_would_be_ambiguous() {
        let proxies = vec![
            sample_proxy_with_ports("15432", 15432, 5432),
            sample_proxy_with_ports("tcp_456", 15433, 15432),
        ];

        let proxy = find_tcp_proxy(&proxies, "15432").unwrap().unwrap();

        assert_eq!(proxy.id, "15432");
    }

    #[test]
    fn ambiguous_domain_or_port_selector_errors() {
        let proxies = vec![
            sample_proxy_with_ports("tcp_123", 15432, 5432),
            sample_proxy_with_ports("tcp_456", 15433, 5432),
        ];

        let domain_err = find_tcp_proxy(&proxies, "containers-us-west.railway.app").unwrap_err();
        assert!(domain_err.to_string().contains("matched multiple proxies"));

        let port_err = find_tcp_proxy(&proxies, "5432").unwrap_err();
        assert!(
            port_err
                .to_string()
                .contains("Use a TCP proxy ID or full endpoint")
        );

        assert!(
            find_tcp_proxy(&proxies, "containers-us-west.railway.app:15433")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn active_tcp_proxies_ignores_deleted_and_deleting_sync_status() {
        let proxies = vec![
            sample_raw_proxy(
                "tcp_active",
                queries::tcp_proxies::TCPProxySyncStatus::ACTIVE,
            ),
            sample_raw_proxy(
                "tcp_deleting",
                queries::tcp_proxies::TCPProxySyncStatus::DELETING,
            ),
            sample_raw_proxy(
                "tcp_deleted",
                queries::tcp_proxies::TCPProxySyncStatus::DELETED,
            ),
        ];

        let active = active_tcp_proxies(&proxies);

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "tcp_active");
    }

    #[test]
    fn tcp_proxy_patch_sets_networking_config_for_service_port() {
        let patch = tcp_proxy_patch("svc_123", 6379);
        let networking = patch
            .services
            .get("svc_123")
            .and_then(|service| service.networking.as_ref())
            .unwrap();

        assert!(networking.tcp_proxies.contains_key("6379"));
    }

    #[test]
    fn tcp_proxy_sync_status_outputs_clean_strings() {
        assert_eq!(
            tcp_proxy_sync_status(&queries::tcp_proxies::TCPProxySyncStatus::ACTIVE),
            "ACTIVE"
        );
        assert_eq!(
            tcp_proxy_sync_status(&queries::tcp_proxies::TCPProxySyncStatus::Other(
                "NEW_STATUS".to_string()
            )),
            "NEW_STATUS"
        );
    }
}
