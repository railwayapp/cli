use anyhow::bail;
use is_terminal::IsTerminal;

use crate::{
    commands::ssh::common::{
        create_spinner, establish_connection, execute_command, get_ssh_connect_params,
    },
    controllers::{
        environment::get_matched_environment,
        project::{ensure_project_and_environment_exist, get_project},
        variables::get_service_variables,
    },
    errors::RailwayError,
    util::prompt::{prompt_select, PromptService},
};

use super::{queries::project::ProjectProject, *};

/// Run a local command using variables from the active environment
#[derive(Debug, Parser)]
pub struct Args {
    /// Service to pull variables from (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to pull variables from (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Run a command remotely on the linked service, whilst creating a duplicate of the linked service.
    /// The service the command is ran on will be disconnected from upstream, and will be automatically deleted after 24 hours.
    #[clap(short, long)]
    remote: bool,

    /// Args to pass to the command
    #[clap(trailing_var_arg = true)]
    args: Vec<String>,
}

async fn get_service(
    configs: &Configs,
    project: &ProjectProject,
    service_arg: Option<String>,
) -> Result<String> {
    let linked_project = configs.get_linked_project().await?;

    let services = project.services.edges.iter().collect::<Vec<_>>();

    let service = if let Some(service_arg) = service_arg {
        // If the user specified a service, use that
        let service_id = services
            .iter()
            .find(|service| service.node.name == service_arg || service.node.id == service_arg);
        if let Some(service_id) = service_id {
            service_id.node.id.to_owned()
        } else {
            bail!("Service not found");
        }
    } else if let Some(service) = linked_project.service {
        // If the user didn't specify a service, but we have a linked service, use that
        service
    } else {
        // If the user didn't specify a service, and we don't have a linked service, get the first service

        if services.is_empty() {
            bail!(RailwayError::ProjectHasNoServices)
        } else {
            // If there are multiple services, prompt the user to select one
            if std::io::stdout().is_terminal() {
                let prompt_services: Vec<_> =
                    services.iter().map(|s| PromptService(&s.node)).collect();
                let service =
                    prompt_select("Select a service to pull variables from", prompt_services)?;
                service.0.id.clone()
            } else {
                bail!("Multiple services found. Please specify a service to pull variables from.")
            }
        }
    };
    Ok(service)
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    // only needs to be mutable for the update check
    let configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    if args.remote {
        let params = get_ssh_connect_params(
            (
                None, /* run doesn't have a project flag */
                args.service.clone(),
                args.environment.clone(),
                None, /* default to latest deployment */
            ),
            &configs,
            &client,
        )
        .await?;

        let token = configs
            .get_railway_auth_token()
            .context("No authentication token found. Please login first with 'railway login'")?;

        let spinner = create_spinner(true);

        let ws_url = format!("wss://{}", configs.get_relay_host_path());
        let mut terminal_client = establish_connection(&ws_url, &token, &params).await?;

        // Run single command
        let run_command = tokio::spawn(async move {
            let e = execute_command(&mut terminal_client, args.args.clone(), spinner).await;
            if let Err(e) = e {
                bail!("Failed to execute command: {e:?}")
            } else {
                Ok(())
            }
        });
        // Now, we need to:
        // 1. Duplicate the current service
        // 2. Disconnect the current service (the one the command is running on) from upstream
        let api_changes: tokio::task::JoinHandle<Result<()>> = tokio::spawn(async move {
            // 1. duplicate

            post_graphql::<mutations::DuplicateService, _>(
                &client,
                configs.get_backboard(),
                mutations::duplicate_service::Variables {
                    id: params.service_id.clone(),
                    environment_id: params.environment_id.clone(),
                },
            )
            .await?;

            // 2. create initial deployment

            post_graphql::<mutations::CreateInitialDeployment, _>(
                &client,
                configs.get_backboard(),
                mutations::create_initial_deployment::Variables {
                    service_id: params.service_id.clone(),
                    environment_id: params.environment_id.clone(),
                },
            )
            .await?;

            // 3. disconnect upstream from original service

            post_graphql::<mutations::UpdateServiceSource, _>(
                &client,
                configs.get_backboard(),
                mutations::update_service_source::Variables {
                    environment_id: params.environment_id.clone(),
                    service_id: params.service_id.clone(),
                    repo: None, // disconnect
                },
            )
            .await?;

            Ok(())
        });
        let (ran, changed) = tokio::join!(run_command, api_changes);
        ran??;
        changed??;
        return Ok(());
    }

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let environment = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());

    let environment_id = get_matched_environment(&project, environment)?.id;
    let service = get_service(&configs, &project, args.service).await?;

    let variables = get_service_variables(
        &client,
        &configs,
        linked_project.project.clone(),
        environment_id,
        service,
    )
    .await?;

    // a bit janky :/
    ctrlc::set_handler(move || {
        // do nothing, we just want to ignore CTRL+C
        // this is for `rails c` and similar REPLs
    })?;

    let mut args = args.args.iter().map(|s| s.as_str()).collect::<Vec<_>>();
    if args.is_empty() {
        return Err(RailwayError::NoCommandProvided.into());
    }

    let child_process_name = match std::env::consts::OS {
        "windows" => {
            args.insert(0, "/C");
            "cmd"
        }
        _ => args.remove(0),
    };

    let exit_status = tokio::process::Command::new(child_process_name)
        .args(args)
        .envs(variables)
        .status()
        .await?;

    if let Some(code) = exit_status.code() {
        // If there is an exit code (process not terminated by signal), exit with that code
        std::process::exit(code);
    }

    Ok(())
}
