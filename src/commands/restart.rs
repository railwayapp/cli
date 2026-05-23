use colored::*;
use futures::StreamExt;
use is_terminal::IsTerminal;

use crate::{
    controllers::{
        deployment::restart_latest_service_deployment,
        project::{find_service_instance, get_environment_instances, resolve_service_context},
    },
    subscription::subscribe_graphql,
    subscriptions::deployment::DeploymentStatus,
    util::{progress::create_spinner_if, prompt::prompt_confirm_with_default},
};

use super::*;
use anyhow::{anyhow, bail};

/// Restart the latest deployment of a service (without rebuilding)
#[derive(Parser)]
pub struct Args {
    /// The service ID/name to restart
    #[clap(long, short)]
    service: Option<String>,

    /// Environment to restart in (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

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
    let ctx = resolve_service_context(args.project, args.service, args.environment).await?;
    let is_terminal = std::io::stdout().is_terminal();
    let service_id = ctx.service_id;
    let service_name = ctx.service_name;
    let environment_name = ctx.environment_name;

    let environment_instances =
        get_environment_instances(&client, &configs, &ctx.project_id, &ctx.environment_id).await?;
    let service_in_env = find_service_instance(&environment_instances, &service_id)
        .ok_or_else(|| anyhow!("The service specified doesn't exist in the current environment"))?;

    let Some(ref latest) = service_in_env.latest_deployment else {
        bail!("No deployment found for service")
    };

    let confirmed = if args.yes {
        true
    } else if is_terminal {
        prompt_confirm_with_default(
            format!(
                "Restart the latest deployment from service {} in environment {}?",
                service_name, environment_name
            )
            .as_str(),
            false,
        )?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };

    if !confirmed {
        return Ok(());
    }

    let spinner = create_spinner_if(
        !args.json,
        format!(
            "Restarting the latest deployment from service {}...",
            service_name
        ),
    );

    restart_latest_service_deployment(
        &client,
        &configs,
        &ctx.project_id,
        &ctx.environment_id,
        &service_id,
    )
    .await?;

    if args.json {
        println!("{}", serde_json::json!({"id": latest.id}));
        return Ok(());
    }

    let spinner = spinner.unwrap();
    spinner.set_message(format!(
        "Waiting for deployment from service {} to be healthy...",
        service_name
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
                        service_name.green()
                    ));
                    return Ok(());
                }
                DeploymentStatus::FAILED => {
                    spinner.finish_with_message(format!(
                        "Deployment from service {} failed",
                        service_name.red()
                    ));
                    bail!("Deployment failed");
                }
                DeploymentStatus::CRASHED => {
                    spinner.finish_with_message(format!(
                        "Deployment from service {} crashed",
                        service_name.red()
                    ));
                    bail!("Deployment crashed");
                }
                _ => {}
            }
        }
    }

    spinner.finish_with_message(format!(
        "The latest deployment from service {} has been restarted",
        service_name.green()
    ));

    Ok(())
}
