use std::collections::BTreeMap;

use crate::consts::SERVICE_NOT_FOUND;

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

    let vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };

    let res = post_graphql::<queries::Project, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;
    let mut all_variables = BTreeMap::<String, String>::new();
    all_variables.insert("IN_RAILWAY_SHELL".to_owned(), "true".to_owned());

    let plugins: Vec<_> = body
        .project
        .plugins
        .edges
        .iter()
        .map(|plugin| &plugin.node)
        .collect();

    for plugin in plugins {
        let vars = queries::variables::Variables {
            environment_id: linked_project.environment.clone(),
            project_id: linked_project.project.clone(),
            service_id: None,
            plugin_id: Some(plugin.id.clone()),
        };

        let res =
            post_graphql::<queries::Variables, _>(&client, configs.get_backboard(), vars).await?;

        let mut body = res.data.context("Failed to retrieve response body")?;

        if body.variables.is_empty() {
            continue;
        }

        all_variables.append(&mut body.variables);
    }

    if let Some(service) = args.service {
        let service_id = body
            .project
            .services
            .edges
            .iter()
            .find(|s| s.node.name == service || s.node.id == service)
            .context(SERVICE_NOT_FOUND)?;

        let vars = queries::variables::Variables {
            environment_id: linked_project.environment.clone(),
            project_id: linked_project.project.clone(),
            service_id: Some(service_id.node.id.clone()),
            plugin_id: None,
        };

        let res =
            post_graphql::<queries::Variables, _>(&client, configs.get_backboard(), vars).await?;

        let mut body = res.data.context("Failed to retrieve response body")?;

        all_variables.append(&mut body.variables);
    } else if linked_project.service.is_some() {
        let vars = queries::variables::Variables {
            environment_id: linked_project.environment.clone(),
            project_id: linked_project.project.clone(),
            service_id: linked_project.service.clone(),
            plugin_id: None,
        };

        let res =
            post_graphql::<queries::Variables, _>(&client, configs.get_backboard(), vars).await?;

        let mut body = res.data.context("Failed to retrieve response body")?;

        all_variables.append(&mut body.variables);
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
