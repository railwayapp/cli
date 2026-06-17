use rmcp::{ErrorData as McpError, model::*};

use crate::controllers::private_network::{self, PrivateNetworkState, PrivateNetworkStatus};

use super::super::handler::{RailwayMcp, ResolvedServiceContext};
use super::super::params::{PrivateNetworkParams, UpdatePrivateNetworkParams};

impl RailwayMcp {
    pub(crate) async fn do_private_network_status(
        &self,
        params: PrivateNetworkParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let statuses = private_network::fetch_private_network_statuses(
            &self.client,
            &self.configs,
            &ctx.environment_id,
            &ctx.service_id,
            params.network.as_deref(),
        )
        .await
        .map_err(mcp_private_network_error)?;

        Ok(CallToolResult::success(vec![Content::text(
            format_private_network_statuses(&ctx, &statuses),
        )]))
    }

    pub(crate) async fn do_private_network_update(
        &self,
        params: UpdatePrivateNetworkParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let status = private_network::update_private_network_endpoint_name(
            &self.client,
            &self.configs,
            &ctx.environment_id,
            &ctx.service_id,
            params.network.as_deref(),
            &params.name,
        )
        .await
        .map_err(mcp_private_network_error)?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Private network endpoint updated.\n\n{}",
            format_private_network_status(&status)
        ))]))
    }
}

fn format_private_network_statuses(
    ctx: &ResolvedServiceContext,
    statuses: &[PrivateNetworkStatus],
) -> String {
    if statuses.is_empty() {
        return format!(
            "No private networks found for service {} in environment {}.",
            ctx.service_id, ctx.environment_id
        );
    }

    let mut output = format!(
        "Private networks for service {} in environment {}:\n\n",
        ctx.service_id, ctx.environment_id
    );

    for (idx, status) in statuses.iter().enumerate() {
        if idx > 0 {
            output.push('\n');
        }
        output.push_str(&format_private_network_status(status));
        output.push('\n');
    }

    output
}

fn format_private_network_status(status: &PrivateNetworkStatus) -> String {
    let mut output = format!(
        "Network: {} (id: {}, dns: {})\nAddress family: {}\nState: {}",
        status.network.name,
        status.network.id,
        status.network.dns_name,
        status.network.ip_family,
        state_label(status.state)
    );

    if let Some(hostname) = &status.full_hostname {
        output.push_str(&format!("\nHostname: {hostname}"));
    }
    if let Some(short_name) = &status.short_name {
        output.push_str(&format!("\nShort name: {short_name}"));
    }
    if let Some(pending_hostname) = &status.pending_hostname {
        output.push_str(&format!("\nPending hostname: {pending_hostname}"));
    }
    if let Some(endpoint) = &status.endpoint {
        output.push_str(&format!(
            "\nEndpoint ID: {}\nSync status: {}\nService instance ID: {}",
            endpoint.id, endpoint.sync_status, endpoint.service_instance_id
        ));
        if !endpoint.private_ips.is_empty() {
            output.push_str(&format!(
                "\nPrivate IPs: {}",
                endpoint.private_ips.join(", ")
            ));
        }
    } else {
        output.push_str(
            "\nMessage: Private networking is initializing and will be ready once the deployment of this service is complete.",
        );
    }

    output
}

fn state_label(state: PrivateNetworkState) -> &'static str {
    match state {
        PrivateNetworkState::Ready => "ready",
        PrivateNetworkState::Creating => "creating",
        PrivateNetworkState::Updating => "updating",
        PrivateNetworkState::Deleting => "deleting",
        PrivateNetworkState::Initializing => "initializing",
        PrivateNetworkState::Unknown => "unknown",
    }
}

fn mcp_private_network_error(error: anyhow::Error) -> McpError {
    let message = error.to_string();
    if message.contains("Malformed")
        || message.contains("already used")
        || message.contains("Multiple private networks")
        || message.contains("Private network '")
        || message.contains("Endpoint name must")
        || message.contains("Enter your endpoint name")
    {
        McpError::invalid_params(message, None)
    } else {
        McpError::internal_error(
            format!("Failed to manage private networking: {message}"),
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::controllers::private_network::{PrivateNetwork, PrivateNetworkEndpoint};

    use super::*;

    fn status() -> PrivateNetworkStatus {
        private_network::private_network_status(
            PrivateNetwork {
                id: "pn_123".to_string(),
                project_id: "project".to_string(),
                environment_id: "environment".to_string(),
                name: "railway".to_string(),
                dns_name: "railway".to_string(),
                ip_family: "IPv4 & IPv6".to_string(),
                network_id: 1,
                tags: vec!["SUPPORTS_IPV4_PRIVNETS".to_string()],
                created_at: None,
            },
            Some(PrivateNetworkEndpoint {
                id: "pne_123".to_string(),
                service_instance_id: "si_123".to_string(),
                dns_name: "api".to_string(),
                new_dns_name: None,
                private_ips: vec!["fd12::1".to_string()],
                sync_status: "ACTIVE".to_string(),
                tags: vec![],
                created_at: None,
            }),
        )
    }

    #[test]
    fn formats_private_network_status() {
        let output = format_private_network_status(&status());

        assert!(output.contains("Network: railway"));
        assert!(output.contains("Hostname: api.railway.internal"));
        assert!(output.contains("Address family: IPv4 & IPv6"));
        assert!(output.contains("Private IPs: fd12::1"));
    }

    #[test]
    fn validation_errors_are_invalid_params() {
        let error =
            private_network::validate_endpoint_name("api.railway.internal", "railway.internal")
                .unwrap_err();
        let mapped = mcp_private_network_error(error);

        assert_eq!(mapped.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }
}
