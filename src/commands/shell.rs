use std::collections::BTreeMap;

use crate::{consts::SERVICE_NOT_FOUND, controllers::project::get_project};

use super::*;

/// Open a subshell with Railway variables available
#[derive(Parser)]
pub struct Args {
    /// Service to pull variables from (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,
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
            .context(SERVICE_NOT_FOUND)?;

        let vars = queries::variables_for_service_deployment::Variables {
            environment_id: linked_project.environment.clone(),
            project_id: linked_project.project.clone(),
            service_id: service_id.node.id.clone(),
        };

        let res = post_graphql::<queries::VariablesForServiceDeployment, _>(
            &client,
            configs.get_backboard(),
            vars,
        )
        .await?;

        let mut body = res.data.context("Failed to retrieve response body")?;

        all_variables.append(&mut body.variables_for_service_deployment);
    } else if let Some(service) = linked_project.service {
        let vars = queries::variables_for_service_deployment::Variables {
            environment_id: linked_project.environment.clone(),
            project_id: linked_project.project.clone(),
            service_id: service.clone(),
        };

        let res = post_graphql::<queries::VariablesForServiceDeployment, _>(
            &client,
            configs.get_backboard(),
            vars,
        )
        .await?;

        let mut body = res.data.context("Failed to retrieve response body")?;

        all_variables.append(&mut body.variables_for_service_deployment);
    } else {
        eprintln!("No service linked, skipping service variables");
    }

    let shell = std::env::var("SHELL").unwrap_or(match std::env::consts::OS {
        "windows" => "cmd".to_string(),
        _ => "sh".to_string(),
    });

    println!("Entering subshell with Railway variables available. Type 'exit' to exit.");

    tokio::process::Command::new(shell)
        .envs(all_variables)
        .spawn()
        .context("Failed to spawn command")?
        .wait()
        .await
        .context("Failed to wait for command")?;
    println!("Exited subshell, Railway variables no longer available.");
    Ok(())
}
