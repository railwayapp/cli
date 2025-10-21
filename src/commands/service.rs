use anyhow::bail;
use chrono::{DateTime, Local, Utc};
use serde::Serialize;

use crate::{
    controllers::{
        environment::get_matched_environment,
        project::{ensure_project_and_environment_exist, get_project},
    },
    errors::RailwayError,
    util::prompt::{fake_select, prompt_options, PromptService},
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
    created_at: Option<DateTime<Utc>>,
    stopped: bool,
}

pub async fn command(args: Args) -> Result<()> {
    // Handle legacy direct service link (when no subcommand is provided but service arg is)
    if args.command.is_none() && args.service.is_some() {
        return link_command(LinkArgs {
            service: args.service,
        })
        .await;
    }

    match args.command {
        Some(Commands::Link(link_args)) => link_command(link_args).await,
        Some(Commands::Status(status_args)) => status_command(status_args).await,
        None => {
            // If no subcommand and no service arg, show help
            bail!("Please specify a subcommand. Use 'railway service --help' for more information.");
        }
    }
}

async fn link_command(args: LinkArgs) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let services: Vec<_> = project
        .services
        .edges
        .iter()
        .filter(|a| {
            a.node
                .service_instances
                .edges
                .iter()
                .any(|b| b.node.environment_id == linked_project.environment)
        })
        .map(|s| PromptService(&s.node))
        .collect();

    if let Some(service) = args.service {
        let service = services
            .iter()
            .find(|s| s.0.id == service || s.0.name == service)
            .ok_or_else(|| RailwayError::ServiceNotFound(service))?;

        configs.link_service(service.0.id.clone())?;
        configs.write()?;
        return Ok(());
    }

    if services.is_empty() {
        bail!("No services found");
    }

    let service = if !services.is_empty() {
        Some(if let Some(service) = args.service {
            let service_norm = services.iter().find(|s| {
                (s.0.name.to_lowercase() == service.to_lowercase())
                    || (s.0.id.to_lowercase() == service.to_lowercase())
            });
            if let Some(service) = service_norm {
                fake_select("Select a service", &service.0.name);
                service.clone()
            } else {
                return Err(RailwayError::ServiceNotFound(service).into());
            }
        } else {
            prompt_options("Select a service", services)?
        })
    } else {
        None
    };

    if let Some(service) = service {
        configs.link_service(service.0.id.clone())?;
        configs.write()?;
        println!("Linked service {}", service.0.name.green())
    } else {
        bail!("No service found");
    }
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

    for service_edge in &project.services.edges {
        let service = &service_edge.node;

        // Find the service instance for this environment
        if let Some(instance_edge) = service
            .service_instances
            .edges
            .iter()
            .find(|inst| inst.node.environment_id == environment_id)
        {
            let instance = &instance_edge.node;
            let deployment = &instance.latest_deployment;

            service_statuses.push(ServiceStatusOutput {
                id: service.id.clone(),
                name: service.name.clone(),
                deployment_id: deployment.as_ref().map(|d| d.id.clone()),
                status: deployment
                    .as_ref()
                    .map(|d| format!("{:?}", d.status)),
                created_at: deployment.as_ref().map(|d| d.created_at),
                stopped: deployment
                    .as_ref()
                    .map(|d| d.deployment_stopped)
                    .unwrap_or(false),
            });
        }
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

            println!(
                "Services in {}:\n",
                environment_name.blue().bold()
            );

            for status in service_statuses {
                let status_display = format_status_display(&status);
                let time_display = format_time_display(&status);

                println!(
                    "{:<20} | {:<14} | {:<15} | {}",
                    status.name.bold(),
                    status
                        .deployment_id
                        .as_deref()
                        .unwrap_or("N/A")
                        .dimmed(),
                    status_display,
                    time_display
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
            if let Some(created_at) = target_service.created_at {
                let local_time: DateTime<Local> = DateTime::from(created_at);
                println!(
                    "Created: {}",
                    local_time
                        .format("%Y-%m-%d %H:%M:%S %Z")
                        .to_string()
                        .dimmed()
                );
            }
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
        Some("FAILED") | Some("CRASHED") => status
            .status
            .as_deref()
            .unwrap_or("UNKNOWN")
            .red(),
        Some("BUILDING") | Some("DEPLOYING") | Some("INITIALIZING") | Some("QUEUED") => {
            status.status.as_deref().unwrap_or("UNKNOWN").blue()
        }
        Some("SLEEPING") => "SLEEPING".yellow(),
        Some("REMOVED") | Some("REMOVING") => status
            .status
            .as_deref()
            .unwrap_or("UNKNOWN")
            .dimmed(),
        Some(s) => s.white(),
        None => "NO DEPLOYMENT".dimmed(),
    }
}

fn format_time_display(status: &ServiceStatusOutput) -> colored::ColoredString {
    if let Some(created_at) = status.created_at {
        let local_time: DateTime<Local> = DateTime::from(created_at);
        local_time
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
            .dimmed()
    } else {
        "N/A".dimmed()
    }
}
