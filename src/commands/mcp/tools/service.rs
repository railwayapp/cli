use std::collections::{BTreeMap, HashMap};

use rmcp::{ErrorData as McpError, model::*};
use serde_json::{Map, Value};

use crate::{
    client::{GQLClient, post_graphql},
    controllers::{
        config::environment::fetch_environment_config,
        project::{find_service_instance, get_environment_instances},
        regions::{
            build_multi_region_patch, convert_hashmap_to_map, fetch_regions_for_project,
            format_region_replicas, merge_config, region_data_from_deployment_meta,
            region_locations_from_regions, resolve_deploy_region_id_for_scale,
            validate_total_replicas,
        },
    },
    gql::{mutations, queries},
};

use super::super::handler::RailwayMcp;
use super::super::params::{
    AddReferenceVariableParams, DeployTemplateParams, GetServiceConfigParams, ScaleServiceParams,
    SearchTemplatesParams,
};
use super::storage::PatchMode;

impl RailwayMcp {
    pub(crate) async fn do_scale_service(
        &self,
        params: ScaleServiceParams,
    ) -> Result<CallToolResult, McpError> {
        if params.replicas.is_empty() {
            return Err(McpError::invalid_params(
                "replicas must include at least one region assignment, e.g. {\"eu-west\": 2}.",
                None,
            ));
        }

        let service_ctx = self
            .resolve_service_context(
                params.project_id.clone(),
                params.service_id.clone(),
                params.environment_id.clone(),
            )
            .await?;
        let ctx = self
            .resolve_context(params.project_id, params.environment_id)
            .await?;
        let regions =
            fetch_regions_for_project(&self.client, &self.configs, Some(&service_ctx.project_id))
                .await
                .map_err(|e| {
                    McpError::internal_error(format!("Failed to fetch regions: {e}"), None)
                })?;
        let config_resp = fetch_environment_config(
            &self.client,
            &self.configs,
            &service_ctx.environment_id,
            false,
        )
        .await
        .map_err(|e| {
            McpError::internal_error(format!("Failed to fetch environment config: {e}"), None)
        })?;
        let environment_instances = get_environment_instances(
            &self.client,
            &self.configs,
            &service_ctx.project_id,
            &service_ctx.environment_id,
        )
        .await
        .map_err(|e| {
            McpError::internal_error(format!("Failed to fetch environment instances: {e}"), None)
        })?;
        let existing_from_deployment =
            find_service_instance(&environment_instances, &service_ctx.service_id)
                .ok_or_else(|| {
                    McpError::invalid_params(
                        "Service not found in the selected environment.".to_string(),
                        None,
                    )
                })?
                .latest_deployment
                .as_ref()
                .and_then(|deployment| deployment.meta.as_ref())
                .and_then(|meta| region_data_from_deployment_meta(meta).ok().flatten());
        let existing_from_config = config_resp
            .config
            .services
            .get(&service_ctx.service_id)
            .and_then(|service| service.deploy.as_ref())
            .and_then(|deploy| deploy.multi_region_config.as_ref())
            .map(|config| serde_json::to_value(config).unwrap_or(Value::Object(Map::new())))
            .unwrap_or_else(|| Value::Object(Map::new()));
        let existing = existing_from_deployment.unwrap_or(existing_from_config);

        let mut requested = HashMap::new();
        for (region_input, replicas) in params.replicas {
            if replicas < 0 {
                return Err(McpError::invalid_params(
                    format!("Replica count for region \"{region_input}\" must be zero or greater."),
                    None,
                ));
            }

            let region_id = resolve_deploy_region_id_for_scale(
                &regions,
                &region_input,
                replicas as u64,
                &existing,
            )
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
            if requested.insert(region_id, replicas as u64).is_some() {
                return Err(McpError::invalid_params(
                    format!("Region \"{region_input}\" was specified more than once."),
                    None,
                ));
            }
        }

        let region_data = merge_config(existing, convert_hashmap_to_map(requested));
        validate_total_replicas(&region_data)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        let patch =
            build_multi_region_patch(&service_ctx.service_id, &region_data).map_err(|e| {
                McpError::internal_error(format!("Failed to build scale patch: {e}"), None)
            })?;

        let service_name = ctx
            .project
            .services
            .edges
            .iter()
            .find(|service| service.node.id == service_ctx.service_id)
            .map(|service| service.node.name.as_str())
            .unwrap_or(&service_ctx.service_id);
        let mode = self
            .apply_env_patch(&ctx, patch, Some(format!("Scale service {service_name}")))
            .await?;
        let status = match mode {
            PatchMode::Commit => "committed",
            PatchMode::Stage => {
                "staged (environment has pending changes; use `railway environment edit` to commit)"
            }
        };
        let region_locations = region_locations_from_regions(&regions.regions);
        let region_summary = format_region_replicas(&region_data, &region_locations);

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Service scaled: {service_name} (id: {})\nEnvironment: {}\nRegions: {region_summary}\nChange: {status}",
            service_ctx.service_id, config_resp.name
        ))]))
    }

    pub(crate) async fn do_get_service_config(
        &self,
        params: GetServiceConfigParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;

        let config_resp =
            fetch_environment_config(&self.client, &self.configs, &ctx.environment_id, false)
                .await
                .map_err(|e| {
                    McpError::internal_error(
                        format!("Failed to fetch environment config: {e}"),
                        None,
                    )
                })?;

        let config = &config_resp.config;
        let service_config = config.services.get(&ctx.service_id);

        let mut output = format!(
            "## Service Config (id: {})\nEnvironment: {}\n\n",
            ctx.service_id, config_resp.name
        );

        if let Some(svc) = service_config {
            if let Some(source) = &svc.source {
                if let Some(repo) = &source.repo {
                    output.push_str(&format!("Source repo: {repo}\n"));
                }
                if let Some(image) = &source.image {
                    output.push_str(&format!("Source image: {image}\n"));
                }
                if let Some(root) = &source.root_directory {
                    output.push_str(&format!("Root directory: {root}\n"));
                }
            }
            if let Some(build) = &svc.build {
                if let Some(cmd) = &build.build_command {
                    output.push_str(&format!("Build command: {cmd}\n"));
                }
                if let Some(builder) = &build.builder {
                    output.push_str(&format!("Builder: {builder}\n"));
                }
            }
            if let Some(deploy) = &svc.deploy {
                if let Some(cmd) = &deploy.start_command {
                    output.push_str(&format!("Start command: {cmd}\n"));
                }
                if let Some(replicas) = deploy.num_replicas {
                    output.push_str(&format!("Replicas: {replicas}\n"));
                }
                if let Some(sleep) = deploy.sleep_application {
                    output.push_str(&format!("Sleep when inactive: {sleep}\n"));
                }
                if let Some(hc) = &deploy.healthcheck_path {
                    output.push_str(&format!("Health check path: {hc}\n"));
                }
            }
            output.push_str(&format!("Variables defined: {}\n", svc.variables.len()));
        } else {
            output.push_str(
                "Service not found in environment config (may have no instance-level config).\n",
            );
        }

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    pub(crate) async fn do_add_reference_variable(
        &self,
        params: AddReferenceVariableParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;

        for var in &params.variables {
            if !var.value.starts_with("${{") {
                return Err(McpError::invalid_params(
                    format!(
                        "Variable '{}' value must be a reference expression starting with '${{{{', got: '{}'",
                        var.name, var.value
                    ),
                    None,
                ));
            }
        }

        let names: Vec<String> = params.variables.iter().map(|v| v.name.clone()).collect();
        let variables: BTreeMap<String, String> = params
            .variables
            .into_iter()
            .map(|v| (v.name, v.value))
            .collect();

        let vars = mutations::variable_collection_upsert::Variables {
            project_id: ctx.project_id,
            service_id: ctx.service_id,
            environment_id: ctx.environment_id,
            variables,
            skip_deploys: None,
        };

        post_graphql::<mutations::VariableCollectionUpsert, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await
        .map_err(|e| {
            McpError::internal_error(format!("Failed to set reference variables: {e}"), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Reference variable(s) set: {}",
            names.join(", ")
        ))]))
    }

    pub(crate) async fn do_deploy_template(
        &self,
        params: DeployTemplateParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_context(params.project_id, params.environment_id)
            .await?;
        let public_client = GQLClient::new_public().map_err(|e| {
            McpError::internal_error(format!("Failed to create template client: {e}"), None)
        })?;

        let template_vars = queries::template_detail::Variables {
            code: params.template_code,
        };

        let template_resp = post_graphql::<queries::TemplateDetail, _>(
            &public_client,
            self.configs.get_backboard(),
            template_vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to fetch template: {e}"), None))?;

        let template = template_resp.template;
        let serialized_config = template
            .serialized_config
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));

        let deploy_vars = mutations::template_deploy::Variables {
            project_id: ctx.project_id,
            environment_id: ctx.environment_id,
            template_id: template.id,
            serialized_config,
        };

        let deploy_resp = post_graphql::<mutations::TemplateDeploy, _>(
            &self.client,
            self.configs.get_backboard(),
            deploy_vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to deploy template: {e}"), None))?;

        let result = &deploy_resp.template_deploy_v2;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Template '{}' deployed.\nProject ID: {}\nWorkflow ID: {}",
            template.name,
            result.project_id,
            result.workflow_id.as_deref().unwrap_or("unknown")
        ))]))
    }

    pub(crate) async fn do_search_templates(
        &self,
        params: SearchTemplatesParams,
    ) -> Result<CallToolResult, McpError> {
        let public_client = GQLClient::new_public().map_err(|e| {
            McpError::internal_error(format!("Failed to create template client: {e}"), None)
        })?;
        let vars = queries::template_search::Variables {
            query: params.query.clone(),
            first: Some(params.limit.unwrap_or(5).clamp(1, 50)),
            after: params.after,
            verified: params.verified,
            category: params.category,
        };

        let resp = post_graphql::<queries::TemplateSearch, _>(
            &public_client,
            self.configs.get_backboard(),
            vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to search templates: {e}"), None))?;

        if resp.template_search.edges.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No templates found matching '{}'.",
                params.query
            ))]));
        }

        let output = resp
            .template_search
            .edges
            .iter()
            .map(|template| {
                let template = &template.node;
                let description = template.description.as_deref().unwrap_or("No description");
                format!(
                    "- {} (code: {}) - {}",
                    template.name, template.code, description
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let next_page = if resp.template_search.page_info.has_next_page {
            resp.template_search
                .page_info
                .end_cursor
                .as_deref()
                .map(|cursor| format!("\n\nNext page cursor: {cursor}"))
                .unwrap_or_default()
        } else {
            String::new()
        };

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Templates matching '{}':\n{}{}\n\nUse deploy_template with the code to deploy.",
            params.query, output, next_page
        ))]))
    }
}
