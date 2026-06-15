use rmcp::{ErrorData as McpError, model::*};

use crate::controllers::private_network::{
    self, PatchMode, PrivateNetwork, PrivateNetworkEndpoint,
};

use super::super::handler::{RailwayMcp, ResolvedServiceContext};
use super::super::params::{
    CheckPrivateNetworkEndpointNameParams, EnablePrivateNetworkParams, EnvironmentParams,
    PrivateNetworkStatusParams, SetPrivateNetworkEndpointParams,
};

impl RailwayMcp {
    pub(crate) async fn do_list_private_networks(
        &self,
        params: EnvironmentParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_context(params.project_id, params.environment_id)
            .await?;
        let networks = private_network::fetch_private_networks(
            &self.client,
            &self.configs,
            &ctx.environment_id,
        )
        .await
        .map_err(|e| {
            McpError::internal_error(format!("Failed to fetch private networks: {e}"), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(
            format_private_network_list(&ctx.environment_id, &networks),
        )]))
    }

    pub(crate) async fn do_private_network_status(
        &self,
        params: PrivateNetworkStatusParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let networks = private_network::fetch_private_networks(
            &self.client,
            &self.configs,
            &ctx.environment_id,
        )
        .await
        .map_err(|e| {
            McpError::internal_error(format!("Failed to fetch private networks: {e}"), None)
        })?;
        let network = resolve_private_network_from_list(&networks, params.network.as_deref())?;

        let endpoint = private_network::fetch_private_network_endpoint(
            &self.client,
            &self.configs,
            &network.public_id,
            &ctx.environment_id,
            &ctx.service_id,
        )
        .await
        .map_err(|e| {
            McpError::internal_error(
                format!("Failed to fetch private network endpoint: {e}"),
                None,
            )
        })?;
        let internal_url = if let Some(endpoint) = endpoint.as_ref() {
            let configured_endpoint_name = private_network::fetch_configured_endpoint_name(
                &self.client,
                &self.configs,
                &ctx.environment_id,
                &ctx.service_id,
            )
            .await
            .map_err(|e| {
                McpError::internal_error(
                    format!("Failed to fetch private endpoint config: {e}"),
                    None,
                )
            })?;

            Some(private_network::internal_url_for_dns_name(
                network,
                configured_endpoint_name
                    .as_deref()
                    .unwrap_or(&endpoint.dns_name),
            ))
        } else {
            None
        };

        Ok(CallToolResult::success(vec![Content::text(
            format_private_network_status(
                &ctx,
                network,
                endpoint.as_ref(),
                internal_url.as_deref(),
            ),
        )]))
    }

    pub(crate) async fn do_enable_private_network(
        &self,
        params: EnablePrivateNetworkParams,
    ) -> Result<CallToolResult, McpError> {
        let endpoint = params
            .endpoint
            .as_deref()
            .map(private_network::validate_endpoint_prefix)
            .transpose()
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;

        if let Some(endpoint) = endpoint {
            let ctx = self
                .resolve_service_context(
                    params.project_id,
                    params.service_id,
                    params.environment_id,
                )
                .await?;
            let service_name = service_name(&ctx);
            let mode = private_network::apply_enable_patch(
                &self.client,
                &self.configs,
                &ctx.environment_id,
                Some(&ctx.service_id),
                Some(&endpoint),
                params.stage,
                Some(params.message.unwrap_or_else(|| {
                    format!(
                        "Enable private networking for {} with endpoint {}",
                        service_name, endpoint
                    )
                })),
            )
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to enable private networking: {e}"), None)
            })?;

            if mode == PatchMode::Commit {
                verify_private_network_commit(
                    &self.client,
                    &self.configs,
                    &ctx.environment_id,
                    Some((&ctx.service_id, &endpoint)),
                )
                .await?;
            }

            return Ok(CallToolResult::success(vec![Content::text(
                format_private_network_change(
                    &ctx.environment_id,
                    Some((&service_name, &ctx.service_id)),
                    Some(&endpoint),
                    mode,
                ),
            )]));
        }

        let ctx = self
            .resolve_context(params.project_id, params.environment_id)
            .await?;
        let mode = private_network::apply_enable_patch(
            &self.client,
            &self.configs,
            &ctx.environment_id,
            None,
            None,
            params.stage,
            Some(
                params
                    .message
                    .unwrap_or_else(|| "Enable private networking".to_string()),
            ),
        )
        .await
        .map_err(|e| {
            McpError::internal_error(format!("Failed to enable private networking: {e}"), None)
        })?;

        if mode == PatchMode::Commit {
            verify_private_network_commit(&self.client, &self.configs, &ctx.environment_id, None)
                .await?;
        }

        Ok(CallToolResult::success(vec![Content::text(
            format_private_network_change(&ctx.environment_id, None, None, mode),
        )]))
    }

    pub(crate) async fn do_set_private_network_endpoint(
        &self,
        params: SetPrivateNetworkEndpointParams,
    ) -> Result<CallToolResult, McpError> {
        let endpoint = private_network::validate_endpoint_prefix(&params.endpoint)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let service_name = service_name(&ctx);
        let mode = private_network::apply_set_endpoint_patch(
            &self.client,
            &self.configs,
            &ctx.environment_id,
            &ctx.service_id,
            &endpoint,
            params.stage,
            Some(params.message.unwrap_or_else(|| {
                format!(
                    "Set private network endpoint for {} to {}",
                    service_name, endpoint
                )
            })),
        )
        .await
        .map_err(|e| {
            McpError::internal_error(format!("Failed to set private network endpoint: {e}"), None)
        })?;

        if mode == PatchMode::Commit {
            verify_private_network_commit(
                &self.client,
                &self.configs,
                &ctx.environment_id,
                Some((&ctx.service_id, &endpoint)),
            )
            .await?;
        }

        Ok(CallToolResult::success(vec![Content::text(
            format_private_network_change(
                &ctx.environment_id,
                Some((&service_name, &ctx.service_id)),
                Some(&endpoint),
                mode,
            ),
        )]))
    }

    pub(crate) async fn do_check_private_network_endpoint_name(
        &self,
        params: CheckPrivateNetworkEndpointNameParams,
    ) -> Result<CallToolResult, McpError> {
        let endpoint = private_network::validate_endpoint_prefix(&params.endpoint)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        let ctx = self
            .resolve_context(params.project_id, params.environment_id)
            .await?;
        let networks = private_network::fetch_private_networks(
            &self.client,
            &self.configs,
            &ctx.environment_id,
        )
        .await
        .map_err(|e| {
            McpError::internal_error(format!("Failed to fetch private networks: {e}"), None)
        })?;
        let network = resolve_private_network_from_list(&networks, params.network.as_deref())?;
        let available = private_network::private_network_endpoint_name_available(
            &self.client,
            &self.configs,
            &network.public_id,
            &ctx.environment_id,
            &endpoint,
        )
        .await
        .map_err(|e| {
            McpError::internal_error(format!("Failed to check endpoint name: {e}"), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Endpoint name '{}' is {} on private network {} (id: {}).",
            endpoint,
            if available {
                "available"
            } else {
                "already used"
            },
            network.name,
            network.public_id
        ))]))
    }
}

async fn verify_private_network_commit(
    client: &reqwest::Client,
    configs: &crate::config::Configs,
    environment_id: &str,
    endpoint: Option<(&str, &str)>,
) -> Result<(), McpError> {
    private_network::verify_private_network_enabled(client, configs, environment_id)
        .await
        .map_err(|e| {
            McpError::internal_error(
                format!("Failed to verify private networking config: {e}"),
                None,
            )
        })?;

    if let Some((service_id, endpoint)) = endpoint {
        private_network::verify_endpoint_configured(
            client,
            configs,
            environment_id,
            service_id,
            endpoint,
        )
        .await
        .map_err(|e| {
            McpError::internal_error(
                format!("Failed to verify private endpoint config: {e}"),
                None,
            )
        })?;
    }

    Ok(())
}

fn resolve_private_network_from_list<'a>(
    networks: &'a [PrivateNetwork],
    identifier: Option<&str>,
) -> Result<&'a PrivateNetwork, McpError> {
    private_network::resolve_private_network(networks, identifier)
        .map_err(|e| McpError::invalid_params(e.to_string(), None))?
        .ok_or_else(|| {
            McpError::invalid_params(
                "No private network found in the selected environment.",
                None,
            )
        })
}

fn format_private_network_list(environment_id: &str, networks: &[PrivateNetwork]) -> String {
    if networks.is_empty() {
        return format!("No private networks found in environment {environment_id}.");
    }

    let mut output = format!("Private networks for environment {environment_id}:\n");
    for network in networks {
        output.push_str(&format!(
            "- {} (id: {}, dns: {}.internal, networkId: {}, address: {})\n",
            network.name,
            network.public_id,
            network.dns_name,
            network.network_id,
            if network.supports_ipv4 {
                "IPv4 & IPv6"
            } else {
                "IPv6"
            }
        ));
    }
    output
}

fn format_private_network_status(
    ctx: &ResolvedServiceContext,
    network: &PrivateNetwork,
    endpoint: Option<&PrivateNetworkEndpoint>,
    internal_url: Option<&str>,
) -> String {
    let service_name = service_name(ctx);
    let mut output = format!(
        "Private network endpoint for service {service_name} (id: {}) in environment {}:\nNetwork: {} (id: {}, dns: {}.internal, networkId: {})\nAddress family: {}",
        ctx.service_id,
        ctx.environment_id,
        network.name,
        network.public_id,
        network.dns_name,
        network.network_id,
        if network.supports_ipv4 {
            "IPv4 & IPv6"
        } else {
            "IPv6"
        }
    );

    let Some(endpoint) = endpoint else {
        output.push_str(
            "\nEndpoint: initializing; deploy or apply pending config changes to create it.",
        );
        return output;
    };

    output.push_str(&format!(
        "\nEndpoint ID: {}\nEndpoint name: {}\nSync status: {}",
        endpoint.public_id, endpoint.dns_name, endpoint.sync_status
    ));
    if let Some(internal_url) = internal_url {
        output.push_str(&format!("\nInternal URL: {internal_url}"));
    }
    if let Some(new_dns_name) = &endpoint.new_dns_name {
        output.push_str(&format!("\nPending name: {new_dns_name}"));
    }
    if !endpoint.private_ips.is_empty() {
        output.push_str(&format!(
            "\nPrivate IPs: {}",
            endpoint.private_ips.join(", ")
        ));
    }
    output
}

fn format_private_network_change(
    environment_id: &str,
    service: Option<(&str, &str)>,
    endpoint: Option<&str>,
    mode: PatchMode,
) -> String {
    let change = match mode {
        PatchMode::Commit => "committed",
        PatchMode::Stage => {
            "staged (environment has pending changes; use `railway environment edit` to commit)"
        }
    };
    let mut output = format!(
        "Private networking config {change} in environment {environment_id}. Dashboard parity: this used the environment config workflow, not direct endpoint mutations."
    );

    if let Some((service_name, service_id)) = service {
        output.push_str(&format!("\nService: {service_name} (id: {service_id})"));
    }
    if let Some(endpoint) = endpoint {
        output.push_str(&format!("\nEndpoint: {endpoint}"));
    }

    output
}

fn service_name(ctx: &ResolvedServiceContext) -> String {
    ctx.context
        .project
        .services
        .edges
        .iter()
        .find(|service| service.node.id == ctx.service_id)
        .map(|service| service.node.name.clone())
        .unwrap_or_else(|| ctx.service_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_network(name: &str) -> PrivateNetwork {
        PrivateNetwork {
            public_id: format!("pn_{name}"),
            project_id: "project_123".to_string(),
            environment_id: "env_123".to_string(),
            name: name.to_string(),
            dns_name: name.to_string(),
            network_id: 42,
            tags: vec![],
            supports_ipv4: false,
            created_at: None,
            deleted_at: None,
        }
    }

    #[test]
    fn private_network_change_output_reports_config_workflow() {
        let output = format_private_network_change(
            "env_123",
            Some(("api", "svc_123")),
            Some("api-internal"),
            PatchMode::Stage,
        );

        assert!(output.contains("staged"));
        assert!(output.contains("environment config workflow"));
        assert!(output.contains("api-internal"));
    }

    #[test]
    fn private_network_list_includes_internal_suffix() {
        let output = format_private_network_list("env_123", &[sample_network("railway")]);

        assert!(output.contains("railway.internal"));
        assert!(output.contains("pn_railway"));
    }
}
