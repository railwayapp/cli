use std::fmt::Display;

use crate::commands::Configs;
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

pub fn prompt_multi_options<T: Display>(message: &str, options: Vec<T>) -> Result<Vec<T>> {
    let multi_select = inquire::MultiSelect::new(message, options);
    multi_select
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for multi options")
}
