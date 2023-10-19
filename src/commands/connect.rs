use anyhow::bail;
use std::{collections::BTreeMap, fmt::Display};
use tokio::process::Command;
use which::which;

use crate::controllers::project::get_plugin_or_service;
use crate::controllers::{
    environment::get_matched_environment,
    project::{get_project, PluginOrService},
    variables::get_plugin_or_service_variables,
};
use crate::errors::RailwayError;
use crate::util::prompt::prompt_select;

use super::{queries::project::PluginType, *};

/// Connect to a plugin's shell (psql for Postgres, mongosh for MongoDB, etc.)
#[derive(Parser)]
pub struct Args {
    /// The name of the plugin to connect to
    service_name: Option<String>,

    /// Environment to pull variables from (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,
}

impl Display for PluginOrService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginOrService::Plugin(plugin) => write!(f, "{}", plugin.friendly_name),
            PluginOrService::Service(service) => write!(f, "{}", service.name),
        }
    }
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

    let plugin_or_service = args
        .service_name
        .clone()
        .map(|name| get_plugin_or_service(&project, name))
        .unwrap_or_else(|| {
            let mut nodes_to_prompt: Vec<PluginOrService> = Vec::new();
            for plugin in &project.plugins.edges {
                nodes_to_prompt.push(PluginOrService::Plugin(plugin.node.clone()));
            }
            for service in &project.services.edges {
                nodes_to_prompt.push(PluginOrService::Service(service.node.clone()));
            }

            if nodes_to_prompt.is_empty() {
                return Err(RailwayError::ProjectHasNoServicesOrPlugins.into());
            }

            prompt_select("Select service", nodes_to_prompt).context("No service selected")
        })?;

    let environment_id = get_matched_environment(&project, environment)?.id;

    let variables = get_plugin_or_service_variables(
        &client,
        &configs,
        linked_project.project,
        environment_id.clone(),
        &plugin_or_service,
    )
    .await?;

    let plugin_type = plugin_or_service
        .get_plugin_type(environment_id)
        .ok_or_else(|| RailwayError::UnknownDatabaseType(plugin_or_service.get_name()))?;

    let (cmd_name, args) = get_connect_command(plugin_type, variables)?;

    if which(cmd_name.clone()).is_err() {
        bail!("{} must be installed to continue", cmd_name);
    }

    Command::new(cmd_name.as_str())
        .args(args)
        .spawn()?
        .wait()
        .await?;

    Ok(())
}

impl PluginOrService {
    pub fn get_name(&self) -> String {
        match self {
            PluginOrService::Plugin(plugin) => plugin.friendly_name.clone(),
            PluginOrService::Service(service) => service.name.clone(),
        }
    }

    pub fn get_plugin_type(&self, environment_id: String) -> Option<PluginType> {
        match self {
            PluginOrService::Plugin(plugin) => Some(plugin.name.clone()),
            PluginOrService::Service(service) => {
                let service_instance = service
                    .service_instances
                    .edges
                    .iter()
                    .find(|si| si.node.environment_id == environment_id);

                service_instance
                    .and_then(|si| si.node.source.clone())
                    .and_then(|source| source.image)
                    .map(|image: String| image.to_lowercase())
                    .and_then(|image: String| {
                        if image.contains("postgres") {
                            Some(PluginType::postgresql)
                        } else if image.contains("redis") {
                            Some(PluginType::redis)
                        } else if image.contains("mongo") {
                            Some(PluginType::mongodb)
                        } else if image.contains("mysql") {
                            Some(PluginType::mysql)
                        } else {
                            None
                        }
                    })
            }
        }
    }
}

fn get_connect_command(
    plugin_type: PluginType,
    variables: BTreeMap<String, String>,
) -> Result<(String, Vec<String>)> {
    let pass_arg; // Hack to get ownership of formatted string outside match
    let default = &"".to_string();

    let (cmd_name, args): (&str, Vec<&str>) = match &plugin_type {
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

    Ok((
        cmd_name.to_string(),
        args.iter().map(|s| s.to_string()).collect(),
    ))
}
