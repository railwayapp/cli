use std::fmt::Display;

use crate::commands::queries::project_plugins::ProjectPluginsProjectPluginsEdgesNode;
use crate::commands::{queries::project::ProjectProjectServicesEdgesNode, Configs};
use anyhow::{Context, Result};

pub fn prompt_options<T: Display>(message: &str, options: Vec<T>) -> Result<T> {
    let select = inquire::Select::new(message, options);
    select
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for options")
}

pub fn prompt_confirm(message: &str) -> Result<bool> {
    let confirm = inquire::Confirm::new(message);
    confirm
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for confirm")
}

pub fn prompt_confirm_with_default(message: &str, default: bool) -> Result<bool> {
    let confirm = inquire::Confirm::new(message);
    confirm
        .with_default(default)
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for confirm")
}

pub fn prompt_multi_options<T: Display>(message: &str, options: Vec<T>) -> Result<Vec<T>> {
    let multi_select = inquire::MultiSelect::new(message, options);
    multi_select
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for multi options")
}

pub fn prompt_text(message: &str) -> Result<String> {
    let text = inquire::Text::new(message);
    text.with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for text")
}

pub fn prompt_select<T: Display>(message: &str, options: Vec<T>) -> Result<T> {
    inquire::Select::new(message, options)
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for select")
}

#[derive(Debug, Clone)]
pub struct PromptService<'a>(pub &'a ProjectProjectServicesEdgesNode);

impl<'a> Display for PromptService<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}

#[derive(Debug, Clone)]
pub struct PromptPlugin<'a>(pub &'a ProjectPluginsProjectPluginsEdgesNode);

impl<'a> Display for PromptPlugin<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.friendly_name)
    }
}
