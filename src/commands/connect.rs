use anyhow::bail;
use tokio::process::Command;
use which::which;

use crate::commands::queries::project_plugins::PluginType;
use crate::consts::PLUGIN_NOT_FOUND;
use crate::controllers::variables::get_plugin_variables;
use crate::util::prompt::{prompt_select, PromptPlugin};

use super::*;

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
    let res =
        post_graphql::<queries::ProjectPlugins, _>(&client, configs.get_backboard(), vars).await?;
    let body = res.data.context("Failed to retrieve response body")?;

    let plugin = match args.plugin_name {
        Some(name) => {
            &body
                .project
                .plugins
                .edges
                .iter()
                .find(|edge| edge.node.friendly_name == name)
                .context(PLUGIN_NOT_FOUND)?
                .node
        }
        None => {
            let plugins: Vec<_> = body
                .project
                .plugins
                .edges
                .iter()
                .map(|p| PromptPlugin(&p.node))
                .collect();
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
            vec![
                "-U",
                variables.get("PGUSER").unwrap_or(default),
                "-h",
                variables.get("PGHOST").unwrap_or(default),
                "-p",
                variables.get("PGPORT").unwrap_or(default),
                "-d",
                variables.get("PGDATABASE").unwrap_or(default),
            ],
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
