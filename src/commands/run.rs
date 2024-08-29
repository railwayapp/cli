use anyhow::bail;
use is_terminal::IsTerminal;

use crate::{
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
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

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
