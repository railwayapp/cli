use anyhow::bail;
use is_terminal::IsTerminal;
use serde::Serialize;

use crate::{
    client::post_graphql,
    commands::output::service_summary::{
        ServiceOutput, build_service_output, fetch_region_locations, print_service_card,
    },
    controllers::{
        environment::get_matched_environment,
        project::{
            ensure_project_and_environment_exist, get_environment_instances, get_project,
            get_service_ids_in_env, service_instances_in_env,
        },
    },
    errors::RailwayError,
    util::{
        progress::create_spinner_if,
        prompt::{PromptService, fake_select, prompt_confirm_with_default, prompt_options},
        two_factor::validate_two_factor_if_enabled,
    },
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
    /// List services in the current environment
    #[clap(alias = "ls")]
    List(ListArgs),

    /// Delete a service from an environment
    #[clap(alias = "remove", alias = "rm")]
    Delete(DeleteArgs),

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
struct ListArgs {
    /// Environment to list services from (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct DeleteArgs {
    /// Service name or ID to delete (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to delete the service from (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    yes: bool,

    /// Output in JSON format
    #[clap(long)]
    json: bool,

    /// 2FA code for verification (required if 2FA is enabled in non-interactive mode)
    #[clap(long = "2fa-code")]
    two_factor_code: Option<String>,
}

/// Legacy `status --all` output shape. Preserved so scripts parsing the
/// deprecated command's JSON don't break.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceStatusOutput {
    id: String,
    name: String,
    deployment_id: Option<String>,
    status: Option<String>,
    stopped: bool,
}

#[derive(Parser)]
struct StatusArgs {
    /// Service name or ID to show status for (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,

    /// Deprecated: use `railway service list` instead. Kept for backwards compatibility.
    #[clap(short, long, hide = true)]
    all: bool,

    /// Environment to check status in (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
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
        Some(Commands::List(list_args)) => list_command(list_args).await,
        Some(Commands::Delete(delete_args)) => delete_command(delete_args).await,
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

async fn list_command(args: ListArgs) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let (project, region_locations) = tokio::join!(
        get_project(&client, &configs, linked_project.project.clone()),
        fetch_region_locations(&client, &configs),
    );
    let project = project?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let env_id = if let Some(env_name) = args.environment {
        get_matched_environment(&project, env_name)?.id
    } else {
        linked_project.environment_id()?.to_string()
    };
    let env_name = project
        .environments
        .edges
        .iter()
        .find(|env| env.node.id == env_id)
        .map(|env| env.node.name.clone())
        .expect("environment resolved above");

    let environment_instances =
        get_environment_instances(&client, &configs, &linked_project.project, &env_id).await?;
    let service_ids_in_env = get_service_ids_in_env(&environment_instances);
    let linked_service_id = linked_project.service.as_deref();

    let mut services: Vec<_> = project
        .services
        .edges
        .iter()
        .filter(|edge| service_ids_in_env.contains(&edge.node.id))
        .collect();
    services.sort_by(|a, b| a.node.name.to_lowercase().cmp(&b.node.name.to_lowercase()));

    let rows: Vec<ServiceOutput> = services
        .iter()
        .map(|edge| {
            build_service_output(
                &environment_instances,
                &edge.node,
                linked_service_id,
                &region_locations,
            )
        })
        .collect();

    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if rows.is_empty() {
        println!("No services found in environment '{env_name}'");
        return Ok(());
    }

    println!();
    println!("{} {}", "Services in".bold(), env_name.blue().bold());
    println!();

    for row in &rows {
        print_service_card(row, true);
    }

    Ok(())
}

async fn delete_command(args: DeleteArgs) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let local_linked_project = configs.get_local_linked_project().ok();
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;
    let is_terminal = std::io::stdout().is_terminal();
    let environment =
        resolve_environment_to_delete(&project, &linked_project, args.environment.as_deref())?;
    let environment_id = environment.id.clone();
    let environment_name = environment.name.clone();

    let environment_instances =
        get_environment_instances(&client, &configs, &linked_project.project, &environment_id)
            .await?;
    let service_ids_in_env = get_service_ids_in_env(&environment_instances);
    let services_in_env: Vec<_> = project
        .services
        .edges
        .iter()
        .filter(|edge| service_ids_in_env.contains(&edge.node.id))
        .map(|edge| &edge.node)
        .collect();

    let service = select_service_to_delete(
        services_in_env,
        args.service.as_deref(),
        linked_project.service.as_deref(),
        &environment_name,
        !args.json,
        is_terminal,
    )?;
    let service_id = service.id.clone();
    let service_name = service.name.clone();

    let confirmed = if args.yes {
        true
    } else if is_terminal {
        prompt_confirm_with_default(
            format!(
                "Are you sure you want to delete service \"{}\" from environment \"{}\"? This will permanently delete all its deployments.",
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
        if !args.json {
            println!("Deletion cancelled.");
        }
        return Ok(());
    }

    validate_two_factor_if_enabled(&client, &configs, is_terminal, args.two_factor_code).await?;

    let spinner = create_spinner_if(!args.json, format!("Deleting service {}...", service_name));

    post_graphql::<mutations::ServiceDelete, _>(
        &client,
        configs.get_backboard(),
        mutations::service_delete::Variables {
            service_id: service_id.clone(),
            environment_id: environment_id.clone(),
        },
    )
    .await?;

    let unlink_path = local_linked_project.as_ref().and_then(|project| {
        (project.project == linked_project.project
            && project.service.as_deref() == Some(service_id.as_str())
            && project.environment_id().ok() == Some(environment_id.as_str()))
        .then(|| project.project_path.clone())
    });
    let should_unlink = unlink_path.is_some();
    if let Some(path) = unlink_path {
        let linked_project = configs
            .root_config
            .projects
            .get_mut(&path)
            .ok_or(RailwayError::ProjectNotFound)?;
        linked_project.service = None;
        configs.write()?;
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": service_id,
                "name": service_name,
                "environmentId": environment_id,
                "environmentName": environment_name,
                "unlinked": should_unlink,
            }))?
        );
    } else if let Some(spinner) = spinner {
        spinner.finish_with_message(format!(
            "Deleted service {} from {}",
            service_name.green(),
            environment_name.blue()
        ));
    }

    Ok(())
}

fn resolve_environment_to_delete(
    project: &crate::gql::queries::project::ProjectProject,
    linked_project: &crate::LinkedProject,
    environment_arg: Option<&str>,
) -> Result<crate::gql::queries::project::ProjectProjectEnvironmentsEdgesNode> {
    if project.deleted_at.is_some() {
        bail!(RailwayError::ProjectDeleted);
    }

    let environment = if let Some(environment_arg) = environment_arg {
        get_matched_environment(project, environment_arg.to_string())?
    } else {
        let linked_environment = match linked_project
            .environment_name
            .clone()
            .or_else(|| linked_project.environment.clone())
        {
            Some(environment) => environment,
            None => linked_project.environment_id()?.to_string(),
        };

        get_matched_environment(project, linked_environment)?
    };

    if environment.deleted_at.is_some() {
        bail!(RailwayError::EnvironmentDeleted);
    }

    Ok(environment)
}

fn select_service_to_delete<'a>(
    services_in_env: Vec<&'a crate::gql::queries::project::ProjectProjectServicesEdgesNode>,
    service_arg: Option<&str>,
    linked_service_id: Option<&str>,
    environment_name: &str,
    echo_selection: bool,
    is_terminal: bool,
) -> Result<&'a crate::gql::queries::project::ProjectProjectServicesEdgesNode> {
    if services_in_env.is_empty() {
        bail!("No services found in environment '{}'", environment_name);
    }

    if let Some(service_arg) = service_arg {
        let service = services_in_env
            .iter()
            .copied()
            .find(|service| {
                service.id.eq_ignore_ascii_case(service_arg)
                    || service.name.eq_ignore_ascii_case(service_arg)
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Service \"{}\" not found in environment '{}'",
                    service_arg,
                    environment_name
                )
            })?;
        if echo_selection {
            fake_select("Select a service to delete", &service.name);
        }
        return Ok(service);
    }

    if let Some(linked_service_id) = linked_service_id {
        if let Some(service) = services_in_env
            .iter()
            .copied()
            .find(|service| service.id == linked_service_id)
        {
            return Ok(service);
        }
    }

    if !is_terminal {
        bail!(
            "Service must be specified when not running in a terminal. Use --service <id or name>"
        );
    }

    let service = prompt_options(
        "Select a service to delete",
        services_in_env.iter().copied().map(PromptService).collect(),
    )?;

    Ok(service.0)
}

async fn link_command(args: LinkArgs) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let environment_instances = get_environment_instances(
        &client,
        &configs,
        &linked_project.project,
        linked_project.environment_id()?,
    )
    .await?;
    let service_ids_in_env = get_service_ids_in_env(&environment_instances);
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
        if !std::io::stdout().is_terminal() {
            bail!("Service name required in non-interactive mode. Usage: railway service <name>");
        }
        prompt_options("Select a service", services)?
    };

    configs.link_service(service.0.id.clone())?;
    configs.write()?;
    println!("Linked service {}", service.0.name.green());
    Ok(())
}

async fn status_command(args: StatusArgs) -> Result<()> {
    if args.all {
        eprintln!(
            "{}",
            "Warning: `railway service status --all` is deprecated. Please use `railway service list` instead."
                .yellow()
        );
    }

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
        linked_project.environment_id()?.to_string()
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
    let environment_instances =
        get_environment_instances(&client, &configs, &linked_project.project, &environment_id)
            .await?;

    for instance_edge in service_instances_in_env(&environment_instances) {
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
