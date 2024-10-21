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

    for db in databases {
        deploy::fetch_and_create(
            &client,
            &configs,
            db.to_slug().to_string(),
            &linked_project,
            &HashMap::new(),
        )
        .await?;
    }

    Ok(())
}
