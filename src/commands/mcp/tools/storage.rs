use std::collections::BTreeMap;

use rmcp::{ErrorData as McpError, model::*};

use crate::{
    client::post_graphql,
    controllers::{
        config::{BucketInstance, EnvironmentConfig},
        regions::BucketRegion,
    },
    gql::mutations,
};

use super::super::handler::{RailwayMcp, ResolvedContext};
use super::super::params::{
    CreateBucketParams, CreateVolumeParams, RemoveBucketParams, RemoveVolumeParams,
    UpdateVolumeParams,
};

impl RailwayMcp {
    /// Stage an environment config patch into the staged-changes workflow
    /// (matching the dashboard). Applied via staged_changes_deploy or
    /// `railway changes deploy`.
    pub(crate) async fn stage_env_patch(
        &self,
        ctx: &ResolvedContext,
        patch: EnvironmentConfig,
    ) -> Result<(), McpError> {
        post_graphql::<mutations::EnvironmentStageChanges, _>(
            &self.client,
            self.configs.get_backboard(),
            mutations::environment_stage_changes::Variables {
                environment_id: ctx.environment_id.clone(),
                input: patch,
                merge: Some(true),
            },
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to stage changes: {e}"), None))?;
        Ok(())
    }

    /// Commit an environment config patch immediately, bypassing the
    /// staged-changes workflow. Used for operations that are documented to
    /// apply right away (e.g. scaling).
    pub(crate) async fn commit_env_patch(
        &self,
        ctx: &ResolvedContext,
        patch: EnvironmentConfig,
        commit_message: Option<String>,
    ) -> Result<(), McpError> {
        post_graphql::<mutations::EnvironmentPatchCommit, _>(
            &self.client,
            self.configs.get_backboard(),
            mutations::environment_patch_commit::Variables {
                environment_id: ctx.environment_id.clone(),
                patch,
                commit_message,
            },
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to commit changes: {e}"), None))?;
        Ok(())
    }

    pub(crate) async fn do_create_bucket(
        &self,
        params: CreateBucketParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_context(params.project_id, params.environment_id)
            .await?;

        let region = match params.region.as_deref() {
            Some(region) => BucketRegion::parse(region)
                .map_err(|e| McpError::invalid_params(e.to_string(), None))?,
            None => BucketRegion::Sjc,
        };

        let create_vars = mutations::bucket_create::Variables {
            input: mutations::bucket_create::BucketCreateInput {
                project_id: ctx.project_id.clone(),
                name: params.name,
                environment_id: None,
            },
        };

        let create_resp = post_graphql::<mutations::BucketCreate, _>(
            &self.client,
            self.configs.get_backboard(),
            create_vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to create bucket: {e}"), None))?;

        let bucket = &create_resp.bucket_create;

        let mut buckets = BTreeMap::new();
        buckets.insert(
            bucket.id.clone(),
            BucketInstance {
                region: Some(region.code().to_string()),
                is_created: Some(true),
                is_deleted: None,
            },
        );

        let patch = EnvironmentConfig {
            buckets,
            ..Default::default()
        };

        self.stage_env_patch(&ctx, patch).await.map_err(|e| {
            McpError::internal_error(
                format!(
                    "Bucket '{}' (id: {}) was created in the project but could not be \
                     staged for the environment: {e}. You can complete the deployment \
                     manually from the Railway dashboard.",
                    bucket.name, bucket.id
                ),
                None,
            )
        })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Bucket created: {} (id: {}, region: {}) — staged (call staged_changes_deploy or run `railway changes deploy` to provision it)",
            bucket.name, bucket.id, region
        ))]))
    }

    pub(crate) async fn do_remove_bucket(
        &self,
        params: RemoveBucketParams,
    ) -> Result<CallToolResult, McpError> {
        let ctx = self
            .resolve_context(params.project_id, params.environment_id)
            .await?;

        let mut buckets = BTreeMap::new();
        buckets.insert(
            params.bucket_id.clone(),
            BucketInstance {
                is_deleted: Some(true),
                region: None,
                is_created: None,
            },
        );

        let patch = EnvironmentConfig {
            buckets,
            ..Default::default()
        };

        self.stage_env_patch(&ctx, patch)
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to remove bucket: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Bucket {} staged for removal (call staged_changes_deploy or run `railway changes deploy` to apply it).",
            params.bucket_id
        ))]))
    }

    pub(crate) async fn do_create_volume(
        &self,
        params: CreateVolumeParams,
    ) -> Result<CallToolResult, McpError> {
        if !params.mount_path.starts_with('/') {
            return Err(McpError::invalid_params(
                "Mount path must start with a '/'.",
                None,
            ));
        }

        let ctx = self
            .resolve_service_context(params.project_id, params.service_id, params.environment_id)
            .await?;

        let vars = mutations::volume_create::Variables {
            project_id: ctx.project_id,
            environment_id: ctx.environment_id,
            service_id: ctx.service_id,
            mount_path: params.mount_path,
        };

        let resp = post_graphql::<mutations::VolumeCreate, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to create volume: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Volume created: {} (id: {})",
            resp.volume_create.name, resp.volume_create.id
        ))]))
    }

    pub(crate) async fn do_update_volume(
        &self,
        params: UpdateVolumeParams,
    ) -> Result<CallToolResult, McpError> {
        let mut updated = Vec::new();

        if let Some(name) = params.name {
            let vars = mutations::volume_name_update::Variables {
                volume_id: params.volume_id.clone(),
                name,
            };
            post_graphql::<mutations::VolumeNameUpdate, _>(
                &self.client,
                self.configs.get_backboard(),
                vars,
            )
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to update volume name: {e}"), None)
            })?;
            updated.push("name");
        }

        if let Some(mount_path) = params.mount_path {
            if !mount_path.starts_with('/') {
                return Err(McpError::invalid_params(
                    "Mount path must start with a '/'.",
                    None,
                ));
            }
            let linked = self.configs.get_linked_project().await.ok();
            let environment_id = params
                .environment_id
                .or_else(|| linked.as_ref().and_then(|l| l.environment.clone()))
                .ok_or_else(|| {
                    McpError::invalid_params(
                        "environment_id is required when updating mount_path.",
                        None,
                    )
                })?;

            let vars = mutations::volume_mount_path_update::Variables {
                volume_id: params.volume_id.clone(),
                environment_id,
                service_id: params.service_id,
                mount_path,
            };
            post_graphql::<mutations::VolumeMountPathUpdate, _>(
                &self.client,
                self.configs.get_backboard(),
                vars,
            )
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to update mount path: {e}"), None)
            })?;
            updated.push("mount_path");
        }

        if updated.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No updates provided. Specify name and/or mount_path.".to_string(),
            )]));
        }

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Volume {} updated: {}",
            params.volume_id,
            updated.join(", ")
        ))]))
    }

    pub(crate) async fn do_remove_volume(
        &self,
        params: RemoveVolumeParams,
    ) -> Result<CallToolResult, McpError> {
        let vars = mutations::volume_delete::Variables {
            id: params.volume_id,
        };

        post_graphql::<mutations::VolumeDelete, _>(
            &self.client,
            self.configs.get_backboard(),
            vars,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to remove volume: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(
            "Volume removed successfully.".to_string(),
        )]))
    }
}
