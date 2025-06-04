use colored::*;
use inquire::{
    type_aliases::Suggester,
    validator::{Validation, ValueRequiredValidator},
    Autocomplete, CustomType,
};
use std::{
    fmt::Display,
    path::{Path, PathBuf},
};

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

pub fn prompt_u64_with_placeholder_and_validation_and_cancel(
    message: &str,
    placeholder: &str,
) -> Result<Option<String>> {
    let validator = |input: &str| {
        if input.parse::<u64>().is_ok() {
            Ok(Validation::Valid)
        } else {
            Ok(Validation::Invalid("Not a valid number".into()))
        }
    };
    let select = inquire::Text::new(message);
    select
        .with_render_config(Configs::get_render_config())
        .with_placeholder(placeholder)
        .with_validator(ValueRequiredValidator::new("Input most not be empty"))
        .with_validator(validator)
        .prompt_skippable()
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

pub fn prompt_text_with_placeholder_disappear_skippable(
    message: &str,
    placeholder: &str,
) -> Result<Option<String>> {
    let select = inquire::Text::new(message);
    select
        .with_render_config(Configs::get_render_config())
        .with_placeholder(placeholder)
        .prompt_skippable()
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

pub fn prompt_select_with_cancel<T: Display>(message: &str, options: Vec<T>) -> Result<Option<T>> {
    inquire::Select::new(message, options)
        .with_render_config(Configs::get_render_config())
        .prompt_skippable()
        .context("Failed to prompt for select")
}

pub fn fake_select(message: &str, selected: &str) {
    println!("{} {} {}", ">".green(), message, selected.cyan().bold());
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptService<'a>(pub &'a ProjectProjectServicesEdgesNode);

impl Display for PromptService<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}

#[derive(Clone)]
pub struct PathAutocompleter;
impl Autocomplete for PathAutocompleter {
    fn get_suggestions(&mut self, _input: &str) -> Result<Vec<String>, inquire::CustomUserError> {
        // Return empty suggestions to hide the suggestion list
        Ok(vec![])
    }

    fn get_completion(
        &mut self,
        input: &str,
        _highlighted_suggestion: Option<String>,
    ) -> Result<inquire::autocompletion::Replacement, inquire::CustomUserError> {
        let path = Path::new(input);
        let (dir, prefix) = if input.ends_with('/') || input.ends_with('\\') {
            (path.to_path_buf(), String::new())
        } else {
            let parent = path.parent().unwrap_or(Path::new("."));
            let dir = if parent.as_os_str().is_empty() {
                Path::new(".") // Fix: if parent is empty, use current directory
            } else {
                parent
            };
            
            (
                dir.to_path_buf(),
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string(),
            )
        };

        let mut matches = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Some(name_str) = entry.file_name().to_str() {
                    if name_str.starts_with(&prefix) {
                        let suggestion = if input.ends_with('/') || input.ends_with('\\') {
                            format!("{}{}", input, name_str)
                        } else {
                            // Preserve the full path structure
                            if let Some(parent) = path.parent() {
                                if parent == Path::new(".") {
                                    // Current directory - just use the filename
                                    name_str.to_string()
                                } else {
                                    // Other directory (like ../sushibot) - preserve the parent path
                                    format!("{}/{}", parent.display(), name_str)
                                }
                            } else {
                                name_str.to_string()
                            }
                        };

                        if entry.path().is_dir() {
                            matches.push(format!("{}/", suggestion));
                        } else {
                            matches.push(suggestion);
                        }
                    }
                }
            }
        }

        // Return the first match if any
        if !matches.is_empty() {
            Ok(inquire::autocompletion::Replacement::Some(matches[0].clone()))
        } else {
            Ok(inquire::autocompletion::Replacement::None)
        }
    }
}

fn find_common_prefix(strings: &[String]) -> String {
    if strings.is_empty() {
        return String::new();
    }

    let first = &strings[0];
    let mut prefix = String::new();

    for (i, ch) in first.chars().enumerate() {
        if strings.iter().all(|s| s.chars().nth(i) == Some(ch)) {
            prefix.push(ch);
        } else {
            break;
        }
    }

    prefix
}

pub fn prompt_path(message: &str) -> Result<PathBuf> {
    let input = inquire::Text::new(message)
        .with_autocomplete(PathAutocompleter)
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for path")?;

    Ok(PathBuf::from(input))
}
