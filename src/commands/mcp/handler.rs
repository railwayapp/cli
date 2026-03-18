use std::path::PathBuf;
use std::sync::Arc;

use super::params::*;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};

use crate::{
    client::post_graphql,
    config::Configs,
    controllers::{
        deployment::{FetchLogsParams, fetch_build_logs, fetch_deploy_logs, fetch_http_logs},
        environment::get_matched_environment,
        project::get_project,
        upload::{create_deploy_tarball, upload_deploy_tarball},
        user::get_user,
        variables::get_service_variables,
    },
    gql::{mutations, queries},
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
        let linked = self.configs.get_linked_project().await.ok();

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
            .or_else(|| linked.as_ref().map(|l| l.environment.clone()))
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
            None => ctx.linked.and_then(|l| l.service).ok_or_else(|| {
                let available = format_services(&ctx.project);
                McpError::invalid_params(
                    format!("No service_id provided and no linked service. Available services:\n{available}"),
                    None,
                )
            })?,
        };

        Ok(ResolvedServiceContext {
            project_id: ctx.project_id,
            environment_id: ctx.environment_id,
            service_id,
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

        if let Some(custom_domain) = &params.domain {
            // Custom domain flow: check availability then create
            let avail_vars = queries::custom_domain_available::Variables {
                domain: custom_domain.clone(),
            };
            let avail = post_graphql::<queries::CustomDomainAvailable, _>(
                &self.client,
                self.configs.get_backboard(),
                avail_vars,
            )
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to check domain availability: {e}"), None)
            })?;

            if !avail.custom_domain_available.available {
                let msg = &avail.custom_domain_available.message;
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "Domain '{custom_domain}' is not available: {msg}"
                ))]));
            }

            let create_vars = mutations::custom_domain_create::Variables {
                input: mutations::custom_domain_create::CustomDomainCreateInput {
                    domain: custom_domain.clone(),
                    environment_id: ctx.environment_id,
                    project_id: ctx.project_id,
                    service_id: ctx.service_id,
                    target_port: params.port,
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

            let domain_data = &result.custom_domain_create;
            let mut output = format!(
                "Custom domain created: {}\n\nDNS Records to configure:\n",
                domain_data.domain
            );
            for record in &domain_data.status.dns_records {
                output.push_str(&format!(
                    "  {} {} -> {}\n",
                    record.record_type, record.hostlabel, record.required_value
                ));
            }

            // Include TXT verification record if the domain is not yet verified
            if !domain_data.status.verified {
                if let (Some(host), Some(token)) = (
                    &domain_data.status.verification_dns_host,
                    &domain_data.status.verification_token,
                ) {
                    let zone = domain_data
                        .status
                        .dns_records
                        .first()
                        .map(|r| r.zone.as_str())
                        .unwrap_or("");
                    let host_label = host.strip_suffix(&format!(".{zone}")).unwrap_or(host);
                    output.push_str(&format!(
                        "\nVerification TXT record (required):\n  TXT {host_label} -> railway-verify={token}\n"
                    ));
                }
            }

            Ok(CallToolResult::success(vec![Content::text(output)]))
        } else {
            // Service domain flow: return existing or create new
            let domain_vars = queries::domains::Variables {
                environment_id: ctx.environment_id.clone(),
                project_id: ctx.project_id.clone(),
                service_id: ctx.service_id.clone(),
            };
            let existing = post_graphql::<queries::Domains, _>(
                &self.client,
                self.configs.get_backboard(),
                domain_vars,
            )
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to query domains: {e}"), None))?;

            let domains = &existing.domains;
            if !domains.service_domains.is_empty() || !domains.custom_domains.is_empty() {
                let mut output = String::from("Existing domains:\n");
                for sd in &domains.service_domains {
                    output.push_str(&format!("  Service domain: https://{}\n", sd.domain));
                }
                for cd in &domains.custom_domains {
                    output.push_str(&format!("  Custom domain: https://{}\n", cd.domain));
                }
                return Ok(CallToolResult::success(vec![Content::text(output)]));
            }

            // No domains exist — create a service domain
            let create_vars = mutations::service_domain_create::Variables {
                environment_id: ctx.environment_id,
                service_id: ctx.service_id,
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

            Ok(CallToolResult::success(vec![Content::text(format!(
                "Service domain created: https://{}",
                result.service_domain_create.domain
            ))]))
        }
    }

    #[tool(
        description = "Link a service to the current project directory for the CLI. If no service_id or service_name is provided, lists available services. Uses a fresh config to write the link."
    )]
    async fn link_service(
        &self,
        Parameters(params): Parameters<LinkServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        let linked = self.configs.get_linked_project().await.ok();
        let project_id = params
            .project_id
            .or_else(|| linked.as_ref().map(|l| l.project.clone()))
            .ok_or_else(|| {
                McpError::invalid_params(
                    "No project_id provided and no linked project. Run 'railway link' first.",
                    None,
                )
            })?;

        let project = get_project(&self.client, &self.configs, project_id)
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to get project: {e}"), None))?;

        // Filter to services present in the linked environment (matches CLI behavior)
        let environment_id = linked.as_ref().map(|l| l.environment.as_str());
        let env_service_ids = environment_id
            .and_then(|eid| project.environments.edges.iter().find(|e| e.node.id == eid))
            .map(|e| {
                e.node
                    .service_instances
                    .edges
                    .iter()
                    .map(|si| si.node.service_id.clone())
                    .collect::<std::collections::HashSet<String>>()
            });

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
        let linked = self.configs.get_linked_project().await.ok();
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
        description = "Remove a service from a Railway project. This is irreversible. Requires confirm: true.",
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
        description = "Update service instance settings such as build command, start command, replicas, health check path, sleep mode, and root directory."
    )]
    async fn update_service(
        &self,
        Parameters(params): Parameters<UpdateServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_update_service(params).await
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
        description = "Search for Railway templates by name or code. Returns the top 5 matching templates with their codes."
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
        description = "Remove an object storage bucket from a Railway environment. This is irreversible. Requires confirm: true.",
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
        description = "Remove a persistent volume by ID. This is irreversible. Requires confirm: true.",
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
        description = "Search the Railway documentation and return the content of the best matching page. Returns markdown content up to 8KB."
    )]
    async fn docs_search(
        &self,
        Parameters(params): Parameters<DocsSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        self.do_docs_search(params).await
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
                    .unwrap_or_else(|| sid.clone());
                Some(resolved)
            }
            None => self
                .configs
                .get_linked_project()
                .await
                .ok()
                .and_then(|l| l.service),
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

#[tool_handler]
impl ServerHandler for RailwayMcp {
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
