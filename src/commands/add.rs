use anyhow::bail;
use is_terminal::IsTerminal;
use std::collections::HashMap;
use strum::IntoEnumIterator;

use crate::{
    controllers::{database::DatabaseType, project::ensure_project_and_environment_exist},
    util::prompt::prompt_multi_options,
};

use super::*;

/// Provision a database into your project
#[derive(Parser)]
pub struct Args {
    /// The name of the database to add
    #[arg(short, long, value_enum)]
    database: Vec<DatabaseType>,
    /// The "{key}={value}" environment variable pair to set the template variables
    ///
    /// To specify the variable for a single service prefix it with "{service}."
    /// Example:
    ///
    /// railway deploy -t postgres -v "MY_SPECIAL_ENV_VAR=1" -v "Backend.Port=3000"
    #[arg(short, long)]
    variable: Vec<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let databases = if args.database.is_empty() {
        if !std::io::stdout().is_terminal() {
            bail!("No database specified");
        }
        prompt_multi_options("Select databases to add", DatabaseType::iter().collect())?
    } else {
        args.database
    };

    if databases.is_empty() {
        bail!("No database selected");
    }

    let variables: HashMap<String, String> = args
        .variable
        .iter()
        .map(|v| {
            let mut split = v.split('=');
            let key = split.next().unwrap_or_default().trim().to_owned();
            let value = split.collect::<Vec<&str>>().join("=").trim().to_owned();
            (key, value)
        })
        .filter(|(_, value)| !value.is_empty())
        .collect();

    for db in databases {
        if std::io::stdout().is_terminal() {
            deploy::fetch_and_create(&client, &configs, db.to_slug(), &linked_project, &variables)
                .await?;
        } else {
            println!("Creating {}...", db);
            deploy::fetch_and_create(&client, &configs, db.to_slug(), &linked_project, &variables)
                .await?;
        }
    }

    Ok(())
}
