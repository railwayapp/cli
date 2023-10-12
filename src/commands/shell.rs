use std::collections::BTreeMap;

use crate::{controllers::project::get_project, errors::RailwayError, util::shell::get_shell};

use super::*;

/// Open a subshell with Railway variables available
#[derive(Parser)]
pub struct Args {
    /// Service to pull variables from (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,

    /// Open shell without banner
    #[clap(long)]
    silent: bool,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let mut all_variables = BTreeMap::<String, String>::new();
    all_variables.insert("IN_RAILWAY_SHELL".to_owned(), "true".to_owned());

    if let Some(service) = args.service {
        let service_id = project
            .services
            .edges
            .iter()
            .find(|s| s.node.name == service || s.node.id == service)
            .ok_or_else(|| RailwayError::ServiceNotFound(service))?;

        let vars = queries::variables_for_service_deployment::Variables {
            environment_id: linked_project.environment.clone(),
            project_id: linked_project.project.clone(),
            service_id: service_id.node.id.clone(),
        };

        let mut variables = post_graphql::<queries::VariablesForServiceDeployment, _>(
            &client,
            configs.get_backboard(),
            vars,
        )
        .await?
        .variables_for_service_deployment;

        all_variables.append(&mut variables);
    } else if let Some(service) = linked_project.service {
        let vars = queries::variables_for_service_deployment::Variables {
            environment_id: linked_project.environment.clone(),
            project_id: linked_project.project.clone(),
            service_id: service.clone(),
        };

        let mut variables = post_graphql::<queries::VariablesForServiceDeployment, _>(
            &client,
            configs.get_backboard(),
            vars,
        )
        .await?
        .variables_for_service_deployment;

        all_variables.append(&mut variables);
    } else {
        eprintln!("No service linked, not entering shell.");
        return Ok(());
    }

    let shell = get_shell().await;

    let shell_options = match shell.as_str() {
        "powershell" => vec!["/nologo"],
        "pwsh" => vec!["/nologo"],
        "cmd" => vec!["/k"],
        _ => vec![],
    };

    if !args.silent {
        println!("Entering subshell with Railway variables available. Type 'exit' to exit.\n");
    }

    // a bit janky :/
    ctrlc::set_handler(move || {
        // do nothing, we just want to ignore CTRL+C
        // this is for `rails c` and similar REPLs
    })?;

    tokio::process::Command::new(shell)
        .args(shell_options)
        .envs(all_variables)
        .spawn()
        .context("Failed to spawn command")?
        .wait()
        .await
        .context("Failed to wait for command")?;

    if !args.silent {
        println!("Exited subshell, Railway variables no longer available.");
    }
    Ok(())
}
