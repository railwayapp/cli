use std::fmt::Display;

use anyhow::bail;
use is_terminal::IsTerminal;

use crate::{
    consts::{NO_SERVICE_LINKED, SERVICE_NOT_FOUND},
    table::Table,
    util::prompt::prompt_select,
};

use super::{
    queries::project::{PluginType, ProjectProjectPluginsEdgesNode},
    *,
};

/// Show variables for active environment
#[derive(Parser)]
pub struct Args {
    /// Show variables for a plugin
    #[clap(short, long)]
    plugin: bool,

    /// Show variables for a specific service
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
    let plugins: Vec<_> = body
        .project
        .plugins
        .edges
        .iter()
        .map(|plugin| Plugin(&plugin.node))
        .collect();

    let (vars, name) = if args.plugin {
        if plugins.is_empty() {
            bail!("No plugins found");
        }
        let plugin = prompt_plugin(plugins)?;
        (
            queries::variables::Variables {
                environment_id: linked_project.environment.clone(),
                project_id: linked_project.project.clone(),
                service_id: None,
                plugin_id: Some(plugin.0.id.clone()),
            },
            format!("{plugin}"),
        )
    } else if let Some(ref service) = args.service {
        let service_name = body
            .project
            .services
            .edges
            .iter()
            .find(|edge| edge.node.id == *service || edge.node.name == *service)
            .context(SERVICE_NOT_FOUND)?;
        (
            queries::variables::Variables {
                environment_id: linked_project.environment.clone(),
                project_id: linked_project.project.clone(),
                service_id: Some(service_name.node.id.clone()),
                plugin_id: None,
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
            queries::variables::Variables {
                environment_id: linked_project.environment.clone(),
                project_id: linked_project.project.clone(),
                service_id: Some(service.clone()),
                plugin_id: None,
            },
            service_name.node.name.clone(),
        )
    } else {
        if plugins.is_empty() {
            bail!(NO_SERVICE_LINKED);
        }
        let plugin = prompt_plugin(plugins)?;
        (
            queries::variables::Variables {
                environment_id: linked_project.environment.clone(),
                project_id: linked_project.project.clone(),
                service_id: None,
                plugin_id: Some(plugin.0.id.clone()),
            },
            format!("{plugin}"),
        )
    };

    let res = post_graphql::<queries::Variables, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;

    if body.variables.is_empty() {
        eprintln!("No variables found");
        return Ok(());
    }

    if args.kv {
        for (key, value) in body.variables {
            println!("{}={}", key, value);
        }
        return Ok(());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&body.variables)?);
        return Ok(());
    }

    let table = Table::new(name, body.variables);
    table.print()?;

    Ok(())
}

fn prompt_plugin(plugins: Vec<Plugin>) -> Result<Plugin> {
    if !std::io::stdout().is_terminal() {
        bail!("Plugin must be provided when not running in a terminal")
    }
    let plugin = prompt_select("Select a plugin", plugins)?;

    Ok(plugin)
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
