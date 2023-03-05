use std::collections::BTreeMap;

use anyhow::bail;

use crate::consts::SERVICE_NOT_FOUND;

use super::*;

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

    let plugins: Vec<_> = body
        .project
        .plugins
        .edges
        .iter()
        .map(|plugin| &plugin.node)
        .collect();
    let environment_id = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());
    for plugin in plugins {
        let vars = queries::variables::Variables {
            environment_id: environment_id.clone(),
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
            environment_id: environment_id.clone(),
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
            environment_id: environment_id.clone(),
            project_id: linked_project.project.clone(),
            service_id: linked_project.service.clone(),
            plugin_id: None,
        };

        let res =
            post_graphql::<queries::Variables, _>(&client, configs.get_backboard(), vars).await?;

        let mut body = res.data.context("Failed to retrieve response body")?;

        all_variables.append(&mut body.variables);
    } else {
        let services: Vec<_> = body.project.services.edges.iter().collect();
        if services.len() > 1 {
            bail!(
                "Multiple services found, please link one using {}",
                "railway service".bold().dimmed()
            );
        }
        let service_id = services.first().map(|s| s.node.id.clone());

        let vars = queries::variables::Variables {
            environment_id: environment_id.clone(),
            project_id: linked_project.project.clone(),
            service_id,
            plugin_id: None,
        };

        let res =
            post_graphql::<queries::Variables, _>(&client, configs.get_backboard(), vars).await?;

        let mut body = res.data.context("Failed to retrieve response body")?;

        all_variables.append(&mut body.variables);
    }

    tokio::process::Command::new(args.args.first().context("No command provided")?)
        .args(args.args[1..].iter())
        .envs(all_variables)
        .spawn()
        .context("Failed to spawn command")?
        .wait()
        .await
        .context("Failed to wait for command")?;
    Ok(())
}
