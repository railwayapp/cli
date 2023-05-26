use anyhow::bail;
use tokio::process::Command;
use which::which;

use crate::controllers::variables::get_plugin_variables;
use crate::controllers::{environment::get_matched_environment, project::get_project};
use crate::errors::RailwayError;
use crate::util::prompt::{prompt_select, PromptPlugin};

use super::{queries::project::PluginType, *};

/// Connect to a plugin's shell (psql for Postgres, mongosh for MongoDB, etc.)
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

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let plugin = match args.plugin_name {
        Some(name) => {
            &project
                .plugins
                .edges
                .iter()
                .find(|edge| edge.node.friendly_name == name)
                .ok_or_else(|| RailwayError::PluginNotFound(name))?
                .node
        }
        None => {
            let plugins: Vec<_> = project
                .plugins
                .edges
                .iter()
                .map(|p| PromptPlugin(&p.node))
                .collect();
            if plugins.is_empty() {
                return Err(RailwayError::ProjectHasNoPlugins.into());
            }
            prompt_select("Select a plugin", plugins)
                .context("No plugin selected")?
                .0
        }
    };

    let environment_id = get_matched_environment(&project, environment)?.id;

    let variables = get_plugin_variables(
        &client,
        &configs,
        linked_project.project,
        environment_id,
        plugin.id.clone(),
    )
    .await?;

    let pass_arg; // Hack to get ownership of formatted string outside match
    let default = &"".to_string();
    let (cmd_name, args): (&str, Vec<&str>) = match &plugin.name {
        PluginType::postgresql => (
            "psql",
            vec![variables.get("DATABASE_URL").unwrap_or(default)],
        ),
        PluginType::redis => (
            "redis-cli",
            vec!["-u", variables.get("REDIS_URL").unwrap_or(default)],
        ),
        PluginType::mongodb => (
            "mongosh",
            vec![variables.get("MONGO_URL").unwrap_or(default).as_str()],
        ),
        PluginType::mysql => {
            // -p is a special case as it requires no whitespace between arg and value
            pass_arg = format!("-p{}", variables.get("MYSQLPASSWORD").unwrap_or(default));
            (
                "mysql",
                vec![
                    "-h",
                    variables.get("MYSQLHOST").unwrap_or(default),
                    "-u",
                    variables.get("MYSQLUSER").unwrap_or(default),
                    "-P",
                    variables.get("MYSQLPORT").unwrap_or(default),
                    "-D",
                    variables.get("MYSQLDATABASE").unwrap_or(default),
                    pass_arg.as_str(),
                ],
            )
        }
        PluginType::Other(o) => bail!("Unsupported plugin type {}", o),
    };

    if which(cmd_name).is_err() {
        bail!("{} must be installed to continue", cmd_name);
    }

    Command::new(cmd_name).args(args).spawn()?.wait().await?;

    Ok(())
}
