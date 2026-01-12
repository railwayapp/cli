use colored::*;
use futures::StreamExt;
use std::time::Duration;

use crate::{
    consts::TICK_STRING,
    controllers::project::{
        ensure_project_and_environment_exist, find_service_instance, get_project,
    },
    errors::RailwayError,
    subscription::subscribe_graphql,
    subscriptions::deployment::DeploymentStatus,
    util::prompt::prompt_confirm_with_default,
};

use super::*;
use anyhow::{anyhow, bail};

/// Restart the latest deployment of a service (without rebuilding)
#[derive(Parser)]
pub struct Args {
    /// The service ID/name to restart
    #[clap(long, short)]
    service: Option<String>,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    yes: bool,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let service_id = args
        .service
        .or_else(|| linked_project.service.clone())
        .ok_or_else(|| {
            anyhow!(
                "No service found. Please link one via `railway link` or specify one via the `--service` flag."
            )
        })?;
    let service = project
        .services
        .edges
        .iter()
        .find(|s| {
            s.node.id == service_id || s.node.name.to_lowercase() == service_id.to_lowercase()
        })
        .ok_or_else(|| anyhow!(RailwayError::ServiceNotFound(service_id)))?;

    let service_in_env =
        find_service_instance(&project, &linked_project.environment, &service.node.id).ok_or_else(
            || anyhow!("The service specified doesn't exist in the current environment"),
        )?;

    let Some(ref latest) = service_in_env.latest_deployment else {
        bail!("No deployment found for service")
    };

    if !args.yes {
        let confirmed = prompt_confirm_with_default(
            format!(
                "Restart the latest deployment from service {} in environment {}?",
                service.node.name,
                linked_project
                    .environment_name
                    .clone()
                    .unwrap_or("unknown".to_string())
            )
            .as_str(),
            false,
        )?;

        if !confirmed {
            return Ok(());
        }
    }

    let spinner = if !args.json {
        let s = indicatif::ProgressBar::new_spinner()
            .with_style(
                indicatif::ProgressStyle::default_spinner()
                    .tick_chars(TICK_STRING)
                    .template("{spinner:.green} {msg}")?,
            )
            .with_message(format!(
                "Restarting the latest deployment from service {}...",
                service.node.name
            ));
        s.enable_steady_tick(Duration::from_millis(100));
        Some(s)
    } else {
        None
    };

    post_graphql::<mutations::DeploymentRestart, _>(
        &client,
        configs.get_backboard(),
        mutations::deployment_restart::Variables {
            id: latest.id.clone(),
        },
    )
    .await?;

    if args.json {
        println!("{}", serde_json::json!({"id": latest.id}));
        return Ok(());
    }

    let spinner = spinner.unwrap();
    spinner.set_message(format!(
        "Waiting for deployment from service {} to be healthy...",
        service.node.name
    ));

    let mut stream =
        subscribe_graphql::<subscriptions::Deployment>(subscriptions::deployment::Variables {
            id: latest.id.clone(),
        })
        .await?;

    while let Some(Ok(res)) = stream.next().await {
        if let Some(errors) = res.errors {
            spinner.finish_with_message(format!(
                "Failed to get deployment status: {}",
                errors
                    .iter()
                    .map(|err| err.to_string())
                    .collect::<Vec<String>>()
                    .join("; ")
            ));
            bail!("Failed to get deployment status");
        }
        if let Some(data) = res.data {
            match data.deployment.status {
                DeploymentStatus::SUCCESS => {
                    spinner.finish_with_message(format!(
                        "The latest deployment from service {} has been restarted and is healthy",
                        service.node.name.green()
                    ));
                    return Ok(());
                }
                DeploymentStatus::FAILED => {
                    spinner.finish_with_message(format!(
                        "Deployment from service {} failed",
                        service.node.name.red()
                    ));
                    bail!("Deployment failed");
                }
                DeploymentStatus::CRASHED => {
                    spinner.finish_with_message(format!(
                        "Deployment from service {} crashed",
                        service.node.name.red()
                    ));
                    bail!("Deployment crashed");
                }
                _ => {}
            }
        }
    }

    spinner.finish_with_message(format!(
        "The latest deployment from service {} has been restarted",
        service.node.name.green()
    ));

    Ok(())
}
