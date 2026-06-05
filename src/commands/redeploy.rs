use colored::*;
use is_terminal::IsTerminal;

use crate::{
    controllers::project::{
        find_service_instance, get_environment_instances, resolve_service_context,
    },
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

    /// Environment to redeploy in (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

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
    let ctx = resolve_service_context(args.project, args.service, args.environment).await?;
    let project_id = ctx.project_id;
    let environment_id = ctx.environment_id;
    let environment_name = ctx.environment_name;
    let service_id = ctx.service_id;
    let service_name = &ctx.service_name;
    let service_node_id = service_id.clone();

    let environment_instances =
        get_environment_instances(&client, &configs, &project_id, &environment_id).await?;
    let service_in_env = find_service_instance(&environment_instances, &service_node_id)
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
                    service_id: service_id.clone(),
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
