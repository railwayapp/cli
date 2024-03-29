use anyhow::bail;
use std::{collections::BTreeMap, fmt::Display};
use tokio::process::Command;
use which::which;

use crate::controllers::{
    database::DatabaseType, environment::get_matched_environment, project::get_project,
    variables::get_service_variables,
};
use crate::errors::RailwayError;
use crate::util::prompt::prompt_select;
use crate::{controllers::project::get_service, queries::project::ProjectProjectServicesEdgesNode};

use super::*;

/// Connect to a database's shell (psql for Postgres, mongosh for MongoDB, etc.)
#[derive(Parser)]
pub struct Args {
    /// The name of the database to connect to
    service_name: Option<String>,

    /// Environment to pull variables from (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,
}

impl Display for ProjectProjectServicesEdgesNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
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

    let service = args
        .service_name
        .clone()
        .map(|name| get_service(&project, name))
        .unwrap_or_else(|| {
            let nodes_to_prompt = project
                .services
                .edges
                .iter()
                .map(|s| s.node.clone())
                .collect::<Vec<ProjectProjectServicesEdgesNode>>();

            if nodes_to_prompt.is_empty() {
                return Err(RailwayError::ProjectHasNoServices.into());
            }

            prompt_select("Select service", nodes_to_prompt).context("No service selected")
        })?;

    let environment_id = get_matched_environment(&project, environment)?.id;

    let variables = get_service_variables(
        &client,
        &configs,
        linked_project.project,
        environment_id.clone(),
        service.name,
    )
    .await?;
    let database_type = {
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
                if image.contains("postgres")
                    || image.contains("postgis")
                    || image.contains("timescale")
                {
                    Some(DatabaseType::PostgreSQL)
                } else if image.contains("redis") {
                    Some(DatabaseType::Redis)
                } else if image.contains("mongo") {
                    Some(DatabaseType::MongoDB)
                } else if image.contains("mysql") {
                    Some(DatabaseType::MySQL)
                } else {
                    None
                }
            })
    };
    if let Some(db_type) = database_type {
        let (cmd_name, args) = get_connect_command(db_type, variables)?;

        if which(cmd_name.clone()).is_err() {
            bail!("{} must be installed to continue", cmd_name);
        }

        Command::new(cmd_name.as_str())
            .args(args)
            .spawn()?
            .wait()
            .await?;

        Ok(())
    } else {
        bail!("No supported database found in service")
    }
}

fn get_connect_command(
    plugin_type: DatabaseType,
    variables: BTreeMap<String, String>,
) -> Result<(String, Vec<String>)> {
    let pass_arg; // Hack to get ownership of formatted string outside match
    let default = &"".to_string();

    let (cmd_name, args): (&str, Vec<&str>) = match &plugin_type {
        DatabaseType::PostgreSQL => (
            "psql",
            vec![variables.get("DATABASE_URL").unwrap_or(default)],
        ),
        DatabaseType::Redis => (
            "redis-cli",
            vec!["-u", variables.get("REDIS_URL").unwrap_or(default)],
        ),
        DatabaseType::MongoDB => (
            "mongosh",
            vec![variables.get("MONGO_URL").unwrap_or(default).as_str()],
        ),
        DatabaseType::MySQL => {
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
    };

    Ok((
        cmd_name.to_string(),
        args.iter().map(|s| s.to_string()).collect(),
    ))
}
