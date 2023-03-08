use std::fmt::Display;

use anyhow::bail;


use crate::{
    consts::{NO_SERVICE_LINKED, SERVICE_NOT_FOUND},
    table::Table,
};

use super::{
    queries::project::{PluginType, ProjectProjectPluginsEdgesNode},
    *,
};

/// Show variables for active environment
#[derive(Parser)]
pub struct Args {
    /// Service to show variables for
    #[clap(short, long)]
    service: Option<String>,

    /// Show variables in KV format
    #[clap(short, long)]
    kv: bool,
}

pub async fn command(args: Args, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };

    let res = post_graphql::<queries::Project, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;

    let (vars, name) = if let Some(ref service) = args.service {
        let service_name = body
            .project
            .services
            .edges
            .iter()
            .find(|edge| edge.node.id == *service || edge.node.name == *service)
            .context(SERVICE_NOT_FOUND)?;
        (
            queries::variables_for_service_deployment::Variables {
                environment_id: linked_project.environment.clone(),
                project_id: linked_project.project.clone(),
                service_id: service_name.node.id.clone(),
            },
            service_name.node.name.clone(),
        )
    } else if let Some(ref service) = linked_project.service {
        let service_name = body
            .project
            .services
            .edges
            .iter()
            .find(|edge| edge.node.id == *service)
            .context(SERVICE_NOT_FOUND)?;
        (
            queries::variables_for_service_deployment::Variables {
                environment_id: linked_project.environment.clone(),
                project_id: linked_project.project.clone(),
                service_id: service.clone(),
            },
            service_name.node.name.clone(),
        )
    } else {
        bail!(NO_SERVICE_LINKED);
    };

    let res = post_graphql::<queries::VariablesForServiceDeployment, _>(
        &client,
        configs.get_backboard(),
        vars,
    )
    .await?;

    let body = res.data.context("Failed to retrieve response body")?;

    if body.variables_for_service_deployment.is_empty() {
        eprintln!("No variables found");
        return Ok(());
    }

    if args.kv {
        for (key, value) in body.variables_for_service_deployment {
            println!("{}={}", key, value);
        }
        return Ok(());
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&body.variables_for_service_deployment)?
        );
        return Ok(());
    }

    let table = Table::new(name, body.variables_for_service_deployment);
    table.print()?;

    Ok(())
}

struct Plugin<'a>(&'a ProjectProjectPluginsEdgesNode);

impl<'a> Display for Plugin<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match &self.0.name {
                PluginType::mongodb => "MongoDB",
                PluginType::mysql => "MySQL",
                PluginType::postgresql => "PostgreSQL",
                PluginType::redis => "Redis",
                PluginType::Other(plugin) => plugin,
            }
        )
    }
}
