use anyhow::bail;
use is_terminal::IsTerminal;
use std::time::Duration;
use strum::IntoEnumIterator;

use crate::{
    consts::TICK_STRING,
    controllers::{database::DatabaseType, project::get_project},
    util::prompt::prompt_multi_options,
};

use super::*;

/// Add a new plugin to your project
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

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let databases = if args.database.is_empty() {
        if !std::io::stdout().is_terminal() {
            bail!("No plugins specified");
        }
        prompt_multi_options("Select databases to add", DatabaseType::iter().collect())?
    } else {
        args.database
    };

    if selected.is_empty() {
        bail!("No plugins selected");
    }

    for plugin in selected {
        let vars = mutations::plugin_create::Variables {
            project_id: linked_project.project.clone(),
            name: plugin.to_lowercase(),
        };
        if std::io::stdout().is_terminal() {
            let spinner = indicatif::ProgressBar::new_spinner()
                .with_style(
                    indicatif::ProgressStyle::default_spinner()
                        .tick_chars(TICK_STRING)
                        .template("{spinner:.green} {msg}")?,
                )
                .with_message(format!("Creating {plugin}..."));
            spinner.enable_steady_tick(Duration::from_millis(100));
            post_graphql::<mutations::PluginCreate, _>(&client, configs.get_backboard(), vars)
                .await?;
            spinner.finish_with_message(format!("Created {plugin}"));
        } else {
            println!("Creating {}...", plugin);
            post_graphql::<mutations::PluginCreate, _>(&client, configs.get_backboard(), vars)
                .await?;
        }
    }

    Ok(())
}
