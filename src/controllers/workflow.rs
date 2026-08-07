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

/// One poll's outcome that should stop the retry loop even though the
/// overall wait is a failure -- `retry_with_backoff` retries every `Err`,
/// so terminal outcomes must travel through the `Ok` channel.
enum TerminalPoll {
    Complete,
    Failed(String),
}

/// Waits for a workflow to complete by polling workflowStatus.
///
/// `Running` (and unknown statuses) keep polling, and so does `NotFound` --
/// tolerating read-after-write lag right after the workflow was created.
/// `Error` is terminal: a workflow that has already failed never un-fails,
/// so it surfaces immediately instead of burning the full ~2-minute retry
/// budget re-polling a lost cause.
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
                    WorkflowStatus::Complete => Ok(TerminalPoll::Complete),
                    WorkflowStatus::Error => {
                        let error_msg = result
                            .workflow_status
                            .error
                            .filter(|e| !e.is_empty())
                            .unwrap_or_else(|| "Unknown error".to_string());
                        Ok(TerminalPoll::Failed(error_msg))
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

    match result {
        Ok(TerminalPoll::Complete) => Ok(()),
        Ok(TerminalPoll::Failed(message)) => Err(WorkflowError::Failed(message)),
        Err(e) => Err(e
            .downcast::<WorkflowError>()
            .unwrap_or_else(|e| WorkflowError::Failed(e.to_string()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MockBackboard;
    use serde_json::json;

    fn status(value: &str, error: Option<&str>) -> serde_json::Value {
        json!({ "workflowStatus": { "status": value, "error": error } })
    }

    #[tokio::test]
    async fn complete_on_first_poll_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub("WorkflowStatus", status("Complete", None));

        let configs = server.configs(&dir);
        let client = reqwest::Client::new();
        wait_for_workflow(&client, &configs, "wf-1".to_string())
            .await
            .unwrap();

        assert_eq!(server.hits(), 1);
        assert_eq!(
            server.variables_for("WorkflowStatus"),
            vec![json!({ "workflowId": "wf-1" })]
        );
    }

    #[tokio::test]
    async fn failed_workflow_surfaces_immediately_with_its_message() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub(
            "WorkflowStatus",
            status("Error", Some("volume detach failed")),
        );

        let configs = server.configs(&dir);
        let client = reqwest::Client::new();
        let err = wait_for_workflow(&client, &configs, "wf-1".to_string())
            .await
            .unwrap_err();

        assert!(matches!(err, WorkflowError::Failed(ref m) if m == "volume detach failed"));
        // Terminal: exactly one poll, not the full ~2-minute retry budget.
        assert_eq!(server.hits(), 1);
    }

    #[tokio::test]
    async fn failed_workflow_without_detail_reports_unknown_error() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub("WorkflowStatus", status("Error", Some("")));

        let configs = server.configs(&dir);
        let client = reqwest::Client::new();
        let err = wait_for_workflow(&client, &configs, "wf-1".to_string())
            .await
            .unwrap_err();
        assert!(matches!(err, WorkflowError::Failed(ref m) if m == "Unknown error"));
    }

    #[tokio::test]
    async fn running_polls_until_complete() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub("WorkflowStatus", status("Running", None));
        server.stub("WorkflowStatus", status("Complete", None));

        let configs = server.configs(&dir);
        let client = reqwest::Client::new();
        wait_for_workflow(&client, &configs, "wf-1".to_string())
            .await
            .unwrap();
        assert_eq!(server.hits(), 2);
    }

    #[tokio::test]
    async fn not_found_tolerates_read_after_write_lag() {
        // A just-created workflow can briefly read as NotFound; the wait must
        // poll through it rather than give up.
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub("WorkflowStatus", status("NotFound", None));
        server.stub("WorkflowStatus", status("Complete", None));

        let configs = server.configs(&dir);
        let client = reqwest::Client::new();
        wait_for_workflow(&client, &configs, "wf-1".to_string())
            .await
            .unwrap();
        assert_eq!(server.hits(), 2);
    }
}
