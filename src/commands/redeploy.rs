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

    /// Pull and deploy the latest commit or image from the configured source, instead of redeploying the existing deployment
    #[clap(long)]
    from_source: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;
    let environment_id = linked_project.environment_id()?.to_string();
    let environment_name = linked_project
        .environment_name
        .as_deref()
        .unwrap_or("unknown");

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
    let service_name = &service.node.name;

    let service_in_env = find_service_instance(&project, &environment_id, &service.node.id)
        .ok_or_else(|| anyhow!("The service specified doesn't exist in the current environment"))?;

    let latest_deployment_id = if args.from_source {
        None
    } else {
        let latest = service_in_env
            .latest_deployment
            .as_ref()
            .ok_or_else(|| anyhow!("No deployment found for service"))?;
        if !latest.can_redeploy {
            bail!(
                "The latest deployment for service {service_name} cannot be redeployed. \
                This may be because it's currently building, deploying, or was removed."
            );
        }
        Some(latest.id.clone())
    };

    let (prompt_msg, spinner_msg, finish_msg) = if args.from_source {
        (
            format!(
                "Pull the latest source (commit or image) and deploy service {service_name} in environment {environment_name}?"
            ),
            format!("Pulling the latest source and deploying service {service_name}..."),
            format!(
                "Triggered a deploy from the latest source for service {}",
                service_name.green()
            ),
        )
    } else {
        (
            format!(
                "Redeploy the latest deployment from service {service_name} in environment {environment_name}?"
            ),
            format!("Redeploying the latest deployment from service {service_name}..."),
            format!(
                "The latest deployment from service {} has been redeployed",
                service_name.green()
            ),
        )
    };

    let confirmed = if args.bypass {
        true
    } else if std::io::stdout().is_terminal() {
        prompt_confirm_with_default(&prompt_msg, false)?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };

    if !confirmed {
        return Ok(());
    }

    let spinner = create_spinner_if(!args.json, spinner_msg);

    let json_output = match latest_deployment_id {
        None => {
            post_graphql::<mutations::ServiceInstanceDeployLatestCommit, _>(
                &client,
                configs.get_backboard(),
                mutations::service_instance_deploy_latest_commit::Variables {
                    environment_id,
                    service_id: service.node.id.clone(),
                },
            )
            .await?;
            serde_json::json!({ "success": true })
        }
        Some(id) => {
            let response = post_graphql::<mutations::DeploymentRedeploy, _>(
                &client,
                configs.get_backboard(),
                mutations::deployment_redeploy::Variables { id },
            )
            .await?;
            serde_json::json!({ "id": response.deployment_redeploy.id })
        }
    };

    if args.json {
        println!("{json_output}");
    } else if let Some(spinner) = spinner {
        spinner.finish_with_message(finish_msg);
    }

    Ok(())
}
