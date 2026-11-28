//! Publishing what a run's agent offered to publish (its `publish-workspace`
//! offer): the Publish button in the run's chat, pressed from the terminal or
//! a script. It creates a GitHub repository and a Railway service from the
//! agent's workspace, then deploys.

use std::io::IsTerminal;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use colored::Colorize;
use serde_json::Value;

use crate::{commands::cloud_agent::task, util::progress::create_spinner};

/// How long to follow a publish before leaving it to run on its own.
const PUBLISH_WAIT: Duration = Duration::from_secs(15 * 60);

/// The run whose offer is published, and where to point people.
pub(super) struct Target<'a> {
    pub client: &'a reqwest::Client,
    pub backboard: &'a str,
    pub host: &'a str,
    pub project_id: &'a str,
    pub environment_id: &'a str,
    pub agent_id: &'a str,
    pub session_id: &'a str,
    /// Any task on the run; the API finds its agent's offer from it.
    pub task_id: &'a str,
    /// The command that publishes it, as typed, for trying again.
    pub again: String,
}

/// Publish the offer and follow it until the service exists, it fails, or the
/// wait runs out. Returns where the publish ended up (a `TaskPublishView`).
pub(super) async fn publish(target: &Target<'_>, json: bool) -> Result<Value> {
    let started = task::publish(target.client, target.backboard, target.task_id)
        .await
        .map_err(|error| {
            let message = format!("{error:#}");
            if message.contains("hasn't offered to publish") {
                anyhow::anyhow!("Nothing to publish: the agent hasn't offered to publish this run.")
            } else if message.contains("cloudAgentTaskPublish") {
                anyhow::anyhow!(
                    "Publishing from the CLI isn't available yet. Publish from the run on the dashboard."
                )
            } else {
                error
            }
        })?;
    let spinner = (!json && std::io::stderr().is_terminal()).then(|| {
        create_spinner(
            "Publishing: creating the repository, pushing the code and creating the service"
                .to_string(),
        )
    });
    let deadline = Instant::now() + PUBLISH_WAIT;
    let mut state = started;
    let stopped = loop {
        match status(&state) {
            Some("PUBLISHED" | "FAILED") => break false,
            _ if Instant::now() >= deadline => break false,
            _ => {}
        }
        tokio::select! {
            _ = tokio::time::sleep(task::POLL_INTERVAL) => {}
            _ = tokio::signal::ctrl_c() => break true,
        }
        // A read that fails leaves the last state; the next poll tries again.
        if let Some(read) = task::publish_state(
            target.client,
            target.backboard,
            target.agent_id,
            target.session_id,
            target.task_id,
        )
        .await
        {
            state = read;
        }
    };
    if let Some(spinner) = &spinner {
        spinner.finish_and_clear();
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&state)?);
    } else {
        match status(&state) {
            Some("PUBLISHED") => {
                println!(
                    "{} https://github.com/{}",
                    "Published:".green().bold(),
                    task::text(&state, "repo").unwrap_or_default()
                );
                if let Some(service) = task::text(&state, "serviceId") {
                    println!(
                        "{} https://{}/project/{}/service/{service}?environmentId={}",
                        format!(
                            "{} is deploying:",
                            task::text(&state, "serviceName").unwrap_or("The service")
                        )
                        .dimmed(),
                        target.host,
                        target.project_id,
                        target.environment_id,
                    );
                }
            }
            Some("FAILED") => {}
            _ if stopped => println!(
                "{}",
                format!(
                    "Stopped waiting. It keeps publishing; {} shows where it is.",
                    target.again
                )
                .yellow()
            ),
            _ => println!(
                "{}",
                format!("Still publishing. {} shows where it is.", target.again).yellow()
            ),
        }
    }
    if status(&state) == Some("FAILED") {
        bail!(
            "Publishing failed: {} Try again with {}",
            task::text(&state, "error").unwrap_or("no reason given."),
            target.again
        );
    }
    Ok(state)
}

pub(super) fn status(state: &Value) -> Option<&str> {
    task::text(state, "status")
}

/// What publishing makes, in a sentence, from the agent's offer.
pub(super) fn summary(offer: &Value) -> String {
    let visibility = if offer["private"].as_bool() == Some(false) {
        "public"
    } else {
        "private"
    };
    let named = task::text(offer, "name")
        .map(|name| format!(" ({name})"))
        .unwrap_or_default();
    format!(
        "Publishing creates a {visibility} GitHub repository{named} and a Railway service from it, then deploys."
    )
}

/// Where a publish that isn't just offered stands, in a line.
pub(super) fn state_line(state: &Value, again: &str) -> Option<String> {
    Some(match status(state)? {
        "PUBLISHING" => format!("Publishing now. {again} shows where it is."),
        "PUBLISHED" => format!(
            "Published: https://github.com/{}",
            task::text(state, "repo").unwrap_or_default()
        ),
        "FAILED" => format!(
            "Publishing failed: {} Try again with {again}",
            task::text(state, "error").unwrap_or("no reason given.")
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_offer_says_what_publishing_makes() {
        assert_eq!(
            summary(&json!({ "status": "OFFERED", "name": "water-tracker", "private": true })),
            "Publishing creates a private GitHub repository (water-tracker) and a Railway service from it, then deploys."
        );
        assert_eq!(
            summary(&json!({ "status": "OFFERED", "name": null, "private": false })),
            "Publishing creates a public GitHub repository and a Railway service from it, then deploys."
        );
    }

    #[test]
    fn a_publish_under_way_or_done_says_where_it_is() {
        let again = "railway agent --thread-id t --publish";
        assert_eq!(state_line(&json!({ "status": "OFFERED" }), again), None);
        assert_eq!(
            state_line(&json!({ "status": "PUBLISHED", "repo": "me/app" }), again).as_deref(),
            Some("Published: https://github.com/me/app")
        );
        assert_eq!(
            state_line(
                &json!({ "status": "FAILED", "error": "Repository exists." }),
                again
            )
            .as_deref(),
            Some(
                "Publishing failed: Repository exists. Try again with railway agent --thread-id t --publish"
            )
        );
    }
}
