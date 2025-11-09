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

/// Restart (no image pull) the latest deployment of a service and wait for healthchecks
#[derive(Parser)]
pub struct Args {
    /// The service ID/name to restart
    #[clap(long, short)]
    service: Option<String>,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    bypass: bool,
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
                let env_name = linked_project
                    .environment_name
                    .clone()
                    .unwrap_or("unknown".to_string());

                let confirmed = prompt_confirm_with_default(
                    format!(
                        "Restart the container for service {} in environment {}?",
                        service.node.name, env_name
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
                .with_message(format!("Restarting service {}...", service.node.name));
            spinner.enable_steady_tick(Duration::from_millis(100));

            // Call restart mutation
            post_graphql::<mutations::DeploymentRestart, _>(
                &client,
                configs.get_backboard(),
                mutations::deployment_restart::Variables {
                    id: latest.id.clone(),
                },
            )
            .await?;

            // Wait for healthchecks via latest deployment status
            let max_wait = Duration::from_secs(300);
            let poll_interval = Duration::from_secs(2);
            let start = std::time::Instant::now();
            loop {
                if start.elapsed() > max_wait {
                    spinner.finish_and_clear();
                    bail!("Timed out waiting for health checks after restart");
                }

                let resp = post_graphql::<queries::LatestDeployment, _>(
                    &client,
                    configs.get_backboard(),
                    queries::latest_deployment::Variables {
                        service_id: service.node.id.clone(),
                        environment_id: linked_project.environment.clone(),
                    },
                )
                .await?;

                let si = resp.service_instance;
                if let Some(ld) = si.latest_deployment {
                    match ld.status {
                        queries::latest_deployment::DeploymentStatus::SUCCESS => {
                            spinner.finish_with_message(format!(
                                "Restart successful for service {}",
                                service.node.name.green()
                            ));
                            return Ok(());
                        }
                        queries::latest_deployment::DeploymentStatus::FAILED
                        | queries::latest_deployment::DeploymentStatus::CRASHED => {
                            spinner.finish_and_clear();
                            bail!("Restart completed but health checks failed");
                        }
                        _ => {}
                    }
                }

                tokio::time::sleep(poll_interval).await;
            }
        }
    } else {
        bail!("No deployment found for service")
    }

    Ok(())
}
