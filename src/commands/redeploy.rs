use colored::*;
use std::time::Duration;

use crate::{
    consts::TICK_STRING,
    controllers::project::{ensure_project_and_environment_exist, get_project},
    errors::RailwayError,
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
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
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

    let service_in_env = service
        .node
        .service_instances
        .edges
        .iter()
        .find(|a| a.node.environment_id == linked_project.environment)
        .ok_or_else(|| anyhow!("The service specified doesn't exist in the current environment"))?;

    if let Some(ref latest) = service_in_env.node.latest_deployment {
        if latest.can_redeploy {
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
    } else {
        bail!("No deployment found for service")
    }
    Ok(())
}
