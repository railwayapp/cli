use super::*;
use crate::client::post_graphql;
use crate::controllers::project::resolve_service_context;
use crate::gql::queries::deployments::{DeploymentStatus, ResponseData, Variables};
use chrono::{DateTime, Local, Utc};
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
    /// List deployments for a service with IDs, statuses and other metadata
    #[clap(visible_alias = "ls")]
    List(ListArgs),

    /// Upload and deploy project from the current directory
    #[clap(visible_alias = "deploy")]
    Up(crate::commands::up::Args),

    /// Redeploy the latest deployment of a service
    Redeploy(crate::commands::redeploy::Args),
}

#[derive(Parser)]
struct ListArgs {
    /// Service name or ID to list deployments for (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to list deployments from (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID or name to list deployments from (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT")]
    project: Option<String>,

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
            list_deployments(
                list_args.project,
                list_args.service,
                list_args.environment,
                list_args.limit,
                list_args.json,
            )
            .await
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

async fn list_deployments(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    limit: i64,
    json: bool,
) -> Result<()> {
    let limit = if limit > 1000 {
        eprintln!(
            "{}",
            "Warning: limit cannot exceed 1000, using 1000 instead".yellow()
        );
        1000
    } else if limit < 1 {
        eprintln!(
            "{}",
            "Warning: limit must be at least 1, using 1 instead".yellow()
        );
        1
    } else {
        limit
    };

    let ctx = resolve_service_context(project, service, environment).await?;
    let client = ctx.client;
    let configs = ctx.configs;
    let project_id = ctx.project_id;
    let environment_id = ctx.environment_id;
    let service_id = ctx.service_id;

    let variables = Variables {
        input: crate::gql::queries::deployments::DeploymentListInput {
            service_id: Some(service_id.clone()),
            environment_id: Some(environment_id),
            project_id: Some(project_id),
            status: None,
            include_deleted: None,
        },
        first: Some(limit),
    };

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
        return Ok(());
    }

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

        // Convert UTC time to local timezone
        let local_time: DateTime<Local> = DateTime::from(deployment.created_at);
        let created_at = local_time.format("%Y-%m-%d %H:%M:%S %Z");
        println!(
            "  {} | {} | {}",
            deployment.id,
            status_colored,
            created_at.to_string().dimmed()
        );
    }

    Ok(())
}
