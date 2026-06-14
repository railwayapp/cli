use rmcp::{ErrorData as McpError, model::*};

use crate::controllers::tcp_proxy::{self, PatchMode, TcpProxy};

use super::super::handler::{RailwayMcp, ResolvedServiceContext};
use super::super::params::{
    CreateTcpProxyParams, RemoveTcpProxyParams, ServiceParams, TcpProxySelectorParams,
};

impl RailwayMcp {
    pub(crate) async fn do_list_tcp_proxies(
        &self,
        params: ServiceParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let proxies = tcp_proxy::fetch_tcp_proxies(
            &self.client,
            &self.configs,
            &ctx.environment_id,
            &ctx.service_id,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to fetch TCP proxies: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(
            format_tcp_proxy_list(&ctx, &proxies),
        )]))
    }

    pub(crate) async fn do_create_tcp_proxy(
        &self,
        params: CreateTcpProxyParams,
    ) -> Result<CallToolResult, McpError> {
        let port = tcp_proxy::validate_application_port(params.application_port)
            .map_err(|e| McpError::invalid_params(e, None))?;
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let proxies = tcp_proxy::fetch_tcp_proxies(
            &self.client,
            &self.configs,
            &ctx.environment_id,
            &ctx.service_id,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to fetch TCP proxies: {e}"), None))?;

        if let Some(proxy) = tcp_proxy::existing_proxy_for_create(&proxies, port)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?
        {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "TCP proxy already exists for application port {}.\n{}",
                port,
                format_tcp_proxy_details(proxy)
            ))]));
        }

        let service_name = service_name(&ctx);
        let mode = tcp_proxy::apply_tcp_proxy_patch(
            &self.client,
            &self.configs,
            &ctx.context.project,
            &ctx.environment_id,
            &ctx.service_id,
            &service_name,
            port,
        )
        .await
        .map_err(|e| {
            McpError::internal_error(format!("Failed to configure TCP proxy: {e}"), None)
        })?;

        if mode == PatchMode::Commit {
            tcp_proxy::verify_tcp_proxy_configured(
                &self.client,
                &self.configs,
                &ctx.environment_id,
                &ctx.service_id,
                port,
            )
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to verify TCP proxy config: {e}"), None)
            })?;
        }

        let active_proxy = tcp_proxy::fetch_tcp_proxies(
            &self.client,
            &self.configs,
            &ctx.environment_id,
            &ctx.service_id,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to fetch TCP proxies: {e}"), None))?
        .into_iter()
        .find(|proxy| proxy.application_port == i64::from(port));

        Ok(CallToolResult::success(vec![Content::text(
            format_tcp_proxy_create(&ctx, port, mode, active_proxy.as_ref()),
        )]))
    }

    pub(crate) async fn do_get_tcp_proxy(
        &self,
        params: TcpProxySelectorParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let proxies = tcp_proxy::fetch_tcp_proxies(
            &self.client,
            &self.configs,
            &ctx.environment_id,
            &ctx.service_id,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to fetch TCP proxies: {e}"), None))?;
        let proxy = resolve_tcp_proxy_from_list(&proxies, &params.proxy)?;

        Ok(CallToolResult::success(vec![Content::text(
            format_tcp_proxy_details(proxy),
        )]))
    }

    pub(crate) async fn do_remove_tcp_proxy(
        &self,
        params: RemoveTcpProxyParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let proxies = tcp_proxy::fetch_tcp_proxies(
            &self.client,
            &self.configs,
            &ctx.environment_id,
            &ctx.service_id,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to fetch TCP proxies: {e}"), None))?;
        let proxy = resolve_tcp_proxy_from_list(&proxies, &params.proxy)?;

        if !params.confirm {
            return Ok(CallToolResult::success(vec![Content::text(
                format_tcp_proxy_remove_preview(proxy),
            )]));
        }

        tcp_proxy::delete_tcp_proxy(
            &self.client,
            &self.configs,
            &ctx.environment_id,
            &ctx.service_id,
            proxy,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to remove TCP proxy: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "TCP proxy removed: {} (id: {}, application port: {}, committed: true)",
            proxy.endpoint, proxy.id, proxy.application_port
        ))]))
    }
}

fn resolve_tcp_proxy_from_list<'a>(
    proxies: &'a [TcpProxy],
    identifier: &str,
) -> Result<&'a TcpProxy, McpError> {
    tcp_proxy::find_tcp_proxy(proxies, identifier)
        .map_err(|e| McpError::invalid_params(e.to_string(), None))?
        .ok_or_else(|| {
            McpError::invalid_params(
                format!(
                    "TCP proxy '{}' not found on the selected service",
                    identifier
                ),
                None,
            )
        })
}

fn format_tcp_proxy_list(ctx: &ResolvedServiceContext, proxies: &[TcpProxy]) -> String {
    let service_name = service_name(ctx);
    if proxies.is_empty() {
        return format!(
            "No TCP proxies found for service {service_name} (id: {}) in environment {}.",
            ctx.service_id, ctx.environment_id
        );
    }

    let mut output = format!(
        "TCP proxies for service {service_name} (id: {}) in environment {}:\n",
        ctx.service_id, ctx.environment_id
    );
    for proxy in proxies {
        output.push_str(&format!(
            "- {} (id: {}, application port: {}, proxy port: {}, sync: {})\n",
            proxy.endpoint, proxy.id, proxy.application_port, proxy.proxy_port, proxy.sync_status
        ));
    }
    output
}

fn format_tcp_proxy_create(
    ctx: &ResolvedServiceContext,
    port: u16,
    mode: PatchMode,
    active_proxy: Option<&TcpProxy>,
) -> String {
    let service_name = service_name(ctx);
    format_tcp_proxy_create_for_service(&service_name, &ctx.service_id, port, mode, active_proxy)
}

fn format_tcp_proxy_create_for_service(
    service_name: &str,
    service_id: &str,
    port: u16,
    mode: PatchMode,
    active_proxy: Option<&TcpProxy>,
) -> String {
    let change = match mode {
        PatchMode::Commit => "committed",
        PatchMode::Stage => {
            "staged (environment has pending changes; use `railway environment edit` to commit)"
        }
    };
    let mut output = format!(
        "TCP proxy configured for service {service_name} (id: {}) on application port {port}.\nChange: {change}",
        service_id
    );

    if let Some(proxy) = active_proxy {
        output.push_str(&format!("\n{}", format_tcp_proxy_details(proxy)));
    } else {
        output.push_str(
            "\nRedeploy the service for the TCP proxy to become active, then call list_tcp_proxies.",
        );
    }

    output
}

fn format_tcp_proxy_details(proxy: &TcpProxy) -> String {
    let mut output = format!(
        "TCP proxy:\nEndpoint: {}\nID: {}\nDomain: {}\nProxy port: {}\nApplication port: {}\nSync status: {}\nService ID: {}\nEnvironment ID: {}",
        proxy.endpoint,
        proxy.id,
        proxy.domain,
        proxy.proxy_port,
        proxy.application_port,
        proxy.sync_status,
        proxy.service_id,
        proxy.environment_id
    );

    if let Some(created_at) = &proxy.created_at {
        output.push_str(&format!("\nCreated: {created_at}"));
    }
    if let Some(updated_at) = &proxy.updated_at {
        output.push_str(&format!("\nUpdated: {updated_at}"));
    }

    output
}

fn format_tcp_proxy_remove_preview(proxy: &TcpProxy) -> String {
    format!(
        "This will permanently remove TCP proxy {} (id: {}, application port: {}). Call again with confirm: true to proceed.",
        proxy.endpoint, proxy.id, proxy.application_port
    )
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

    #[test]
    fn tcp_proxy_details_include_agent_selectors() {
        let output = format_tcp_proxy_details(&sample_proxy());

        assert!(output.contains("Endpoint: containers-us-west.railway.app:15432"));
        assert!(output.contains("ID: tcp_123"));
        assert!(output.contains("Application port: 5432"));
        assert!(output.contains("Sync status: ACTIVE"));
    }

    #[test]
    fn tcp_proxy_remove_preview_requires_confirmation() {
        let output = format_tcp_proxy_remove_preview(&sample_proxy());

        assert!(output.contains("confirm: true"));
        assert!(output.contains("tcp_123"));
        assert!(output.contains("5432"));
    }

    #[test]
    fn tcp_proxy_create_output_reports_staged_or_committed() {
        let proxy = sample_proxy();

        let committed = format_tcp_proxy_create_for_service(
            "redis",
            "svc_123",
            5432,
            PatchMode::Commit,
            Some(&proxy),
        );
        assert!(committed.contains("Change: committed"));
        assert!(committed.contains("Endpoint: containers-us-west.railway.app:15432"));

        let staged =
            format_tcp_proxy_create_for_service("redis", "svc_123", 5432, PatchMode::Stage, None);
        assert!(staged.contains("Change: staged"));
        assert!(staged.contains("Redeploy the service"));
    }
}
