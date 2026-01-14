use serde::Serialize;

use crate::{
    controllers::{
        environment::get_matched_environment,
        project::{ensure_project_and_environment_exist, get_project, get_service_ids_in_env},
    },
    errors::RailwayError,
    util::prompt::{PromptService, prompt_options},
};

use super::*;

/// Manage services
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,

    /// The service ID/name to link (deprecated: use 'service link' instead)
    service: Option<String>,
}

#[derive(Parser)]
enum Commands {
    /// Link a service to the current project
    Link(LinkArgs),

    /// Show deployment status for services
    Status(StatusArgs),

    /// View logs from a service
    Logs(crate::commands::logs::Args),

    /// Redeploy the latest deployment of a service
    Redeploy(crate::commands::redeploy::Args),

    /// Restart the latest deployment of a service
    Restart(crate::commands::restart::Args),

    /// Scale a service across regions
    Scale(crate::commands::scale::Args),
}

#[derive(Parser)]
struct LinkArgs {
    /// The service ID/name to link
    service: Option<String>,
}

#[derive(Parser)]
struct StatusArgs {
    /// Service name or ID to show status for (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,

    /// Show status for all services in the environment
    #[clap(short, long)]
    all: bool,

    /// Environment to check status in (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceStatusOutput {
    id: String,
    name: String,
    deployment_id: Option<String>,
    status: Option<String>,
    stopped: bool,
}

pub async fn command(args: Args) -> Result<()> {
    // Handle legacy direct service link (when no subcommand is provided)
    // This maintains backward compatibility:
    // - `railway service` -> prompts to link
    // - `railway service <name>` -> links that service
    if args.command.is_none() {
        return link_command(LinkArgs {
            service: args.service,
        })
        .await;
    }

    match args.command {
        Some(Commands::Link(link_args)) => link_command(link_args).await,
        Some(Commands::Status(status_args)) => status_command(status_args).await,
        Some(Commands::Logs(logs_args)) => crate::commands::logs::command(logs_args).await,
        Some(Commands::Redeploy(redeploy_args)) => {
            crate::commands::redeploy::command(redeploy_args).await
        }
        Some(Commands::Restart(restart_args)) => {
            crate::commands::restart::command(restart_args).await
        }
        Some(Commands::Scale(scale_args)) => crate::commands::scale::command(scale_args).await,
        None => unreachable!(),
    }
}

async fn link_command(args: LinkArgs) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let service_ids_in_env = get_service_ids_in_env(&project, &linked_project.environment);
    let services: Vec<_> = project
        .services
        .edges
        .iter()
        .filter(|a| service_ids_in_env.contains(&a.node.id))
        .map(|s| PromptService(&s.node))
        .collect();

    let service = if let Some(name) = args.service {
        services
            .into_iter()
            .find(|s| s.0.id.eq_ignore_ascii_case(&name) || s.0.name.eq_ignore_ascii_case(&name))
            .ok_or_else(|| RailwayError::ServiceNotFound(name))?
    } else if services.is_empty() {
        bail!("No services found")
    } else {
        prompt_options("Select a service", services)?
    };

    configs.link_service(service.0.id.clone())?;
    configs.write()?;
    println!("Linked service {}", service.0.name.green());
    Ok(())
}

async fn status_command(args: StatusArgs) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    // Determine which environment to use
    let environment_id = if let Some(env_name) = args.environment {
        let env = get_matched_environment(&project, env_name)?;
        env.id
    } else {
        linked_project.environment.clone()
    };

    let environment_name = project
        .environments
        .edges
        .iter()
        .find(|env| env.node.id == environment_id)
        .map(|env| env.node.name.clone())
        .context("Environment not found")?;

    // Collect service instances for the environment
    let mut service_statuses: Vec<ServiceStatusOutput> = Vec::new();

    let env = project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment_id)
        .context("Environment not found")?;

    for instance_edge in &env.node.service_instances.edges {
        let instance = &instance_edge.node;
        let deployment = &instance.latest_deployment;

        service_statuses.push(ServiceStatusOutput {
            id: instance.service_id.clone(),
            name: instance.service_name.clone(),
            deployment_id: deployment.as_ref().map(|d| d.id.clone()),
            status: deployment.as_ref().map(|d| format!("{:?}", d.status)),
            stopped: deployment
                .as_ref()
                .map(|d| d.deployment_stopped)
                .unwrap_or(false),
        });
    }

    if args.all {
        // Show all services
        if args.json {
            println!("{}", serde_json::to_string_pretty(&service_statuses)?);
        } else {
            if service_statuses.is_empty() {
                println!("No services found in environment '{}'", environment_name);
                return Ok(());
            }

            println!("Services in {}:\n", environment_name.blue().bold());

            for status in service_statuses {
                let status_display = format_status_display(&status);

                println!(
                    "{:<20} | {:<14} | {}",
                    status.name.bold(),
                    status.deployment_id.as_deref().unwrap_or("N/A").dimmed(),
                    status_display
                );
            }
        }
    } else {
        // Show single service (specified or linked)
        let target_service = if let Some(service_name) = args.service {
            service_statuses
                .iter()
                .find(|s| s.id == service_name || s.name == service_name)
                .ok_or_else(|| RailwayError::ServiceNotFound(service_name.clone()))?
        } else {
            // Use linked service
            let linked_service_id = linked_project
                .service
                .as_ref()
                .context("No service linked. Use --service flag or --all to see all services")?;

            service_statuses
                .iter()
                .find(|s| &s.id == linked_service_id)
                .context("Linked service not found in this environment")?
        };

        if args.json {
            println!("{}", serde_json::to_string_pretty(&target_service)?);
        } else {
            println!("Service: {}", target_service.name.green().bold());
            println!(
                "Deployment: {}",
                target_service
                    .deployment_id
                    .as_deref()
                    .unwrap_or("No deployment")
                    .dimmed()
            );
            println!("Status: {}", format_status_display(target_service));
        }
    }

    Ok(())
}

fn format_status_display(status: &ServiceStatusOutput) -> colored::ColoredString {
    if status.stopped && status.status.as_deref() == Some("SUCCESS") {
        return "STOPPED".yellow();
    }

    match status.status.as_deref() {
        Some("SUCCESS") => "SUCCESS".green(),
        Some("FAILED") | Some("CRASHED") => status.status.as_deref().unwrap_or("UNKNOWN").red(),
        Some("BUILDING") | Some("DEPLOYING") | Some("INITIALIZING") | Some("QUEUED") => {
            status.status.as_deref().unwrap_or("UNKNOWN").blue()
        }
        Some("SLEEPING") => "SLEEPING".yellow(),
        Some("REMOVED") | Some("REMOVING") => {
            status.status.as_deref().unwrap_or("UNKNOWN").dimmed()
        }
        Some(s) => s.white(),
        None => "NO DEPLOYMENT".dimmed(),
    }
}
