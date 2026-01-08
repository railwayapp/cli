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

/// Redeploy the latest deployment of a service
#[derive(Parser)]
pub struct Args {
    /// The service ID/name to redeploy from
    #[clap(long, short)]
    service: Option<String>,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    bypass: bool,

    /// Restart the deployment without pulling a new image (useful for refreshing external resources)
    #[clap(long)]
    restart: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let service_id = args.service.or_else(|| linked_project.service.clone()).ok_or_else(|| anyhow!("No service found. Please link one via `railway link` or specify one via the `--service` flag."))?;
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

    if args.restart {
        if !args.bypass {
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

        let spinner = indicatif::ProgressBar::new_spinner()
            .with_style(
                indicatif::ProgressStyle::default_spinner()
                    .tick_chars(TICK_STRING)
                    .template("{spinner:.green} {msg}")?,
            )
            .with_message(format!(
                "Restarting the latest deployment from service {}...",
                service.node.name
            ));
        spinner.enable_steady_tick(Duration::from_millis(100));

        post_graphql::<mutations::DeploymentRestart, _>(
            &client,
            configs.get_backboard(),
            mutations::deployment_restart::Variables {
                id: latest.id.clone(),
            },
        )
        .await?;

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
    } else {
        if !latest.can_redeploy {
            bail!(
                "The latest deployment for service {} cannot be redeployed. \
                This may be because it's currently building, deploying, or was removed.",
                service.node.name
            );
        }

        if !args.bypass {
            let confirmed = prompt_confirm_with_default(
                format!(
                    "Redeploy the latest deployment from service {} in environment {}?",
                    service.node.name,
                    linked_project
                        .environment_name
                        .unwrap_or("unknown".to_string())
                )
                .as_str(),
                false,
            )?;

            if !confirmed {
                return Ok(());
            }
        }

        let spinner = indicatif::ProgressBar::new_spinner()
            .with_style(
                indicatif::ProgressStyle::default_spinner()
                    .tick_chars(TICK_STRING)
                    .template("{spinner:.green} {msg}")?,
            )
            .with_message(format!(
                "Redeploying the latest deployment from service {}...",
                service.node.name
            ));
        spinner.enable_steady_tick(Duration::from_millis(100));

        post_graphql::<mutations::DeploymentRedeploy, _>(
            &client,
            configs.get_backboard(),
            mutations::deployment_redeploy::Variables {
                id: latest.id.clone(),
            },
        )
        .await?;

        spinner.finish_with_message(format!(
            "The latest deployment from service {} has been redeployed",
            service.node.name.green()
        ));
    }

    Ok(())
}
