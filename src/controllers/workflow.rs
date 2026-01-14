use anyhow::{Result, bail};

use crate::{
    client::post_graphql,
    commands::{Configs, queries},
    util::retry::{RetryConfig, retry_with_backoff},
};

/// Waits for a workflow to complete by polling workflowStatus.
/// Returns Ok(()) on success, or an error with a user-friendly message on failure.
pub async fn wait_for_workflow(
    client: &reqwest::Client,
    configs: &Configs,
    workflow_id: String,
    display_name: &str,
) -> Result<()> {
    let client = client.clone();
    let backboard = configs.get_backboard();
    let display_name = display_name.to_string();

    retry_with_backoff(
        RetryConfig {
            max_attempts: 120, // ~2 minutes with 1s intervals
            initial_delay_ms: 1000,
            max_delay_ms: 2000,
            backoff_multiplier: 1.0,
            on_retry: None,
        },
        || {
            let client = client.clone();
            let backboard = backboard.clone();
            let display_name = display_name.clone();
            let workflow_id = workflow_id.clone();
            async move {
                let result = post_graphql::<queries::WorkflowStatus, _>(
                    &client,
                    backboard,
                    queries::workflow_status::Variables { workflow_id },
                )
                .await?;

                use queries::workflow_status::WorkflowStatus;
                match result.workflow_status.status {
                    WorkflowStatus::Complete => Ok(()),
                    WorkflowStatus::Error => {
                        let error_msg = result
                            .workflow_status
                            .error
                            .filter(|e| !e.is_empty())
                            .unwrap_or_else(|| "Unknown error".to_string());
                        bail!("Failed to add {display_name}: {error_msg}")
                    }
                    WorkflowStatus::NotFound => {
                        bail!("Failed to add {display_name}")
                    }
                    WorkflowStatus::Running | WorkflowStatus::Other(_) => {
                        bail!("still deploying")
                    }
                }
            }
        },
    )
    .await
}
