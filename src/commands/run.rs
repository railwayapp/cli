use anyhow::bail;
use is_terminal::IsTerminal;

use crate::{
    controllers::variables::{get_all_plugin_variables, get_service_variables},
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

enum ServiceOrPlugins {
    Service(String),
    Plugins(Vec<String>),
}

async fn get_service_or_plugins(
    configs: &Configs,
    project: &ProjectProject,
    service_arg: Option<String>,
) -> Result<ServiceOrPlugins> {
    let linked_project = configs.get_linked_project().await?;

    let services = project.services.edges.iter().collect::<Vec<_>>();

    let service = if let Some(service_arg) = service_arg {
        // If the user specified a service, use that
        let service_id = services
            .iter()
            .find(|service| service.node.name == service_arg || service.node.id == service_arg);
        if let Some(service_id) = service_id {
            ServiceOrPlugins::Service(service_id.node.id.to_owned())
        } else {
            bail!("Service not found");
        }
    } else if let Some(service) = linked_project.service {
        // If the user didn't specify a service, but we have a linked service, use that
        ServiceOrPlugins::Service(service)
    } else {
        // If the user didn't specify a service, and we don't have a linked service, get the first service

        if services.is_empty() {
            // If there are no services, backboard will generate one for us
            ServiceOrPlugins::Plugins(
                project
                    .plugins
                    .edges
                    .iter()
                    .map(|plugin| plugin.node.id.to_owned())
                    .collect(),
            )
        } else if services.len() == 1 {
            // If there is only one service, use that
            services
                .first()
                .map(|service| service.node.id.to_owned())
                .map(ServiceOrPlugins::Service)
                .unwrap()
        } else {
            // If there are multiple services, prompt the user to select one
            if std::io::stdout().is_terminal() {
                let prompt_services: Vec<_> =
                    services.iter().map(|s| PromptService(&s.node)).collect();
                let service =
                    prompt_select("Select a service to pull variables from", prompt_services)?;
                ServiceOrPlugins::Service(service.0.id.clone())
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

    let vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };
    let res = post_graphql::<queries::Project, _>(&client, configs.get_backboard(), vars).await?;
    let body = res.data.context("Failed to get project (query project)")?;

    let environment = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());

    let environment_id = body
        .project
        .environments
        .edges
        .iter()
        .find(|env| env.node.name == environment || env.node.id == environment)
        .map(|env| env.node.id.to_owned())
        .context("Environment not found")?;

    let service = get_service_or_plugins(&configs, &body.project, args.service).await?;

    let variables = match service {
        ServiceOrPlugins::Service(service_id) => {
            get_service_variables(
                &client,
                &configs,
                linked_project.project.clone(),
                environment_id,
                service_id,
            )
            .await?
        }
        ServiceOrPlugins::Plugins(plugin_ids) => {
            // we fetch all the plugin variables
            get_all_plugin_variables(
                &client,
                &configs,
                linked_project.project.clone(),
                environment_id,
                &plugin_ids,
            )
            .await?
        }
    };

    // a bit janky :/
    ctrlc::set_handler(move || {
        // do nothing, we just want to ignore CTRL+C
        // this is for `rails c` and similar REPLs
    })?;

    let exit_status: std::process::ExitStatus;

    match std::env::consts::OS {
        "windows" => {
            exit_status = tokio::process::Command::new("cmd")
                .arg("/C")
                .args(args.args.iter())
                .envs(variables)
                .status()
                .await
                .context("Failed to spawn command")?;
        }
        _ => {
            exit_status =
                tokio::process::Command::new(args.args.first().context("No command provided")?)
                    .args(args.args[1..].iter())
                    .envs(variables)
                    .status()
                    .await
                    .context("Failed to spawn command")?;
        }
    }

    if exit_status.success() {
        println!("Looking good? Run `railway up` to deploy your changes!");
    }

    if let Some(code) = exit_status.code() {
        // If there is an exit code (process not terminated by signal), exit with that code
        std::process::exit(code);
    }

    Ok(())
}
