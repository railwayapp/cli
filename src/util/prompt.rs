use colored::*;
use std::fmt::Display;

use crate::commands::{queries::project::ProjectProjectServicesEdgesNode, Configs};
use anyhow::{Context, Result};

pub fn prompt_options<T: Display>(message: &str, options: Vec<T>) -> Result<T> {
    let select = inquire::Select::new(message, options);
    select
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for options")
}

pub fn prompt_options_skippable<T: Display>(message: &str, options: Vec<T>) -> Result<Option<T>> {
    let select = inquire::Select::new(message, options);
    select
        .with_render_config(Configs::get_render_config())
        .prompt_skippable()
        .context("Failed to prompt for options")
}

pub fn prompt_text(message: &str) -> Result<String> {
    let select = inquire::Text::new(message);
    select
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for options")
}

pub fn prompt_text_with_placeholder_if_blank(
    message: &str,
    placeholder: &str,
    blank_message: &str,
) -> Result<String> {
    let select = inquire::Text::new(message);
    select
        .with_render_config(Configs::get_render_config())
        .with_placeholder(placeholder)
        .with_formatter(&|input: &str| {
            if input.is_empty() {
                String::from(blank_message)
            } else {
                input.to_string()
            }
        })
        .prompt()
        .context("Failed to prompt for options")
}

pub fn prompt_text_with_placeholder_disappear(message: &str, placeholder: &str) -> Result<String> {
    let select = inquire::Text::new(message);
    select
        .with_render_config(Configs::get_render_config())
        .with_placeholder(placeholder)
        .prompt()
        .context("Failed to prompt for options")
}

pub fn prompt_confirm_with_default(message: &str, default: bool) -> Result<bool> {
    let confirm = inquire::Confirm::new(message);
    confirm
        .with_default(default)
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for confirm")
}

pub fn prompt_confirm_with_default_with_cancel(
    message: &str,
    default: bool,
) -> Result<Option<bool>> {
    let confirm = inquire::Confirm::new(message);
    confirm
        .with_default(default)
        .with_render_config(Configs::get_render_config())
        .prompt_skippable()
        .context("Failed to prompt for confirm")
}

pub fn prompt_multi_options<T: Display>(message: &str, options: Vec<T>) -> Result<Vec<T>> {
    let multi_select = inquire::MultiSelect::new(message, options);
    multi_select
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for multi options")
}

pub fn prompt_select<T: Display>(message: &str, options: Vec<T>) -> Result<T> {
    inquire::Select::new(message, options)
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for select")
}

pub fn fake_select(message: &str, selected: &str) {
    println!("{} {} {}", ">".green(), message, selected.cyan().bold());
}

#[derive(Debug, Clone)]
pub struct PromptService<'a>(pub &'a ProjectProjectServicesEdgesNode);

impl Display for PromptService<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}
