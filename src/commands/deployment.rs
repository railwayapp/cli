use super::*;
use crate::client::post_graphql;
use crate::controllers::project::{ensure_project_and_environment_exist, get_project};
use crate::gql::queries::deployments::{DeploymentStatus, ResponseData, Variables};
use chrono::{DateTime, Utc};
use serde::Serialize;

/// Manage deployments
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,
}

// Aliasing some of our root commands that should be "deployment"
// subcommands. This allows us to deprecate the root commands without
// breaking existing workflows.
#[derive(Parser)]
enum Commands {
    /// List deployments for a service
    #[clap(alias = "ls")]
    List(ListArgs),

    /// Deploy project from the current directory
    Up(crate::commands::up::Args),

    /// Redeploy the latest deployment of a service
    Redeploy(crate::commands::redeploy::Args),
}

#[derive(Parser)]
struct ListArgs {
    /// Service name or ID to list deployments for (defaults to linked service)
    service: Option<String>,

    /// Maximum number of deployments to show (default: 20, max: 1000)
    #[clap(long, default_value = "20")]
    limit: i64,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeploymentOutput {
    id: String,
    status: String,
    created_at: DateTime<Utc>,
    meta: Option<serde_json::Value>,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Commands::List(list_args) => {
            list_deployments(list_args.service, list_args.limit, list_args.json).await
        }
        Commands::Up(deploy_args) => {
            // Call the existing up command implementation
            crate::commands::up::command(deploy_args).await
        }
        Commands::Redeploy(redeploy_args) => {
            // Call the existing redeploy command implementation
            crate::commands::redeploy::command(redeploy_args).await
        }
    }
}

async fn list_deployments(service: Option<String>, limit: i64, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    // Validate limit
    let limit = if limit > 1000 {
        eprintln!("Warning: limit cannot exceed 1000, using 1000 instead");
        1000
    } else if limit < 1 {
        eprintln!("Warning: limit must be at least 1, using 1 instead");
        1
    } else {
        limit
    };

    // Determine service ID
    let service_id = if let Some(service_name_or_id) = service {
        // Look up service by name or ID
        let project = get_project(&client, &configs, linked_project.project.clone()).await?;
        let service = project
            .services
            .edges
            .iter()
            .find(|s| {
                s.node.name.to_lowercase() == service_name_or_id.to_lowercase()
                    || s.node.id == service_name_or_id
            })
            .ok_or_else(|| anyhow::anyhow!("Service '{}' not found", service_name_or_id))?;
        service.node.id.clone()
    } else if let Some(linked_service_id) = linked_project.service {
        linked_service_id
    } else {
        bail!("No service specified and no service linked. Use 'railway link' to link a service or specify one with the service argument.");
    };

    // Prepare GraphQL variables
    let variables = Variables {
        input: crate::gql::queries::deployments::DeploymentListInput {
            service_id: Some(service_id.clone()),
            environment_id: Some(linked_project.environment.clone()),
            project_id: None,
            status: None,
            include_deleted: None,
        },
        first: Some(limit),
    };

    // Query deployments
    let response: ResponseData = post_graphql::<crate::gql::queries::Deployments, _>(
        &client,
        configs.get_backboard(),
        variables,
    )
    .await?;

    let deployments = response
        .deployments
        .edges
        .into_iter()
        .take(limit as usize)
        .map(|edge| edge.node)
        .collect::<Vec<_>>();

    if json {
        let output: Vec<DeploymentOutput> = deployments
            .into_iter()
            .map(|d| DeploymentOutput {
                id: d.id,
                status: format!("{:?}", d.status),
                created_at: d.created_at,
                meta: d.meta,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        if deployments.is_empty() {
            println!("No deployments found");
            return Ok(());
        }

        println!("{}", "Recent Deployments".bold());

        for deployment in deployments {
            let status_colored = match deployment.status {
                DeploymentStatus::SUCCESS => format!("{:?}", deployment.status).green(),
                DeploymentStatus::FAILED | DeploymentStatus::CRASHED => {
                    format!("{:?}", deployment.status).red()
                }
                DeploymentStatus::BUILDING
                | DeploymentStatus::DEPLOYING
                | DeploymentStatus::INITIALIZING
                | DeploymentStatus::WAITING
                | DeploymentStatus::QUEUED => format!("{:?}", deployment.status).blue(),
                DeploymentStatus::REMOVED | DeploymentStatus::REMOVING => {
                    format!("{:?}", deployment.status).dimmed()
                }
                _ => format!("{:?}", deployment.status).white(),
            };

            let created_at = deployment.created_at.format("%Y-%m-%d %H:%M:%S UTC");
            println!(
                "  {} | {} | {}",
                deployment.id,
                status_colored,
                created_at.to_string().dimmed()
            );
        }
    }

    Ok(())
}
