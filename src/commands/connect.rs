use anyhow::bail;
use tokio::process::Command;

use crate::commands::queries::project_plugins::PluginType;
use crate::consts::PLUGIN_NOT_FOUND;
use crate::controllers::variables::{get_plugin_variables};
use crate::util::prompt::{prompt_select, PromptPlugin};

use super::{*};

/// Change the active environment
#[derive(Parser)]
pub struct Args {
    /// The name of the plugin to connect to
    plugin_name: Option<String>,

    /// Environment to pull variables from (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let environment = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());

    let vars = queries::project_plugins::Variables {
        id: linked_project.project.to_owned(),
    };
    let res = post_graphql::<queries::ProjectPlugins, _>(&client, configs.get_backboard(), vars).await?;
    let body = res.data.context("Failed to retrieve response body")?;

    let plugin = match args.plugin_name {
        Some(name) => &body
            .project
            .plugins
            .edges
            .iter()
            .find(|edge| edge.node.friendly_name == name)
            .context(PLUGIN_NOT_FOUND)?.node,
        None => {
            let plugins: Vec<_> = body.project.plugins.edges.iter().map(|p| PromptPlugin(&p.node)).collect();
            if plugins.is_empty() {
                bail!("No plugins found");
            }
            prompt_select("Select a plugin", plugins).context("No")?.0
        }
    };

    let vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };
    let res = post_graphql::<queries::Project, _>(&client, configs.get_backboard(), vars).await?;
    let body = res.data.context("Failed to get project (query project)")?;
    let environment_id = body
        .project
        .environments
        .edges
        .iter()
        .find(|env| env.node.name == environment || env.node.id == environment)
        .map(|env| env.node.id.to_owned())
        .context("Environment not found")?;

    let variables = get_plugin_variables(&client, &configs, linked_project.project, environment_id, plugin.id.clone()).await?;

    match plugin.name {
        PluginType::postgresql => {
            let _ = Command::new("psql")
                .arg("-U")
                .arg(variables.get("PGUSER").unwrap_or(&"".to_string()))
                .arg("-h")
                .arg(variables.get("PGHOST").unwrap_or(&"".to_string()))
                .arg("-p")
                .arg(variables.get("PGPORT").unwrap_or(&"".to_string()))
                .arg("-d")
                .arg(variables.get("PGDATABASE").unwrap_or(&"".to_string()))
                .env("PGPASSWORD", variables.get("PGPASSWORD").unwrap_or(&"".to_string()))
                .spawn()
                .expect("ls command failed to start")
                .wait()
                .await
                .expect("ls command failed to run");
        }
        PluginType::redis => {
            let _ = Command::new("redis-cli")
                .arg("-U")
                .arg(variables.get("PGUSER").unwrap_or(&"".to_string()))
                .arg("-h")
                .arg(variables.get("PGHOST").unwrap_or(&"".to_string()))
                .arg("-p")
                .arg(variables.get("PGPORT").unwrap_or(&"".to_string()))
                .arg("-d")
                .arg(variables.get("PGDATABASE").unwrap_or(&"".to_string()))
                .env("PGPASSWORD", variables.get("PGPASSWORD").unwrap_or(&"".to_string()))
                .spawn()
                .expect("ls command failed to start")
                .wait()
                .await
                .expect("ls command failed to run");
        }
    };

    Ok(())
}
