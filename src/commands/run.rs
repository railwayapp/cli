use anyhow::bail;
use is_terminal::IsTerminal;

use crate::{
    controllers::{
        develop::variables::inject_mkcert_ca_vars,
        environment::get_matched_environment,
        local_override::{
            apply_local_overrides, build_local_override_context, is_local_develop_active,
        },
        project::{ensure_project_and_environment_exist, get_project},
        variables::get_service_variables,
    },
    errors::RailwayError,
    util::prompt::{PromptService, prompt_select},
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

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Skip local develop overrides even if docker-compose.yml exists
    #[clap(long)]
    no_local: bool,

    /// Show verbose domain replacement info
    #[clap(short, long)]
    verbose: bool,

    /// Args to pass to the command
    #[clap(trailing_var_arg = true)]
    args: Vec<String>,
}

fn get_service(
    project: &ProjectProject,
    service_arg: Option<String>,
    linked_service: Option<String>,
) -> Result<String> {
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
    } else if let Some(service) = linked_service {
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

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;

    if args.project.is_some() && args.environment.is_none() {
        bail!("--environment is required when using --project");
    }

    let linked_project = if args.project.is_none() {
        Some(configs.get_linked_project().await?)
    } else {
        None
    };

    if let Some(ref lp) = linked_project {
        ensure_project_and_environment_exist(&client, &configs, lp).await?;
    }

    let project_id = args
        .project
        .clone()
        .or_else(|| linked_project.as_ref().map(|lp| lp.project.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!("No project specified. Use --project or run `railway link` first")
        })?;

    let project = get_project(&client, &configs, project_id.clone()).await?;

    let environment = args
        .environment
        .clone()
        .or_else(|| linked_project.as_ref().map(|lp| lp.environment.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No environment specified. Use --environment or run `railway link` first"
            )
        })?;

    let environment_id = get_matched_environment(&project, environment)?.id;
    let linked_service = linked_project.as_ref().and_then(|lp| lp.service.clone());
    let service = get_service(&project, args.service.clone(), linked_service)?;

    let mut variables = get_service_variables(
        &client,
        &configs,
        project_id.clone(),
        environment_id.clone(),
        service.clone(),
    )
    .await?;

    if !args.no_local && is_local_develop_active(&project.id) {
        let ctx =
            build_local_override_context(&client, &configs, &project, &environment_id).await?;

        variables = apply_local_overrides(variables, &service, &ctx);
        if ctx.https_enabled() {
            inject_mkcert_ca_vars(&mut variables);
        }
        eprintln!("{}", "Using local develop services".yellow());
    }

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
