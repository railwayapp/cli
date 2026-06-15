use std::{fmt, path::PathBuf, sync::Arc};

use super::params::*;

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    service::RequestContext,
    tool, tool_router,
};

use crate::{
    client::post_graphql,
    config::Configs,
    controllers::{
        deployment::{FetchLogsParams, fetch_build_logs, fetch_deploy_logs, fetch_http_logs},
        environment::get_matched_environment,
        project::{get_environment_instances, get_project, get_service_ids_in_env},
        upload::{create_deploy_tarball, upload_deploy_tarball},
        user::get_user,
        variables::get_service_variables,
    },
    gql::{mutations, queries},
    telemetry,
    util::{
        logs::{HttpLogLike, LogLike},
        time::parse_time,
    },
    workspace::workspaces,
};

#[derive(Clone)]
pub struct RailwayMcp {
    pub(crate) client: reqwest::Client,
    pub(crate) configs: Arc<Configs>,
    tool_router: ToolRouter<Self>,
}

pub(crate) struct ResolvedContext {
    pub(crate) project_id: String,
    pub(crate) environment_id: String,
    pub(crate) project: queries::RailwayProject,
    pub(crate) linked: Option<crate::config::LinkedProject>,
}

pub(crate) struct ResolvedServiceContext {
    pub(crate) project_id: String,
    pub(crate) environment_id: String,
    pub(crate) service_id: String,
    pub(crate) context: ResolvedContext,
}

impl RailwayMcp {
    pub fn new(client: reqwest::Client, configs: Configs) -> Self {
        Self {
            client,
            configs: Arc::new(configs),
            tool_router: Self::tool_router(),
        }
    }

    pub(crate) async fn resolve_context(
        &self,
        project_id: Option<String>,
        environment_id: Option<String>,
    ) -> Result<ResolvedContext, McpError> {
        let local_linked = self.configs.get_local_linked_project().ok();
        let token_linked = self.configs.get_linked_project().await.ok();
        let linked = local_linked.or(token_linked);

        let project_id = project_id
            .or_else(|| linked.as_ref().map(|l| l.project.clone()))
            .ok_or_else(|| {
                McpError::invalid_params(
                    "No project_id provided and no linked project. Run 'railway link' or pass a project_id.",
                    None,
                )
            })?;

        let project = get_project(&self.client, &self.configs, project_id.clone())
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to get project: {e}"), None))?;

        let env_id_or_name = environment_id
            .or_else(|| linked.as_ref().and_then(|l| l.environment.clone()))
            .ok_or_else(|| {
                let available = format_environments(&project);
                McpError::invalid_params(
                    format!("No environment_id provided and no linked environment. Available environments:\n{available}"),
                    None,
                )
            })?;

        let environment = get_matched_environment(&project, env_id_or_name).map_err(|e| {
            let available = format_environments(&project);
            McpError::invalid_params(
                format!("Failed to resolve environment: {e}. Available environments:\n{available}"),
                None,
            )
        })?;

        Ok(ResolvedContext {
            project_id,
            environment_id: environment.id,
            project,
            linked,
        })
    }

    pub(crate) async fn get_latest_deployment_id(
        &self,
        project_id: &str,
        environment_id: &str,
        service_id: &str,
    ) -> Result<String, McpError> {
        let vars = queries::deployments::Variables {
            input: queries::deployments::DeploymentListInput {
                project_id: Some(project_id.to_owned()),
                environment_id: Some(environment_id.to_owned()),
                service_id: Some(service_id.to_owned()),
                include_deleted: None,
                status: None,
            },
            first: Some(1),
        };
        let response = post_graphql::<queries::Deployments, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to fetch deployments: {e}"), None))?;

        response
            .deployments
            .edges
            .first()
            .map(|e| e.node.id.clone())
            .ok_or_else(|| {
                McpError::internal_error("No deployments found for this service.".to_string(), None)
            })
    }

    pub(crate) async fn resolve_service_context(
        &self,
        project_id: Option<String>,
        service_id: Option<String>,
        environment_id: Option<String>,
    ) -> Result<ResolvedServiceContext, McpError> {
        let ctx = self.resolve_context(project_id, environment_id).await?;

        let service_id = match service_id {
            Some(sid) => ctx
                .project
                .services
                .edges
                .iter()
                .find(|s| s.node.id == sid || s.node.name.eq_ignore_ascii_case(&sid))
                .map(|s| s.node.id.clone())
                .ok_or_else(|| {
                    let available = format_services(&ctx.project);
                    McpError::invalid_params(
                        format!("Service '{sid}' not found. Available services:\n{available}"),
                        None,
                    )
                })?,
            None => ctx
                .linked
                .as_ref()
                .and_then(|l| l.service.clone())
                .ok_or_else(|| {
                let available = format_services(&ctx.project);
                McpError::invalid_params(
                    format!("No service_id provided and no linked service. Available services:\n{available}"),
                    None,
                )
            })?,
        };

        Ok(ResolvedServiceContext {
            project_id: ctx.project_id.clone(),
            environment_id: ctx.environment_id.clone(),
            service_id,
            context: ctx,
        })
    }

    pub(crate) async fn fetch_domains(
        &self,
        ctx: &ResolvedServiceContext,
    ) -> Result<queries::domains::DomainsDomains, McpError> {
        post_graphql::<queries::Domains, _>(
            &self.client,
            self.configs.get_backboard(),
            queries::domains::Variables {
                environment_id: ctx.environment_id.clone(),
                project_id: ctx.project_id.clone(),
                service_id: ctx.service_id.clone(),
            },
        )
        .await
        .map(|response| response.domains)
        .map_err(|e| McpError::internal_error(format!("Failed to query domains: {e}"), None))
    }

    pub(crate) async fn resolve_domain_details(
        &self,
        ctx: &ResolvedServiceContext,
        identifier: &str,
    ) -> Result<McpDomainDetails, McpError> {
        let domains = self.fetch_domains(ctx).await?;
        let items = mcp_domain_items(&domains);
        find_mcp_domain(&items, identifier).cloned().ok_or_else(|| {
            McpError::invalid_params(
                format!("Domain '{identifier}' not found on the selected service."),
                None,
            )
        })
    }
}

fn format_environments(project: &queries::RailwayProject) -> String {
    project
        .environments
        .edges
        .iter()
        .map(|e| format!("- {} (id: {})", e.node.name, e.node.id))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_services(project: &queries::RailwayProject) -> String {
    project
        .services
        .edges
        .iter()
        .map(|s| format!("- {} (id: {})", s.node.name, s.node.id))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpDomainKind {
    Custom,
    Service,
}

impl fmt::Display for McpDomainKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            McpDomainKind::Custom => write!(f, "custom"),
            McpDomainKind::Service => write!(f, "service"),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct McpDomainDetails {
    id: String,
    domain: String,
    kind: McpDomainKind,
    target_port: Option<i64>,
    sync_status: String,
    service_domain_suffix: Option<String>,
    environment_id: String,
    service_id: String,
    dns_records: Vec<McpDnsRecord>,
    verification: Option<McpVerification>,
    certificate: Option<McpCertificateStatus>,
}

#[derive(Debug, Clone)]
struct McpDnsRecord {
    record_type: String,
    name: String,
    required_value: String,
    current_value: String,
    status: String,
    zone: String,
}

#[derive(Debug, Clone)]
struct McpVerification {
    verified: bool,
    dns_host: Option<String>,
    token: Option<String>,
}

#[derive(Debug, Clone)]
struct McpCertificateStatus {
    status: String,
    detailed_status: Option<String>,
    error_message: Option<String>,
    error_type: Option<String>,
    retryable: Option<bool>,
    cdn_provider: Option<String>,
}

fn mcp_domain_items(domains: &queries::domains::DomainsDomains) -> Vec<McpDomainDetails> {
    domains
        .service_domains
        .iter()
        .map(mcp_domain_from_service)
        .chain(domains.custom_domains.iter().map(mcp_domain_from_custom))
        .collect()
}

fn mcp_domain_from_service(
    domain: &queries::domains::DomainsDomainsServiceDomains,
) -> McpDomainDetails {
    McpDomainDetails {
        id: domain.id.clone(),
        domain: domain.domain.clone(),
        kind: McpDomainKind::Service,
        target_port: domain.target_port,
        sync_status: enum_name(&domain.sync_status),
        service_domain_suffix: domain.suffix.clone(),
        environment_id: domain.environment_id.clone(),
        service_id: domain.service_id.clone(),
        dns_records: Vec::new(),
        verification: None,
        certificate: None,
    }
}

fn mcp_domain_from_custom(
    domain: &queries::domains::DomainsDomainsCustomDomains,
) -> McpDomainDetails {
    McpDomainDetails {
        id: domain.id.clone(),
        domain: domain.domain.clone(),
        kind: McpDomainKind::Custom,
        target_port: domain.target_port,
        sync_status: enum_name(&domain.sync_status),
        service_domain_suffix: None,
        environment_id: domain.environment_id.clone(),
        service_id: domain.service_id.clone(),
        dns_records: domain
            .status
            .dns_records
            .iter()
            .map(|record| McpDnsRecord {
                record_type: enum_name(&record.record_type),
                name: if record.hostlabel.is_empty() {
                    "@".to_string()
                } else {
                    record.hostlabel.clone()
                },
                required_value: record.required_value.clone(),
                current_value: record.current_value.clone(),
                status: enum_name(&record.status),
                zone: record.zone.clone(),
            })
            .collect(),
        verification: Some(McpVerification {
            verified: domain.status.verified,
            dns_host: domain.status.verification_dns_host.clone(),
            token: domain.status.verification_token.clone(),
        }),
        certificate: Some(McpCertificateStatus {
            status: enum_name(&domain.status.certificate_status),
            detailed_status: enum_name_option(&domain.status.certificate_status_detailed),
            error_message: domain.status.certificate_error_message.clone(),
            error_type: enum_name_option(&domain.status.certificate_error_type),
            retryable: domain.status.certificate_retryable,
            cdn_provider: enum_name_option(&domain.status.cdn_provider),
        }),
    }
}

fn find_mcp_domain<'a>(
    domains: &'a [McpDomainDetails],
    identifier: &str,
) -> Option<&'a McpDomainDetails> {
    let normalized = normalize_domain_identifier(identifier);

    domains.iter().find(|domain| {
        domain.id.eq_ignore_ascii_case(identifier)
            || domain.domain.eq_ignore_ascii_case(&normalized)
    })
}

fn normalize_domain_identifier(identifier: &str) -> String {
    let trimmed = identifier.trim();
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);

    without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .trim_end_matches('.')
        .to_string()
}

fn format_domains(domains: &[McpDomainDetails]) -> String {
    if domains.is_empty() {
        return "No domains found.".to_string();
    }

    let mut output = String::from("## Domains\n");
    for domain in domains {
        output.push_str(&format!(
            "- https://{} (type: {}, id: {}, target_port: {}, sync_status: {})\n",
            domain.domain,
            domain.kind,
            domain.id,
            format_target_port(domain.target_port),
            domain.sync_status
        ));
    }
    output
}

fn format_domain_details(domain: &McpDomainDetails) -> String {
    let mut output = format!(
        "## Domain status\nURL: https://{}\nID: {}\nType: {}\nTarget port: {}\nSync status: {}\n",
        domain.domain,
        domain.id,
        domain.kind,
        format_target_port(domain.target_port),
        domain.sync_status
    );

    if let Some(verification) = &domain.verification {
        output.push_str(&format!(
            "Verified: {}\n",
            if verification.verified { "yes" } else { "no" }
        ));
    }

    if let Some(certificate) = &domain.certificate {
        output.push_str(&format!("Certificate status: {}\n", certificate.status));
        if let Some(detailed_status) = &certificate.detailed_status {
            output.push_str(&format!("Certificate detail: {detailed_status}\n"));
        }
        if let Some(error_message) = &certificate.error_message {
            output.push_str(&format!("Certificate error: {error_message}\n"));
        }
        if let Some(error_type) = &certificate.error_type {
            output.push_str(&format!("Certificate error type: {error_type}\n"));
        }
        if let Some(retryable) = certificate.retryable {
            output.push_str(&format!("Certificate retryable: {retryable}\n"));
        }
        if let Some(cdn_provider) = &certificate.cdn_provider {
            output.push_str(&format!("CDN provider: {cdn_provider}\n"));
        }
    }

    if !domain.dns_records.is_empty() {
        output.push_str("\nDNS records:\n");
        for record in &domain.dns_records {
            output.push_str(&format!(
                "- {} {} -> {} (status: {}, current: {})\n",
                record.record_type,
                record.name,
                record.required_value,
                record.status,
                if record.current_value.is_empty() {
                    "-"
                } else {
                    &record.current_value
                }
            ));
        }

        if let Some(verification) = &domain.verification
            && !verification.verified
            && let (Some(host), Some(token)) = (&verification.dns_host, &verification.token)
        {
            let zone = domain
                .dns_records
                .first()
                .map(|record| record.zone.as_str())
                .unwrap_or("");
            let host_label = host.strip_suffix(&format!(".{zone}")).unwrap_or(host);
            output.push_str(&format!(
                "- TXT {host_label} -> {} (verification)\n",
                verification_txt_value(token)
            ));
        }
    }

    output
}

fn verification_txt_value(token: &str) -> String {
    let mut token = token;
    while let Some(stripped) = token.strip_prefix("railway-verify=") {
        token = stripped;
    }
    format!("railway-verify={token}")
}

fn format_target_port(port: Option<i64>) -> String {
    port.map(|port| port.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn mcp_service_domain_input(
    domain: &McpDomainDetails,
    new_domain: &str,
) -> Result<String, McpError> {
    let normalized = normalize_domain_identifier(new_domain);

    if normalized.is_empty() {
        return Err(McpError::invalid_params(
            "new_domain must not be empty.",
            None,
        ));
    }

    if normalized.contains('.') {
        return Ok(normalized);
    }

    let Some(suffix) = &domain.service_domain_suffix else {
        return Err(McpError::invalid_params(
            "Pass the full service domain because the current suffix could not be resolved.",
            None,
        ));
    };

    Ok(format!("{normalized}.{suffix}"))
}

fn enum_name<T: fmt::Debug>(value: &T) -> String {
    format!("{value:?}")
}

fn enum_name_option<T: fmt::Debug>(value: &Option<T>) -> Option<String> {
    value.as_ref().map(enum_name)
}

fn validate_domain_port(port: i64) -> Result<i64, McpError> {
    if (1..=65535).contains(&port) {
        Ok(port)
    } else {
        Err(McpError::invalid_params(
            "port must be a number from 1 to 65535",
            None,
        ))
    }
}

#[tool_router]
impl RailwayMcp {
    #[tool(description = "Check Railway authentication status and return the current user")]
    async fn whoami(&self) -> Result<CallToolResult, McpError> {
        let user = get_user(&self.client, &self.configs).await.map_err(|e| {
            McpError::internal_error(
                format!("Not authenticated. Run 'railway login' first. Error: {e}"),
                None,
            )
        })?;

        let name = user.name.unwrap_or_else(|| "Unknown".to_string());
        let output = format!("Logged in as {} ({})", name, user.email);
        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    #[tool(
        description = "List all projects in the user's Railway account, grouped by workspace. Returns project names and IDs."
    )]
    async fn list_projects(&self) -> Result<CallToolResult, McpError> {
        let workspaces = workspaces()
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to list projects: {e}"), None))?;

        let mut output = String::new();
        for ws in &workspaces {
            output.push_str(&format!("## {}\n", ws.name()));
            for project in ws.projects() {
                output.push_str(&format!("- {} (id: {})\n", project.name(), project.id()));
            }
            output.push('\n');
        }

        if output.is_empty() {
            output = "No projects found.".to_string();
        }

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    #[tool(
        description = "List all Railway workspaces available to the current user. Returns workspace names, IDs, team IDs, and project counts. Use the workspace ID with create_project."
    )]
    async fn list_workspaces(&self) -> Result<CallToolResult, McpError> {
        let workspaces = workspaces().await.map_err(|e| {
            McpError::internal_error(format!("Failed to list workspaces: {e}"), None)
        })?;

        if workspaces.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No workspaces found.".to_string(),
            )]));
        }

        let output = workspaces
            .iter()
            .map(|workspace| {
                let kind = if workspace.team_id().is_some() {
                    "team"
                } else {
                    "personal"
                };
                let team_id = workspace.team_id().unwrap_or("none");
                format!(
                    "- {} (workspace_id: {}, type: {}, team_id: {}, projects: {})",
                    workspace.name(),
                    workspace.id(),
                    kind,
                    team_id,
                    workspace.projects().len()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    #[tool(
        description = "List all services in a Railway project. If no project_id is provided, uses the currently linked project."
    )]
    async fn list_services(
        &self,
        Parameters(params): Parameters<ProjectParams>,
    ) -> Result<CallToolResult, McpError> {
        let project_id = match params.project_id {
            Some(id) => id,
            None => {
                let linked = self.configs.get_linked_project().await.map_err(|e| {
                    McpError::internal_error(
                        format!("No linked project and no project_id provided. Run 'railway link' or pass a project_id. Error: {e}"),
                        None,
                    )
                })?;
                linked.project
            }
        };

        let project = get_project(&self.client, &self.configs, project_id)
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to get project: {e}"), None))?;

        let mut output = format!("## Services in project: {}\n\n", project.name);
        for edge in &project.services.edges {
            output.push_str(&format!("- {} (id: {})\n", edge.node.name, edge.node.id));
        }

        if project.services.edges.is_empty() {
            output.push_str("No services found.\n");
        }

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    #[tool(
        description = "List recent deployments for a service. Returns deployment IDs, status, timestamps, and commit hashes. If no IDs are provided, uses the currently linked project/service/environment."
    )]
    async fn list_deployments(
        &self,
        Parameters(params): Parameters<ListDeploymentsParams>,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let limit = params.limit.unwrap_or(20);

        let vars = queries::deployments::Variables {
            input: queries::deployments::DeploymentListInput {
                project_id: Some(ctx.project_id),
                environment_id: Some(ctx.environment_id),
                service_id: Some(ctx.service_id),
                include_deleted: None,
                status: None,
            },
            first: Some(limit),
        };

        let response = post_graphql::<queries::Deployments, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to fetch deployments: {e}"), None))?;

        let mut output = String::new();
        for edge in &response.deployments.edges {
            let node = &edge.node;
            let commit = node
                .meta
                .as_ref()
                .and_then(|m| m.get("commitHash"))
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            output.push_str(&format!(
                "{} | {:?} | {} | {}\n",
                node.id, node.status, node.created_at, commit
            ));
        }

        if output.is_empty() {
            output = "No deployments found.".to_string();
        }

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    #[tool(
        description = "List all environment variables for a service. Returns KEY=VALUE pairs. If no IDs are provided, uses the currently linked project/service/environment."
    )]
    async fn list_variables(
        &self,
        Parameters(params): Parameters<ServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;

        let variables = get_service_variables(
            &self.client,
            &self.configs,
            ctx.project_id,
            ctx.environment_id,
            ctx.service_id,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to fetch variables: {e}"), None))?;

        if variables.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No variables found.",
            )]));
        }

        let output: String = variables
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    #[tool(
        description = "Get build, deploy, or HTTP logs for a service's deployment. Set log_type to 'build', 'deploy', or 'http'. Supports filtering by level/search for build/deploy logs, and method/status/path/request_id for HTTP logs. If no deployment_id is provided, uses the latest deployment."
    )]
    async fn get_logs(
        &self,
        Parameters(params): Parameters<GetLogsParams>,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;

        let deployment_id = match params.deployment_id {
            Some(did) => did,
            None => {
                self.get_latest_deployment_id(&ctx.project_id, &ctx.environment_id, &ctx.service_id)
                    .await?
            }
        };

        let log_type = params.log_type.unwrap_or_default();

        let filter = {
            let mut parts = Vec::new();
            match log_type {
                LogType::Http => {
                    if let Some(ref method) = params.method {
                        parts.push(format!("@method:{method}"));
                    }
                    if let Some(ref status) = params.status {
                        parts.push(format!("@httpStatus:{status}"));
                    }
                    if let Some(ref path) = params.path {
                        parts.push(format!("@path:{path}"));
                    }
                    if let Some(ref request_id) = params.request_id {
                        parts.push(format!("@requestId:{request_id}"));
                    }
                }
                _ => {
                    if let Some(ref level) = params.level {
                        parts.push(format!("@level:{level}"));
                    }
                }
            }
            if let Some(ref search) = params.search {
                parts.push(search.clone());
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(" "))
            }
        };

        let start_date = params
            .since
            .as_deref()
            .map(parse_time)
            .transpose()
            .map_err(|e| McpError::invalid_params(format!("Invalid 'since' time: {e}"), None))?;
        let end_date = params
            .until
            .as_deref()
            .map(parse_time)
            .transpose()
            .map_err(|e| McpError::invalid_params(format!("Invalid 'until' time: {e}"), None))?;

        let lines = params.lines.unwrap_or(100);

        let backboard = self.configs.get_backboard();
        let fetch_params = FetchLogsParams {
            client: &self.client,
            backboard: &backboard,
            deployment_id: deployment_id.clone(),
            limit: Some(lines),
            filter,
            start_date,
            end_date,
        };

        let mut logs = Vec::<String>::new();

        match log_type {
            LogType::Build => {
                fetch_build_logs(fetch_params, |log| {
                    let line = format!("[{}] {}", log.timestamp(), log.message());
                    logs.push(line);
                })
                .await
                .map_err(|e| {
                    McpError::internal_error(format!("Failed to fetch build logs: {e}"), None)
                })?;
            }
            LogType::Http => {
                fetch_http_logs(fetch_params, |log| {
                    let line = format!(
                        "[{}] {} {} {} {}ms",
                        log.timestamp(),
                        log.method(),
                        log.path(),
                        log.http_status(),
                        log.total_duration(),
                    );
                    logs.push(line);
                })
                .await
                .map_err(|e| {
                    McpError::internal_error(format!("Failed to fetch HTTP logs: {e}"), None)
                })?;
            }
            LogType::Deploy => {
                fetch_deploy_logs(fetch_params, |log| {
                    let line = format!("[{}] {}", log.timestamp(), log.message());
                    logs.push(line);
                })
                .await
                .map_err(|e| {
                    McpError::internal_error(format!("Failed to fetch deploy logs: {e}"), None)
                })?;
            }
        }

        let output = logs.join("\n");
        if output.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(format!(
                "No {log_type:?} logs found for deployment {deployment_id}"
            ))]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(output)]))
        }
    }

    #[tool(
        description = "Set one or more environment variables on a service. Pass variables as a JSON object mapping names to values. Triggers a redeploy unless skip_deploys is true."
    )]
    async fn set_variables(
        &self,
        Parameters(params): Parameters<SetVariablesParams>,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;

        let keys: Vec<String> = params.variables.keys().cloned().collect();

        let vars = mutations::variable_collection_upsert::Variables {
            project_id: ctx.project_id,
            service_id: ctx.service_id,
            environment_id: ctx.environment_id,
            variables: params.variables,
            skip_deploys: params.skip_deploys,
        };

        post_graphql::<mutations::VariableCollectionUpsert, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to set variables: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Successfully set {} variable(s): {}",
            keys.len(),
            keys.join(", ")
        ))]))
    }

    #[tool(
        description = "Add a domain to a service. If 'domain' is provided, creates a custom domain and returns required DNS records. If omitted, generates a Railway service domain (or returns existing domains)."
    )]
    async fn generate_domain(
        &self,
        Parameters(params): Parameters<GenerateDomainParams>,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let target_port = params.port.map(validate_domain_port).transpose()?;

        if let Some(custom_domain) = &params.domain {
            // Custom domain creation performs availability checks server-side.
            let create_vars = mutations::custom_domain_create::Variables {
                input: mutations::custom_domain_create::CustomDomainCreateInput {
                    domain: custom_domain.clone(),
                    environment_id: ctx.environment_id.clone(),
                    project_id: ctx.project_id.clone(),
                    service_id: ctx.service_id.clone(),
                    target_port,
                },
            };
            let result = post_graphql::<mutations::CustomDomainCreate, _>(
                &self.client,
                self.configs.get_backboard(),
                create_vars,
            )
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to create custom domain: {e}"), None)
            })?;

            let domain = self
                .resolve_domain_details(&ctx, &result.custom_domain_create.id)
                .await?;
            Ok(CallToolResult::success(vec![Content::text(format!(
                "Custom domain created.\n\n{}",
                format_domain_details(&domain)
            ))]))
        } else {
            // Service domain flow: return existing or create new
            let existing = self.fetch_domains(&ctx).await?;

            let domains = &existing;
            if !domains.service_domains.is_empty() || !domains.custom_domains.is_empty() {
                let output = format!(
                    "Existing domains:\n{}",
                    format_domains(&mcp_domain_items(domains))
                );
                return Ok(CallToolResult::success(vec![Content::text(output)]));
            }

            // No domains exist — create a service domain
            let create_vars = mutations::service_domain_create::Variables {
                environment_id: ctx.environment_id.clone(),
                service_id: ctx.service_id.clone(),
                target_port,
            };
            let result = post_graphql::<mutations::ServiceDomainCreate, _>(
                &self.client,
                self.configs.get_backboard(),
                create_vars,
            )
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to create service domain: {e}"), None)
            })?;

            let domain = self
                .resolve_domain_details(&ctx, &result.service_domain_create.id)
                .await?;
            Ok(CallToolResult::success(vec![Content::text(format!(
                "Service domain created.\n\n{}",
                format_domain_details(&domain)
            ))]))
        }
    }

    #[tool(
        description = "List service and custom domains for a service. Returns domain, type, ID, target port, and sync status."
    )]
    async fn list_domains(
        &self,
        Parameters(params): Parameters<ServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let domains = self.fetch_domains(&ctx).await?;
        Ok(CallToolResult::success(vec![Content::text(
            format_domains(&mcp_domain_items(&domains)),
        )]))
    }

    #[tool(
        description = "Show status for a service or custom domain by domain name, URL, or domain ID. Includes DNS records, verification status, certificate status/errors, sync status, and target port."
    )]
    async fn domain_status(
        &self,
        Parameters(params): Parameters<DomainStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let domain = self.resolve_domain_details(&ctx, &params.domain).await?;
        Ok(CallToolResult::success(vec![Content::text(
            format_domain_details(&domain),
        )]))
    }

    #[tool(
        description = "Retry TLS certificate issuance for a custom domain by domain name, URL, or domain ID. Custom domains only. Returns updated status after readback."
    )]
    async fn retry_domain_certificate(
        &self,
        Parameters(params): Parameters<DomainStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let domain = self.resolve_domain_details(&ctx, &params.domain).await?;

        if domain.kind != McpDomainKind::Custom {
            return Err(McpError::invalid_params(
                "Certificate retry is only supported for custom domains.",
                None,
            ));
        }

        post_graphql::<mutations::CustomDomainIssueCertificate, _>(
            &self.client,
            self.configs.get_backboard_internal(),
            mutations::custom_domain_issue_certificate::Variables {
                id: domain.id.clone(),
            },
        )
        .await
        .map_err(|e| {
            McpError::internal_error(format!("Failed to retry domain certificate: {e}"), None)
        })?;

        let updated = self.resolve_domain_details(&ctx, &domain.id).await?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Certificate retry requested.\n\n{}",
            format_domain_details(&updated)
        ))]))
    }

    #[tool(
        description = "Delete a custom or service domain by domain name, URL, or domain ID. This is irreversible. Returns a preview first.",
        annotations(destructive_hint = true)
    )]
    async fn delete_domain(
        &self,
        Parameters(params): Parameters<DeleteDomainParams>,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let domain = self.resolve_domain_details(&ctx, &params.domain).await?;

        if !params.confirm {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "⚠️ This will permanently delete {} domain https://{} (id: {}). Call again with confirm: true to proceed.",
                domain.kind, domain.domain, domain.id
            ))]));
        }

        match domain.kind {
            McpDomainKind::Custom => {
                post_graphql::<mutations::CustomDomainDelete, _>(
                    &self.client,
                    self.configs.get_backboard(),
                    mutations::custom_domain_delete::Variables {
                        id: domain.id.clone(),
                    },
                )
                .await
                .map_err(|e| {
                    McpError::internal_error(format!("Failed to delete custom domain: {e}"), None)
                })?;
            }
            McpDomainKind::Service => {
                post_graphql::<mutations::ServiceDomainDelete, _>(
                    &self.client,
                    self.configs.get_backboard(),
                    mutations::service_domain_delete::Variables {
                        id: domain.id.clone(),
                    },
                )
                .await
                .map_err(|e| {
                    McpError::internal_error(format!("Failed to delete service domain: {e}"), None)
                })?;
            }
        }

        let domains = self.fetch_domains(&ctx).await?;
        let remaining = mcp_domain_items(&domains);
        if find_mcp_domain(&remaining, &domain.id).is_some() {
            return Err(McpError::internal_error(
                format!(
                    "Domain deletion was requested, but {} still exists after verification.",
                    domain.id
                ),
                None,
            ));
        }

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Deleted {} domain https://{} (id: {}).",
            domain.kind, domain.domain, domain.id
        ))]))
    }

    #[tool(
        description = "Update a custom or service domain by domain name, URL, or domain ID. Supports target port changes for both domain types and Railway service-domain renames via new_domain. Port must be from 1 to 65535. Returns updated status after readback."
    )]
    async fn update_domain(
        &self,
        Parameters(params): Parameters<UpdateDomainParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.port.is_none() && params.new_domain.is_none() {
            return Err(McpError::invalid_params(
                "Provide port, new_domain, or both.",
                None,
            ));
        }

        let port = params.port.map(validate_domain_port).transpose()?;
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;
        let domain = self.resolve_domain_details(&ctx, &params.domain).await?;

        match domain.kind {
            McpDomainKind::Custom => {
                if params.new_domain.is_some() {
                    return Err(McpError::invalid_params(
                        "Custom domains cannot be renamed. Create the new custom domain, then delete the old one.",
                        None,
                    ));
                }

                post_graphql::<mutations::CustomDomainUpdate, _>(
                    &self.client,
                    self.configs.get_backboard(),
                    mutations::custom_domain_update::Variables {
                        environment_id: domain.environment_id.clone(),
                        id: domain.id.clone(),
                        target_port: port,
                    },
                )
                .await
                .map_err(|e| {
                    McpError::internal_error(format!("Failed to update custom domain: {e}"), None)
                })?;
            }
            McpDomainKind::Service => {
                post_graphql::<mutations::ServiceDomainUpdate, _>(
                    &self.client,
                    self.configs.get_backboard(),
                    mutations::service_domain_update::Variables {
                        input: mutations::service_domain_update::ServiceDomainUpdateInput {
                            domain: params
                                .new_domain
                                .as_deref()
                                .map(|new_domain| mcp_service_domain_input(&domain, new_domain))
                                .transpose()?
                                .unwrap_or_else(|| domain.domain.clone()),
                            environment_id: domain.environment_id.clone(),
                            service_domain_id: domain.id.clone(),
                            service_id: domain.service_id.clone(),
                            target_port: port,
                        },
                    },
                )
                .await
                .map_err(|e| {
                    McpError::internal_error(format!("Failed to update service domain: {e}"), None)
                })?;
            }
        }

        let updated = self.resolve_domain_details(&ctx, &domain.id).await?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Updated domain https://{}.\n\n{}",
            domain.domain,
            format_domain_details(&updated)
        ))]))
    }

    #[tool(
        description = "Link a service to the current project directory for the CLI. If no service_id or service_name is provided, lists available services. Uses a fresh config to write the link."
    )]
    async fn link_service(
        &self,
        Parameters(params): Parameters<LinkServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        let linked = self.configs.get_linked_project().await.ok();
        let requested_project_id = params.project_id.clone();
        let project_id = params
            .project_id
            .or_else(|| linked.as_ref().map(|l| l.project.clone()))
            .ok_or_else(|| {
                McpError::invalid_params(
                    "No project_id provided and no linked project. Run 'railway link' first.",
                    None,
                )
            })?;

        let project = get_project(&self.client, &self.configs, project_id.clone())
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to get project: {e}"), None))?;

        // Filter to services present in the linked environment (matches CLI behavior)
        let environment_id = linked
            .as_ref()
            .filter(|l| {
                requested_project_id
                    .as_ref()
                    .is_none_or(|requested| requested == &l.project)
            })
            .and_then(|l| l.environment.as_deref());
        let env_service_ids = if let Some(eid) = environment_id {
            let environment = get_matched_environment(&project, eid.to_string()).map_err(|e| {
                let available = format_environments(&project);
                McpError::invalid_params(
                    format!(
                        "Failed to resolve environment: {e}. Available environments:\n{available}"
                    ),
                    None,
                )
            })?;
            let instances = get_environment_instances(
                &self.client,
                &self.configs,
                &project_id,
                &environment.id,
            )
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to get environment instances: {e}"), None)
            })?;
            Some(get_service_ids_in_env(&instances))
        } else {
            None
        };

        let services_in_env: Vec<_> = project
            .services
            .edges
            .iter()
            .filter(|s| {
                env_service_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&s.node.id))
            })
            .collect();

        if params.service_id.is_none() && params.service_name.is_none() {
            let available: Vec<String> = services_in_env
                .iter()
                .map(|s| format!("- {} (id: {})", s.node.name, s.node.id))
                .collect();
            let available = if available.is_empty() {
                "No services found in this environment.".to_string()
            } else {
                available.join("\n")
            };
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Available services in project '{}':\n{available}",
                project.name
            ))]));
        }

        let service = if let Some(sid) = &params.service_id {
            services_in_env
                .iter()
                .find(|s| s.node.id == *sid || s.node.name.eq_ignore_ascii_case(sid))
                .ok_or_else(|| {
                    McpError::invalid_params(
                        format!("Service '{sid}' not found in the current environment."),
                        None,
                    )
                })?
        } else {
            let name = params.service_name.as_ref().unwrap();
            services_in_env
                .iter()
                .find(|s| s.node.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    McpError::invalid_params(
                        format!("Service '{name}' not found in the current environment."),
                        None,
                    )
                })?
        };

        let mut configs = Configs::new()
            .map_err(|e| McpError::internal_error(format!("Failed to create config: {e}"), None))?;
        configs
            .link_service(service.node.id.clone())
            .map_err(|e| McpError::internal_error(format!("Failed to link service: {e}"), None))?;
        configs
            .write()
            .map_err(|e| McpError::internal_error(format!("Failed to write config: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Linked service '{}' (id: {})",
            service.node.name, service.node.id
        ))]))
    }

    #[tool(
        description = "Switch the linked environment for the current project directory. If no environment_id or environment_name is provided, lists available environments. Preserves the existing service link."
    )]
    async fn link_environment(
        &self,
        Parameters(params): Parameters<LinkEnvironmentParams>,
    ) -> Result<CallToolResult, McpError> {
        let local_linked = self.configs.get_local_linked_project().ok();
        let token_linked = self.configs.get_linked_project().await.ok();
        let linked = local_linked.or(token_linked);
        let project_id = linked.as_ref().map(|l| l.project.clone()).ok_or_else(|| {
            McpError::invalid_params(
                "No linked project. Run 'railway link' first or use link_service.",
                None,
            )
        })?;

        let project = get_project(&self.client, &self.configs, project_id.clone())
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to get project: {e}"), None))?;

        if params.environment_id.is_none() && params.environment_name.is_none() {
            let available = format_environments(&project);
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Available environments in project '{}':\n{available}",
                project.name
            ))]));
        }

        let env_identifier = params.environment_id.or(params.environment_name).unwrap();
        let environment =
            get_matched_environment(&project, env_identifier.clone()).map_err(|e| {
                let available = format_environments(&project);
                McpError::invalid_params(
                    format!("Environment not found: {e}. Available:\n{available}"),
                    None,
                )
            })?;

        // Clear service link on environment switch (matches CLI behavior in config.rs link_project)
        let mut configs = Configs::new()
            .map_err(|e| McpError::internal_error(format!("Failed to create config: {e}"), None))?;
        configs
            .link_project(
                project_id,
                Some(project.name.clone()),
                environment.id.clone(),
                Some(environment.name.clone()),
            )
            .map_err(|e| {
                McpError::internal_error(format!("Failed to link environment: {e}"), None)
            })?;
        configs
            .write()
            .map_err(|e| McpError::internal_error(format!("Failed to write config: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Linked environment '{}' (id: {}). Service link cleared — use link_service to re-link.",
            environment.name, environment.id
        ))]))
    }

    #[tool(
        description = "Create a new Railway project. Returns the project ID and the default environment ID."
    )]
    async fn create_project(
        &self,
        Parameters(params): Parameters<CreateProjectParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_create_project(params).await
    }

    #[tool(
        description = "Create a new environment in a Railway project. Optionally fork from an existing environment."
    )]
    async fn create_environment(
        &self,
        Parameters(params): Parameters<CreateEnvironmentParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_create_environment(params).await
    }

    #[tool(
        description = "Create a new service in a Railway project. Optionally connect a GitHub repo or Docker image."
    )]
    async fn create_service(
        &self,
        Parameters(params): Parameters<CreateServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_create_service(params).await
    }

    #[tool(
        description = "Connect an existing Railway service to a GitHub repo or Docker image. For GitHub repos, this enables Railway-managed deployment triggers when the project has GitHub App access."
    )]
    async fn connect_service_source(
        &self,
        Parameters(params): Parameters<ConnectServiceSourceParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_connect_service_source(params).await
    }

    #[tool(description = "Disconnect an existing Railway service from its current source.")]
    async fn disconnect_service_source(
        &self,
        Parameters(params): Parameters<ServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_disconnect_service_source(params).await
    }

    #[tool(
        description = "Remove a service from a Railway project. This is irreversible. Returns a preview first.",
        annotations(destructive_hint = true)
    )]
    async fn remove_service(
        &self,
        Parameters(params): Parameters<RemoveServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        if !params.confirm {
            return Ok(CallToolResult::success(vec![Content::text(
                "⚠️ This will permanently delete the service and all its deployments. Call again with confirm: true to proceed.",
            )]));
        }
        self.do_remove_service(params).await
    }

    #[tool(
        description = "Update service instance settings such as build command, start command, health check, sleep mode, root directory, cron schedule, Dockerfile path, restart policy, pre-deploy command, Railway config file, and watch patterns. Use scale_service for replicas and regions."
    )]
    async fn update_service(
        &self,
        Parameters(params): Parameters<UpdateServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_update_service(params).await
    }

    #[tool(
        description = "Scale one service across Railway deploy regions using friendly region names or region IDs. Provide replicas as a map, e.g. {\"eu-west\": 2, \"us-east\": 1}. Maximum 50 total replicas across regions."
    )]
    async fn scale_service(
        &self,
        Parameters(params): Parameters<ScaleServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_scale_service(params).await
    }

    #[tool(
        description = "Get the deployment status of all services in a Railway environment. Returns a table of service name, status, replica count, and latest deploy time."
    )]
    async fn environment_status(
        &self,
        Parameters(params): Parameters<EnvironmentStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_environment_status(params).await
    }

    #[tool(
        description = "Get the current configuration of a service instance including source, build config, start command, and variable count."
    )]
    async fn get_service_config(
        &self,
        Parameters(params): Parameters<GetServiceConfigParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_get_service_config(params).await
    }

    #[tool(
        description = "Set reference variables on a service. Each variable value must be a Railway reference expression starting with '${{' (e.g. '${{ Postgres.DATABASE_URL }}')."
    )]
    async fn add_reference_variable(
        &self,
        Parameters(params): Parameters<AddReferenceVariableParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_add_reference_variable(params).await
    }

    #[tool(
        description = "Deploy a Railway template by its code (e.g. 'postgres', 'redis'). Returns the workflow ID to track deployment progress."
    )]
    async fn deploy_template(
        &self,
        Parameters(params): Parameters<DeployTemplateParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_deploy_template(params).await
    }

    #[tool(
        description = "Search for Railway templates using Railway's backend-ranked template search. Returns matching templates with their codes."
    )]
    async fn search_templates(
        &self,
        Parameters(params): Parameters<SearchTemplatesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_search_templates(params).await
    }

    #[tool(
        description = "Get CPU and memory (or other) metrics for a service. Returns recent data points and average values for the specified time window."
    )]
    async fn service_metrics(
        &self,
        Parameters(params): Parameters<ServiceMetricsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_service_metrics(params).await
    }

    #[tool(
        description = "Get HTTP request counts grouped by status code bucket (2xx/3xx/4xx/5xx) from recent HTTP logs."
    )]
    async fn http_requests(
        &self,
        Parameters(params): Parameters<HttpObservabilityParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_http_requests(params).await
    }

    #[tool(
        description = "Get the HTTP error rate (4xx + 5xx) as a percentage of total requests from recent HTTP logs."
    )]
    async fn http_error_rate(
        &self,
        Parameters(params): Parameters<HttpObservabilityParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_http_error_rate(params).await
    }

    #[tool(
        description = "Get HTTP response time percentiles (p50/p90/p95/p99) in milliseconds from recent HTTP logs."
    )]
    async fn http_response_time(
        &self,
        Parameters(params): Parameters<HttpObservabilityParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_http_response_time(params).await
    }

    #[tool(
        description = "Create a new object storage bucket in a Railway environment. Default region is sjc. Returns the bucket ID and name."
    )]
    async fn create_bucket(
        &self,
        Parameters(params): Parameters<CreateBucketParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_create_bucket(params).await
    }

    #[tool(
        description = "Remove an object storage bucket from a Railway environment. This is irreversible. Returns a preview first.",
        annotations(destructive_hint = true)
    )]
    async fn remove_bucket(
        &self,
        Parameters(params): Parameters<RemoveBucketParams>,
    ) -> Result<CallToolResult, McpError> {
        if !params.confirm {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "⚠️ This will permanently delete bucket '{}'. Call again with confirm: true to proceed.",
                params.bucket_id
            ))]));
        }
        self.do_remove_bucket(params).await
    }

    #[tool(
        description = "Create a persistent volume and attach it to a service at the given mount path."
    )]
    async fn create_volume(
        &self,
        Parameters(params): Parameters<CreateVolumeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_create_volume(params).await
    }

    #[tool(
        description = "Update a volume's name or mount path. Provide environment_id and service_id when updating mount_path."
    )]
    async fn update_volume(
        &self,
        Parameters(params): Parameters<UpdateVolumeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_update_volume(params).await
    }

    #[tool(
        description = "Remove a persistent volume by ID. This is irreversible. Returns a preview first.",
        annotations(destructive_hint = true)
    )]
    async fn remove_volume(
        &self,
        Parameters(params): Parameters<RemoveVolumeParams>,
    ) -> Result<CallToolResult, McpError> {
        if !params.confirm {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "⚠️ This will permanently delete volume '{}'. Call again with confirm: true to proceed.",
                params.volume_id
            ))]));
        }
        self.do_remove_volume(params).await
    }

    #[tool(
        description = "Search Railway documentation by keyword. Returns a list of matching page URLs. Use docs_fetch to read the full content of a specific page."
    )]
    async fn docs_search(
        &self,
        Parameters(params): Parameters<DocsSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_docs_search(params).await
    }

    #[tool(
        description = "Fetch the full markdown content of a Railway documentation page. Accepts a docs URL (e.g. https://docs.railway.com/guides/getting-started) or a slug (e.g. guides/getting-started). Use docs_search first to find the right page."
    )]
    async fn docs_fetch(
        &self,
        Parameters(params): Parameters<DocsFetchParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_docs_fetch(params).await
    }

    #[tool(
        description = "Deploy code from a directory to Railway. Creates a tarball, uploads it, and starts a deployment. Returns the deployment ID and URLs. Use get_logs to monitor progress."
    )]
    async fn deploy(
        &self,
        Parameters(params): Parameters<DeployParams>,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_context(params.project_id, params.environment_id)
            .await?;

        let service_id = match &params.service_id {
            Some(sid) => {
                // Resolve service name to ID if needed
                let resolved = ctx
                    .project
                    .services
                    .edges
                    .iter()
                    .find(|s| s.node.id == *sid || s.node.name.eq_ignore_ascii_case(sid))
                    .map(|s| s.node.id.clone())
                    .ok_or_else(|| {
                        let available = format_services(&ctx.project);
                        McpError::invalid_params(
                            format!("Service '{sid}' not found. Available services:\n{available}"),
                            None,
                        )
                    })?;
                Some(resolved)
            }
            None => self
                .configs
                .get_local_linked_project()
                .ok()
                .and_then(|l| l.service)
                .or(ctx.linked.as_ref().and_then(|l| l.service.clone())),
        };

        let path = match &params.path {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir().map_err(|e| {
                McpError::internal_error(format!("Failed to get current directory: {e}"), None)
            })?,
        };

        let body = create_deploy_tarball(&path, &path, false, |_, _| {}).map_err(|e| {
            McpError::internal_error(format!("Failed to create deploy tarball: {e}"), None)
        })?;

        let hostname = self.configs.get_host();
        let response = upload_deploy_tarball(
            &self.client,
            hostname,
            &ctx.project_id,
            &ctx.environment_id,
            service_id.as_deref(),
            params.message.as_deref(),
            body,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to deploy: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Deployment started!\n  Deployment ID: {}\n  URL: {}\n  Build Logs: {}\n  Domain: {}\n\nUse get_logs with deployment_id '{}' to check build/deploy progress.",
            response.deployment_id,
            response.url,
            response.logs_url,
            response.deployment_domain,
            response.deployment_id,
        ))]))
    }
}

impl ServerHandler for RailwayMcp {
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        // Capture the client identity at the handshake — the earliest and
        // most reliable point, and the only one that covers sessions which
        // enumerate tools but never call one. Without this, those sessions'
        // `mcp_session` lifecycle event falls back to env/process-tree
        // detection and lands in `agent_unknown`. OnceLock keeps the first
        // (handshake) value, so a later tool call won't override it.
        telemetry::record_mcp_client(&telemetry::McpClientInfo {
            name: request.client_info.name.clone(),
        });

        // Preserve the default rmcp behavior: stash peer info for later
        // `peer_info()` reads, then return our server info.
        if context.peer.peer_info().is_none() {
            context.peer.set_peer_info(request);
        }
        Ok(self.get_info())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tool_name = request.name.to_string();
        let start = std::time::Instant::now();
        // Snapshot the JSON-RPC initialize clientInfo before consuming the
        // context — this is the authoritative agent identity for the entire
        // MCP path and overrides env/process-tree heuristics downstream.
        let mcp_client = context
            .peer
            .peer_info()
            .map(|info| telemetry::McpClientInfo {
                name: info.client_info.name.clone(),
            });
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let result = self.tool_router.call(tcc).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        telemetry::send_mcp_tool_with_client(
            tool_name,
            duration_ms,
            result.is_ok(),
            result.as_ref().err().map(|e| {
                let msg = format!("{e}");
                if msg.len() > 256 {
                    msg[..256].to_string()
                } else {
                    msg
                }
            }),
            mcp_client,
        )
        .await;

        result
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: self.tool_router.list_all(),
            meta: None,
            next_cursor: None,
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned()
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "railway".to_string(),
                title: None,
                version: env!("CARGO_PKG_VERSION").to_string(),
                description: None,
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Railway MCP server. Manage your Railway projects, services, deployments, and more.".to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_domain() -> McpDomainDetails {
        McpDomainDetails {
            id: "dom_123".to_string(),
            domain: "api.example.com".to_string(),
            kind: McpDomainKind::Custom,
            target_port: Some(3000),
            sync_status: "ACTIVE".to_string(),
            service_domain_suffix: None,
            environment_id: "env_123".to_string(),
            service_id: "svc_123".to_string(),
            dns_records: Vec::new(),
            verification: None,
            certificate: None,
        }
    }

    fn sample_service_domain() -> McpDomainDetails {
        let mut domain = sample_domain();
        domain.kind = McpDomainKind::Service;
        domain.domain = "api.up.railway.app".to_string();
        domain.service_domain_suffix = Some("up.railway.app".to_string());
        domain
    }

    #[test]
    fn mcp_domain_lookup_accepts_id_name_and_url() {
        let domains = vec![sample_domain()];

        assert!(find_mcp_domain(&domains, "dom_123").is_some());
        assert!(find_mcp_domain(&domains, "API.EXAMPLE.COM").is_some());
        assert!(find_mcp_domain(&domains, "https://api.example.com/").is_some());
        assert!(find_mcp_domain(&domains, "missing.example.com").is_none());
    }

    #[test]
    fn mcp_service_domain_input_accepts_full_domain_or_host_label() {
        let domain = sample_service_domain();

        assert_eq!(
            mcp_service_domain_input(&domain, "web.up.railway.app").unwrap(),
            "web.up.railway.app"
        );
        assert_eq!(
            mcp_service_domain_input(&domain, "web").unwrap(),
            "web.up.railway.app"
        );
        assert!(mcp_service_domain_input(&domain, "").is_err());
    }

    #[test]
    fn mcp_verification_txt_value_has_one_prefix() {
        assert_eq!(verification_txt_value("abc123"), "railway-verify=abc123");
        assert_eq!(
            verification_txt_value("railway-verify=abc123"),
            "railway-verify=abc123"
        );
        assert_eq!(
            verification_txt_value("railway-verify=railway-verify=abc123"),
            "railway-verify=abc123"
        );
    }
}
