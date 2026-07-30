use std::collections::BTreeMap;

use anyhow::Result;
use reqwest::Client;
use serde::Serialize;

use crate::{
    client::{post_graphql, post_graphql_raw},
    config::Configs,
    controllers::config::{DeployConfig, EnvironmentConfig, ServiceInstance},
    gql::{mutations, queries},
};

const ENVIRONMENT_STAGE_CHANGES_MUTATION: &str =
    include_str!("../gql/mutations/strings/EnvironmentStageChanges.graphql");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LifecycleMode {
    Direct,
    EnvironmentPatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lifecycle {
    pub mode: LifecycleMode,
    pub staged: bool,
    pub committed: bool,
    pub redeploy_required: bool,
    pub redeploy_triggered: bool,
}

impl Lifecycle {
    pub fn direct(changed: bool) -> Self {
        Self {
            mode: LifecycleMode::Direct,
            staged: false,
            committed: changed,
            redeploy_required: changed,
            redeploy_triggered: false,
        }
    }

    pub fn environment_patch(staged: bool) -> Self {
        Self {
            mode: LifecycleMode::EnvironmentPatch,
            staged,
            committed: false,
            redeploy_required: false,
            redeploy_triggered: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticIpAddress {
    pub ipv4: String,
    pub region: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticIpStatus {
    pub enabled: bool,
    pub high_availability: bool,
    pub ips: Vec<StaticIpAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ipv6Status {
    pub enabled: bool,
    pub staged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_value: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Ipv6State {
    status: Ipv6Status,
    staged_value: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureAction {
    pub feature: &'static str,
    pub action: &'static str,
    pub enabled: bool,
    pub changed: bool,
    pub lifecycle: Lifecycle,
}

pub async fn fetch_static_ip_status(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
) -> Result<StaticIpStatus> {
    let gateways = post_graphql::<queries::EgressGateways, _>(
        client,
        configs.get_backboard(),
        queries::egress_gateways::Variables {
            environment_id: environment_id.to_string(),
            service_id: service_id.to_string(),
        },
    )
    .await?
    .egress_gateways;

    let ips = gateways
        .into_iter()
        .map(|gateway| static_ip_address(gateway.ipv4, gateway.region, gateway.zone))
        .collect();

    Ok(static_ip_status_from_addresses(ips))
}

pub async fn enable_static_ips(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
) -> Result<(StaticIpStatus, FeatureAction)> {
    let current = fetch_static_ip_status(client, configs, environment_id, service_id).await?;
    if current.enabled {
        return Ok((
            current,
            FeatureAction {
                feature: "staticIp",
                action: "enable",
                enabled: true,
                changed: false,
                lifecycle: Lifecycle::direct(false),
            },
        ));
    }

    let gateways = post_graphql::<mutations::EgressGatewayAssociationCreate, _>(
        client,
        configs.get_backboard(),
        mutations::egress_gateway_association_create::Variables {
            input: mutations::egress_gateway_association_create::EgressGatewayCreateInput {
                environment_id: environment_id.to_string(),
                service_id: service_id.to_string(),
                region: None,
            },
        },
    )
    .await?
    .egress_gateway_association_create;

    let ips = gateways
        .into_iter()
        .map(|gateway| static_ip_address(gateway.ipv4, gateway.region, gateway.zone))
        .collect();
    let status = static_ip_status_from_addresses(ips);
    Ok((
        status,
        FeatureAction {
            feature: "staticIp",
            action: "enable",
            enabled: true,
            changed: true,
            lifecycle: Lifecycle::direct(true),
        },
    ))
}

pub async fn disable_static_ips(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
) -> Result<(StaticIpStatus, FeatureAction)> {
    let current = fetch_static_ip_status(client, configs, environment_id, service_id).await?;
    if !current.enabled {
        return Ok((
            current,
            FeatureAction {
                feature: "staticIp",
                action: "disable",
                enabled: false,
                changed: false,
                lifecycle: Lifecycle::direct(false),
            },
        ));
    }

    post_graphql::<mutations::EgressGatewayAssociationsClear, _>(
        client,
        configs.get_backboard(),
        mutations::egress_gateway_associations_clear::Variables {
            input: mutations::egress_gateway_associations_clear::EgressGatewayServiceTargetInput {
                environment_id: environment_id.to_string(),
                service_id: service_id.to_string(),
                all_environments: None,
            },
        },
    )
    .await?;

    Ok((
        StaticIpStatus {
            enabled: false,
            high_availability: false,
            ips: Vec::new(),
        },
        FeatureAction {
            feature: "staticIp",
            action: "disable",
            enabled: false,
            changed: true,
            lifecycle: Lifecycle::direct(true),
        },
    ))
}

pub async fn fetch_ipv6_status(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
) -> Result<Ipv6Status> {
    Ok(
        fetch_ipv6_state(client, configs, environment_id, service_id)
            .await?
            .status,
    )
}

pub async fn stage_ipv6(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
    value: bool,
) -> Result<(Ipv6Status, FeatureAction)> {
    let current = fetch_ipv6_state(client, configs, environment_id, service_id).await?;

    if current.staged_value.is_some() && current.status.enabled == value {
        clear_staged_ipv6_value(client, configs, environment_id, service_id).await?;
        let updated = fetch_ipv6_state(client, configs, environment_id, service_id)
            .await?
            .status;
        return Ok((
            updated.clone(),
            ipv6_feature_action(value, true, updated.staged),
        ));
    }

    if current.staged_value == Some(value)
        || (current.staged_value.is_none() && current.status.enabled == value)
    {
        let status = current.status;
        let staged = status.staged;
        return Ok((status, ipv6_feature_action(value, false, staged)));
    }

    let patch = ipv6_patch(service_id, value);
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
    let updated = fetch_ipv6_status(client, configs, environment_id, service_id).await?;

    Ok((
        updated.clone(),
        ipv6_feature_action(value, true, updated.staged),
    ))
}

fn ipv6_feature_action(value: bool, changed: bool, staged: bool) -> FeatureAction {
    FeatureAction {
        feature: "ipv6",
        action: if value { "enable" } else { "disable" },
        enabled: value,
        changed,
        lifecycle: Lifecycle::environment_patch(staged),
    }
}

fn static_ip_address(ipv4: String, region: String, zone: Option<String>) -> StaticIpAddress {
    StaticIpAddress { ipv4, region, zone }
}

fn static_ip_status_from_addresses(ips: Vec<StaticIpAddress>) -> StaticIpStatus {
    let high_availability = ips.iter().any(|ip| ip.zone.is_some());

    StaticIpStatus {
        enabled: !ips.is_empty(),
        high_availability,
        ips,
    }
}

async fn fetch_ipv6_state(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
) -> Result<Ipv6State> {
    let (config, staged_value) = tokio::try_join!(
        crate::controllers::config::environment::fetch_environment_config(
            client,
            configs,
            environment_id,
            false,
        ),
        fetch_staged_ipv6_value(client, configs, environment_id, service_id)
    )?;

    Ok(Ipv6State {
        status: ipv6_status_from_config_and_staged_value(&config.config, service_id, staged_value),
        staged_value,
    })
}

fn ipv6_status_from_config_and_staged_value(
    config: &EnvironmentConfig,
    service_id: &str,
    staged_value: Option<bool>,
) -> Ipv6Status {
    let enabled = ipv6_enabled_from_config(config, service_id);
    let pending_value = staged_value.filter(|value| *value != enabled);

    Ipv6Status {
        enabled,
        staged: pending_value.is_some(),
        pending_value,
    }
}

pub fn ipv6_patch(service_id: &str, value: bool) -> EnvironmentConfig {
    EnvironmentConfig {
        services: BTreeMap::from([(
            service_id.to_string(),
            ServiceInstance {
                deploy: Some(DeployConfig {
                    ipv6_egress_enabled: Some(value),
                    ..DeployConfig::default()
                }),
                ..ServiceInstance::default()
            },
        )]),
        ..EnvironmentConfig::default()
    }
}

pub fn ipv6_clear_patch(service_id: &str) -> serde_json::Value {
    serde_json::json!({
        "services": {
            service_id: {
                "deploy": {
                    "ipv6EgressEnabled": null
                }
            }
        }
    })
}

pub fn ipv6_enabled_from_config(config: &EnvironmentConfig, service_id: &str) -> bool {
    config
        .services
        .get(service_id)
        .and_then(|service| service.deploy.as_ref())
        .and_then(|deploy| deploy.ipv6_egress_enabled)
        .unwrap_or(false)
}

pub fn staged_ipv6_value_from_patch(
    patch: &serde_json::Value,
    service_id: &str,
) -> Result<Option<bool>> {
    let config: EnvironmentConfig = serde_json::from_value(patch.clone())?;
    Ok(config
        .services
        .get(service_id)
        .and_then(|service| service.deploy.as_ref())
        .and_then(|deploy| deploy.ipv6_egress_enabled))
}

async fn fetch_staged_ipv6_value(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
) -> Result<Option<bool>> {
    let response = post_graphql::<queries::EnvironmentStagedChanges, _>(
        client,
        configs.get_backboard(),
        queries::environment_staged_changes::Variables {
            environment_id: environment_id.to_string(),
            decrypt_variables: Some(true),
        },
    )
    .await?;

    staged_ipv6_value_from_patch(&response.environment_staged_changes.patch, service_id)
}

async fn clear_staged_ipv6_value(
    client: &Client,
    configs: &Configs,
    environment_id: &str,
    service_id: &str,
) -> Result<()> {
    post_graphql_raw::<mutations::environment_stage_changes::ResponseData, _>(
        client,
        configs.get_backboard(),
        ENVIRONMENT_STAGE_CHANGES_MUTATION,
        serde_json::json!({
            "environmentId": environment_id,
            "input": ipv6_clear_patch(service_id),
            "merge": true,
        }),
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_ip_status_derives_enabled_and_high_availability() {
        let disabled = static_ip_status_from_addresses(vec![]);
        assert!(!disabled.enabled);
        assert!(!disabled.high_availability);

        let single_gateway = static_ip_status_from_addresses(vec![StaticIpAddress {
            ipv4: "203.0.113.10".to_string(),
            region: "us-west2".to_string(),
            zone: None,
        }]);
        assert!(single_gateway.enabled);
        assert!(!single_gateway.high_availability);
        assert!(
            serde_json::to_value(&single_gateway.ips[0])
                .unwrap()
                .get("type")
                .is_none()
        );
        assert!(
            serde_json::to_value(&single_gateway)
                .unwrap()
                .get("mode")
                .is_none()
        );

        let ha = static_ip_status_from_addresses(vec![StaticIpAddress {
            ipv4: "203.0.113.10".to_string(),
            region: "us-west2".to_string(),
            zone: Some("zone-a".to_string()),
        }]);
        assert!(ha.enabled);
        assert!(ha.high_availability);
    }

    #[test]
    fn ipv6_patch_serializes_to_deploy_config() {
        let value = serde_json::to_value(ipv6_patch("svc_123", true)).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "services": {
                    "svc_123": {
                        "deploy": {
                            "ipv6EgressEnabled": true
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn ipv6_clear_patch_serializes_null_value() {
        assert_eq!(
            ipv6_clear_patch("svc_123"),
            serde_json::json!({
                "services": {
                    "svc_123": {
                        "deploy": {
                            "ipv6EgressEnabled": null
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn ipv6_status_reads_current_and_staged_values() {
        let current = EnvironmentConfig {
            services: BTreeMap::from([(
                "svc_123".to_string(),
                ServiceInstance {
                    deploy: Some(DeployConfig {
                        ipv6_egress_enabled: Some(true),
                        ..DeployConfig::default()
                    }),
                    ..ServiceInstance::default()
                },
            )]),
            ..EnvironmentConfig::default()
        };
        assert!(ipv6_enabled_from_config(&current, "svc_123"));
        assert!(!ipv6_enabled_from_config(&current, "svc_456"));

        let staged = serde_json::json!({
            "services": {
                "svc_123": {
                    "deploy": {
                        "ipv6EgressEnabled": false
                    }
                }
            }
        });
        assert_eq!(
            staged_ipv6_value_from_patch(&staged, "svc_123").unwrap(),
            Some(false)
        );
        assert_eq!(
            staged_ipv6_value_from_patch(&staged, "svc_456").unwrap(),
            None
        );

        let status = ipv6_status_from_config_and_staged_value(&current, "svc_123", Some(false));
        assert!(status.enabled);
        assert!(status.staged);
        assert_eq!(status.pending_value, Some(false));
    }

    #[test]
    fn ipv6_status_ignores_redundant_staged_value() {
        let current = EnvironmentConfig {
            services: BTreeMap::from([(
                "svc_123".to_string(),
                ServiceInstance {
                    deploy: Some(DeployConfig {
                        ipv6_egress_enabled: Some(true),
                        ..DeployConfig::default()
                    }),
                    ..ServiceInstance::default()
                },
            )]),
            ..EnvironmentConfig::default()
        };

        let status = ipv6_status_from_config_and_staged_value(&current, "svc_123", Some(true));
        assert!(status.enabled);
        assert!(!status.staged);
        assert_eq!(status.pending_value, None);
    }

    #[test]
    fn unchanged_ipv6_action_can_still_report_staged_patch() {
        let action = ipv6_feature_action(true, false, true);

        assert_eq!(action.feature, "ipv6");
        assert_eq!(action.action, "enable");
        assert!(action.enabled);
        assert!(!action.changed);
        assert!(action.lifecycle.staged);
        assert!(!action.lifecycle.committed);
        assert!(!action.lifecycle.redeploy_triggered);
    }
}
