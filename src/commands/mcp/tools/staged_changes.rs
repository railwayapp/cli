use rmcp::{ErrorData as McpError, model::*};
use serde_json::json;

use crate::{
    commands::Configs,
    controllers::staged_changes::{
        DeployWaitResult, EnvironmentContext, deploy_staged_changes, discard_all_staged_changes,
        discard_staged_change_paths, is_empty_patch, load_staged_changes, mask_view_values,
        output_json, patch_requires_two_factor,
    },
};

use super::super::handler::RailwayMcp;
use super::super::params::{
    DeployStagedChangesParams, DiscardStagedChangesParams, StagedChangesParams,
};

/// Mirrors backboard's own agent-tool policy: token-authenticated callers
/// cannot complete 2FA, so a 2FA-requiring commit is refused up front instead
/// of silently bypassing verification.
const TWO_FACTOR_TOKEN_REFUSAL: &str = "These staged changes require two-factor verification, which isn't available over an API/MCP token. Apply them from the Railway dashboard.";

impl RailwayMcp {
    pub(crate) async fn do_staged_changes_status(
        &self,
        params: StagedChangesParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_staged_changes_context(params.project_id, params.environment_id)
            .await?;
        let view = load_staged_changes(&ctx).await.map_err(|e| {
            McpError::internal_error(format!("Failed to load staged changes: {e}"), None)
        })?;
        let view = if params.show_values.unwrap_or(false) {
            view
        } else {
            mask_view_values(&view)
        };
        let output = serde_json::to_string_pretty(&output_json(&view)).map_err(|e| {
            McpError::internal_error(format!("Failed to serialize staged changes: {e}"), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    pub(crate) async fn do_staged_changes_deploy(
        &self,
        params: DeployStagedChangesParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_staged_changes_context(params.project_id, params.environment_id)
            .await?;
        let view = load_staged_changes(&ctx).await.map_err(|e| {
            McpError::internal_error(format!("Failed to load staged changes: {e}"), None)
        })?;

        if is_empty_patch(&view.patch.patch) {
            let output = json!({
                "deployed": false,
                "environmentId": ctx.environment_id,
                "environmentName": ctx.environment_name,
                "message": "No staged changes to deploy",
            });
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string()),
            )]));
        }

        if view.patch.status == "APPLYING" {
            return Err(McpError::invalid_request(
                "Staged changes are currently being applied. Check progress with staged_changes_status.",
                None,
            ));
        }

        if patch_requires_two_factor(&view.patch.patch, &view.current_config)
            && Configs::is_using_token_auth()
        {
            return Err(McpError::invalid_request(TWO_FACTOR_TOKEN_REFUSAL, None));
        }

        let outcome = deploy_staged_changes(&ctx, params.message.clone(), params.skip_deploys)
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to deploy staged changes: {e}"), None)
            })?;

        let output = match outcome.wait {
            DeployWaitResult::Committed => json!({
                "deployed": true,
                "environmentId": ctx.environment_id,
                "environmentName": ctx.environment_name,
                "workflowId": outcome.workflow_id,
                "message": params.message,
                "skipDeploys": params.skip_deploys.unwrap_or(false),
            }),
            // Commit accepted; only progress reporting timed out. Not an error.
            DeployWaitResult::Pending => json!({
                "deployed": false,
                "pending": true,
                "environmentId": ctx.environment_id,
                "environmentName": ctx.environment_name,
                "workflowId": outcome.workflow_id,
                "message": "Commit accepted, changes are still applying",
                "next": ["staged_changes_status"],
            }),
            DeployWaitResult::Failed(detail) => {
                return Err(McpError::internal_error(
                    format!("Failed to deploy staged changes: {detail}"),
                    None,
                ));
            }
        };

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string()),
        )]))
    }

    pub(crate) async fn do_staged_changes_discard(
        &self,
        params: DiscardStagedChangesParams,
    ) -> Result<CallToolResult, McpError> {
        let all = params.all.unwrap_or(false);
        let paths = params.paths.unwrap_or_default();
        if all && !paths.is_empty() {
            return Err(McpError::invalid_params(
                "Pass either all: true or paths, not both.",
                None,
            ));
        }
        if !all && paths.is_empty() {
            return Err(McpError::invalid_params(
                "Pass all: true to discard everything, or paths to discard selected staged changes.",
                None,
            ));
        }

        let ctx = self
            .resolve_staged_changes_context(params.project_id, params.environment_id)
            .await?;
        let view = load_staged_changes(&ctx).await.map_err(|e| {
            McpError::internal_error(format!("Failed to load staged changes: {e}"), None)
        })?;

        if is_empty_patch(&view.patch.patch) {
            let output = json!({
                "discarded": false,
                "environmentId": ctx.environment_id,
                "environmentName": ctx.environment_name,
                "message": "No staged changes to discard",
            });
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string()),
            )]));
        }

        let output = if all {
            let updated = discard_all_staged_changes(&ctx).await.map_err(|e| {
                McpError::internal_error(format!("Failed to discard all staged changes: {e}"), None)
            })?;
            json!({
                "discarded": true,
                "discardedChanges": view.pretty.total_changes,
                "environmentId": ctx.environment_id,
                "environmentName": ctx.environment_name,
                "patchId": updated.id,
                "status": updated.status,
            })
        } else {
            let (updated, discarded) =
                discard_staged_change_paths(&ctx, &paths)
                    .await
                    .map_err(|e| {
                        McpError::internal_error(
                            format!("Failed to discard selected staged changes: {e}"),
                            None,
                        )
                    })?;
            json!({
                "discarded": true,
                "discardedChanges": discarded,
                "paths": paths,
                "environmentId": ctx.environment_id,
                "environmentName": ctx.environment_name,
                "patchId": updated.id,
                "status": updated.status,
            })
        };

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string()),
        )]))
    }

    async fn resolve_staged_changes_context(
        &self,
        project_id: Option<String>,
        environment_id: Option<String>,
    ) -> Result<EnvironmentContext, McpError> {
        let ctx = self.resolve_context(project_id, environment_id).await?;
        let environment_name = ctx
            .project
            .environments
            .edges
            .iter()
            .find(|edge| edge.node.id == ctx.environment_id)
            .map(|edge| edge.node.name.clone())
            .unwrap_or_else(|| ctx.environment_id.clone());

        Ok(EnvironmentContext {
            client: self.client.clone(),
            configs: self.configs.clone(),
            project: ctx.project,
            project_id: ctx.project_id,
            environment_id: ctx.environment_id,
            environment_name,
        })
    }
}
