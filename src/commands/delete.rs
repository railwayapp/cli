use std::time::Duration;

use anyhow::bail;
use is_terminal::IsTerminal;

use crate::{
    consts::TICK_STRING,
    controllers::project::get_project,
    errors::RailwayError,
    interact_or,
    util::prompt::{prompt_confirm, prompt_multi_options, prompt_text},
};

use super::*;

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

        let info =
            post_graphql::<queries::TwoFactorInfo, _>(&client, configs.get_backboard(), vars)
                .await?
                .two_factor_info;

        info.is_verified
    };

    if is_two_factor_enabled {
        let token = prompt_text("Enter your 2FA code")?;
        let vars = mutations::validate_two_factor::Variables { token };

        let valid =
            post_graphql::<mutations::ValidateTwoFactor, _>(&client, configs.get_backboard(), vars)
                .await?
                .two_factor_info_validate;

        if !valid {
            return Err(RailwayError::InvalidTwoFactorCode.into());
        }
    }

    let project = get_project(&client, &configs, linked_project.project).await?;

    let nodes = project.plugins.edges;
    let project_plugins: Vec<_> = nodes.iter().map(|p| p.node.name.to_string()).collect();
    let selected = prompt_multi_options("Select plugins to delete", project_plugins)?;

    for plugin in selected {
        let id = nodes
            .iter()
            .find(|p| p.node.name.to_string() == plugin)
            .ok_or_else(|| RailwayError::PluginNotFound(plugin.clone()))?
            .node
            .id
            .clone();

        let vars = mutations::plugin_delete::Variables { id };

        let confirmed =
            prompt_confirm(format!("Are you sure you want to delete {plugin}?").as_str())?;

        if !confirmed {
            return Ok(());
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
