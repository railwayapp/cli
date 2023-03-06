use std::time::Duration;

use anyhow::bail;
use is_terminal::IsTerminal;

use crate::{
    consts::{ABORTED_BY_USER, TICK_STRING},
    interact_or,
    util::prompt::{prompt_confirm, prompt_multi_options, prompt_text},
};

use super::{queries::project_plugins::PluginType, *};

/// Delete plugins from a project
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    interact_or!("Cannot delete plugins in non-interactive mode");

    let configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let is_two_factor_enabled = {
        let vars = queries::two_factor_info::Variables {};

        let res = post_graphql::<queries::TwoFactorInfo, _>(&client, configs.get_backboard(), vars)
            .await?;
        let info = res.data.context("No data")?.two_factor_info;

        info.is_verified
    };

    if is_two_factor_enabled {
        let token = prompt_text("Enter your 2FA code")?;
        let vars = mutations::validate_two_factor::Variables { token };

        let res =
            post_graphql::<mutations::ValidateTwoFactor, _>(&client, configs.get_backboard(), vars)
                .await?;
        let valid = res.data.context("No data")?.two_factor_info_validate;

        if !valid {
            bail!("Invalid 2FA code");
        }
    }

    let vars = queries::project_plugins::Variables {
        id: linked_project.project.clone(),
    };

    let res =
        post_graphql::<queries::ProjectPlugins, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;
    let nodes = body.project.plugins.edges;
    let project_plugins: Vec<_> = nodes
        .iter()
        .map(|p| plugin_enum_to_string(&p.node.name))
        .collect();
    let selected = prompt_multi_options("Select plugins to delete", project_plugins)?;

    for plugin in selected {
        let id = nodes
            .iter()
            .find(|p| plugin_enum_to_string(&p.node.name) == plugin)
            .context("Plugin not found")?
            .node
            .id
            .clone();

        let vars = mutations::plugin_delete::Variables { id };

        let confirmed =
            prompt_confirm(format!("Are you sure you want to delete {plugin}?").as_str())?;

        if !confirmed {
            bail!(ABORTED_BY_USER)
        }

        let spinner = indicatif::ProgressBar::new_spinner()
            .with_style(
                indicatif::ProgressStyle::default_spinner()
                    .tick_chars(TICK_STRING)
                    .template("{spinner:.green} {msg}")?,
            )
            .with_message(format!("Deleting {plugin}..."));
        spinner.enable_steady_tick(Duration::from_millis(100));

        post_graphql::<mutations::PluginDelete, _>(&client, configs.get_backboard(), vars).await?;

        spinner.finish_with_message(format!("Deleted {plugin}"));
    }
    Ok(())
}

fn plugin_enum_to_string(plugin: &PluginType) -> String {
    match plugin {
        PluginType::postgresql => "PostgreSQL".to_owned(),
        PluginType::mysql => "MySQL".to_owned(),
        PluginType::redis => "Redis".to_owned(),
        PluginType::mongodb => "MongoDB".to_owned(),
        PluginType::Other(other) => other.to_owned(),
    }
}
