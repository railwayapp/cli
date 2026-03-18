use rmcp::{ErrorData as McpError, model::*};

use crate::{client::post_graphql, gql::mutations};

use super::super::handler::RailwayMcp;
use super::super::params::{
    CreateEnvironmentParams, CreateProjectParams, CreateServiceParams, EnvironmentStatusParams,
    RemoveServiceParams, UpdateServiceParams,
};

impl RailwayMcp {
    pub(crate) async fn do_create_project(
        &self,
        params: CreateProjectParams,
    ) -> Result<CallToolResult, McpError> {
        let vars = mutations::project_create::Variables {
            name: Some(params.name),
            description: params.description,
            workspace_id: params.workspace_id,
        };

        let result = post_graphql::<mutations::ProjectCreate, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to create project: {e}"), None))?;

        let project = &result.project_create;
        let env_info: Vec<String> = project
            .environments
            .edges
            .iter()
            .map(|e| format!("{} (id: {})", e.node.name, e.node.id))
            .collect();

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Project created: {} (id: {})\nEnvironments: {}",
            project.name,
            project.id,
            if env_info.is_empty() {
                "none".to_string()
            } else {
                env_info.join(", ")
            }
        ))]))
    }

    pub(crate) async fn do_create_environment(
        &self,
        params: CreateEnvironmentParams,
    ) -> Result<CallToolResult, McpError> {
        let linked = self.configs.get_linked_project().await.ok();
        let project_id = params
            .project_id
            .or_else(|| linked.map(|l| l.project))
            .ok_or_else(|| {
                McpError::invalid_params(
                    "No project_id provided and no linked project. Run 'railway link' or pass a project_id.",
                    None,
                )
            })?;

        let vars = mutations::environment_create::Variables {
            project_id,
            name: params.name,
            source_id: params.source_environment_id,
            apply_changes_in_background: None,
        };

        let result = post_graphql::<mutations::EnvironmentCreate, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await
        .map_err(|e| {
            McpError::internal_error(format!("Failed to create environment: {e}"), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Environment created: {} (id: {})",
            result.environment_create.name, result.environment_create.id
        ))]))
    }

    pub(crate) async fn do_create_service(
        &self,
        params: CreateServiceParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_context(params.project_id, params.environment_id)
            .await?;

        let source = if params.source_image.is_some() || params.source_repo.is_some() {
            Some(mutations::service_create::ServiceSourceInput {
                image: params.source_image,
                repo: params.source_repo,
            })
        } else {
            None
        };

        let vars = mutations::service_create::Variables {
            name: params.name,
            project_id: ctx.project_id,
            environment_id: ctx.environment_id,
            source,
            branch: None,
            variables: None,
        };

        let result = post_graphql::<mutations::ServiceCreate, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to create service: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Service created: {} (id: {})",
            result.service_create.name, result.service_create.id
        ))]))
    }

    pub(crate) async fn do_remove_service(
        &self,
        params: RemoveServiceParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;

        let vars = mutations::service_delete::Variables {
            environment_id: ctx.environment_id,
            service_id: ctx.service_id,
        };

        post_graphql::<mutations::ServiceDelete, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to remove service: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(
            "Service removed successfully.".to_string(),
        )]))
    }

    pub(crate) async fn do_update_service(
        &self,
        params: UpdateServiceParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;

        let input = mutations::service_instance_update::ServiceInstanceUpdateInput {
            build_command: params.build_command,
            start_command: params.start_command,
            num_replicas: params.num_replicas,
            healthcheck_path: params.health_check_path,
            sleep_application: params.sleep_application,
            root_directory: params.root_directory,
            ..Default::default()
        };

        let vars = mutations::service_instance_update::Variables {
            service_id: ctx.service_id,
            environment_id: Some(ctx.environment_id),
            input,
        };

        let result = post_graphql::<mutations::ServiceInstanceUpdate, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to update service: {e}"), None))?;

        if result.service_instance_update {
            Ok(CallToolResult::success(vec![Content::text(
                "Service updated successfully.".to_string(),
            )]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(
                "Service settings are already up to date (no changes applied).".to_string(),
            )]))
        }
    }

    pub(crate) async fn do_environment_status(
        &self,
        params: EnvironmentStatusParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_context(params.project_id, params.environment_id)
            .await?;

        // Find the environment in the project
        let env = ctx
            .project
            .environments
            .edges
            .iter()
            .find(|e| e.node.id == ctx.environment_id)
            .ok_or_else(|| {
                McpError::internal_error("Environment not found in project data.".to_string(), None)
            })?;

        let mut output = format!("## Environment: {} ({})\n\n", env.node.name, env.node.id);
        output.push_str("Service | Status | Active Deployments | Latest Deploy\n");
        output.push_str("--------|--------|-------------------|---------------\n");

        for si_edge in &env.node.service_instances.edges {
            let node = &si_edge.node;
            let status = node
                .latest_deployment
                .as_ref()
                .map(|d| format!("{:?}", d.status))
                .unwrap_or_else(|| "No deployment".to_string());
            let active = node.active_deployments.len();
            let deploy_time = node
                .latest_deployment
                .as_ref()
                .map(|d| d.created_at.to_string())
                .unwrap_or_else(|| "-".to_string());

            output.push_str(&format!(
                "{} | {} | {} | {}\n",
                node.service_name, status, active, deploy_time
            ));
        }

        if env.node.service_instances.edges.is_empty() {
            output.push_str("No services in this environment.\n");
        }

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }
}
