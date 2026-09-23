use std::{
    future::IntoFuture,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use colored::Colorize;
use futures::StreamExt;
use graphql_ws_client::{Subscription, graphql::StreamingOperation};
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::{
    gql::subscriptions, subscription::connect_graphql, util::logs::strip_terminal_controls,
};

/// Live event details supplement the verdict; they never determine the exit
/// code. Keep them available during build-log recovery and HTTP status polling.
#[derive(Default)]
pub(super) struct DeploymentErrors(Mutex<Vec<(String, String)>>);

impl DeploymentErrors {
    fn message(&self) -> Option<String> {
        let errors = self.0.lock().unwrap();
        let (_, error) = errors.last()?;
        let error = strip_terminal_controls(error);
        let message = strip_build_log_hint(&error);
        (!message.is_empty()).then(|| message.to_owned())
    }

    pub(super) fn with_error(&self, mut result: Value) -> Value {
        if let Some(error) = self.message() {
            result["error"] = Value::String(error);
        }
        result
    }

    pub(super) fn print_failure(&self) {
        let message = match self.message() {
            Some(error) => format!("Deploy failed: {error}"),
            None => "Deploy failed".to_owned(),
        };
        println!("{}", message.red().bold());
    }

    fn record(&self, event: subscriptions::deployment_events::DeploymentEventsDeploymentEvents) {
        let mut errors = self.0.lock().unwrap();
        // A step may be retried or cleared. Replace its previous error instead
        // of retaining a stale failure after a subsequent successful update.
        errors.retain(|(id, _)| id != &event.id);
        if let Some(payload) = event.payload
            && payload.skipped != Some(true)
            && let Some(error) = payload.error.filter(|error| !error.trim().is_empty())
        {
            errors.push((event.id, error));
        }
    }
}

fn strip_build_log_hint(message: &str) -> &str {
    // Compare the trailing letters, retaining their original byte offsets so
    // the actual error keeps its casing and punctuation. This also handles
    // line breaks, Unicode whitespace/punctuation, and punctuation inside words.
    let mut letters = message
        .char_indices()
        .rev()
        .filter(|(_, c)| c.is_alphanumeric());
    let mut start = message.len();
    for expected in "pleasecheckthebuildlogsformoredetails".chars().rev() {
        match letters.next() {
            Some((offset, actual)) if actual.eq_ignore_ascii_case(&expected) => start = offset,
            _ => return message.trim(),
        }
    }
    // Don't strip a suffix embedded in a longer word, e.g. "displease".
    if message[..start].ends_with(char::is_alphanumeric) {
        return message.trim();
    }
    message[..start].trim()
}

pub(super) struct DeploymentStream {
    pub(super) status: Subscription<StreamingOperation<subscriptions::Deployment>>,
    _tasks: SubscriptionTasks,
}

struct SubscriptionTasks {
    connection: JoinHandle<()>,
    events: Option<JoinHandle<()>>,
}

impl Drop for SubscriptionTasks {
    fn drop(&mut self) {
        if let Some(events) = &self.events {
            events.abort();
        }
        self.connection.abort();
    }
}

impl DeploymentStream {
    pub(super) async fn connect(id: &str, errors: Arc<DeploymentErrors>) -> Result<Self> {
        let (client, actor) = connect_graphql().await?;
        let mut tasks = SubscriptionTasks {
            connection: tokio::spawn(actor.into_future()),
            events: None,
        };

        // Both operations use this client/socket. Listen for events first so
        // the friendly error can be captured before the FAILED status arrives.
        let mut events = client
            .subscribe(StreamingOperation::<subscriptions::DeploymentEvents>::new(
                subscriptions::deployment_events::Variables { id: id.to_owned() },
            ))
            .await?;
        tasks.events = Some(tokio::spawn(async move {
            while let Some(Ok(response)) = events.next().await {
                if let Some(data) = response.data {
                    errors.record(data.deployment_events);
                }
            }
        }));

        let status = client
            .subscribe(StreamingOperation::<subscriptions::Deployment>::new(
                subscriptions::deployment::Variables { id: id.to_owned() },
            ))
            .await?;

        Ok(Self {
            status,
            _tasks: tasks,
        })
    }
}
