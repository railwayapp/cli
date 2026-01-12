use colored::*;
use is_terminal::IsTerminal;

use crate::{
    controllers::project::{
        ensure_project_and_environment_exist, find_service_instance, get_project,
    },
    errors::RailwayError,
    util::{progress::create_spinner_if, prompt::prompt_confirm_with_default},
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
    let is_terminal = std::io::stdout().is_terminal();

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

    if !latest.can_redeploy {
        bail!(
            "The latest deployment for service {} cannot be redeployed. \
            This may be because it's currently building, deploying, or was removed.",
            service.node.name
        );
    }

    let confirmed = if args.bypass {
        true
    } else if is_terminal {
        prompt_confirm_with_default(
            format!(
                "Redeploy the latest deployment from service {} in environment {}?",
                service.node.name,
                linked_project
                    .environment_name
                    .clone()
                    .unwrap_or("unknown".to_string())
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
            "Redeploying the latest deployment from service {}...",
            service.node.name
        ),
    );

    let response = post_graphql::<mutations::DeploymentRedeploy, _>(
        &client,
        configs.get_backboard(),
        mutations::deployment_redeploy::Variables {
            id: latest.id.clone(),
        },
    )
    .await?;

    if args.json {
        println!(
            "{}",
            serde_json::json!({"id": response.deployment_redeploy.id})
        );
    } else if let Some(spinner) = spinner {
        spinner.finish_with_message(format!(
            "The latest deployment from service {} has been redeployed",
            service.node.name.green()
        ));
    }

    Ok(())
}
