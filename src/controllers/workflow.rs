use anyhow::Result;
use thiserror::Error;

use crate::{
    client::post_graphql,
    commands::{Configs, queries},
    util::retry::{RetryConfig, retry_with_backoff},
};

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("workflow failed: {0}")]
    Failed(String),
    #[error("workflow not found")]
    NotFound,
    #[error("workflow timed out")]
    Timeout,
}

/// Waits for a workflow to complete by polling workflowStatus.
pub async fn wait_for_workflow(
    client: &reqwest::Client,
    configs: &Configs,
    workflow_id: String,
) -> Result<(), WorkflowError> {
    let backboard = configs.get_backboard();

    let result = retry_with_backoff(
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
                        Err(WorkflowError::Failed(error_msg).into())
                    }
                    WorkflowStatus::NotFound => Err(WorkflowError::NotFound.into()),
                    WorkflowStatus::Running | WorkflowStatus::Other(_) => {
                        Err(WorkflowError::Timeout.into())
                    }
                }
            }
        },
    )
    .await;

    result.map_err(|e| {
        e.downcast::<WorkflowError>()
            .unwrap_or_else(|e| WorkflowError::Failed(e.to_string()))
    })
}
