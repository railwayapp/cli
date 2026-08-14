use colored::*;
use inquire::{
    Autocomplete,
    validator::{Validation, ValueRequiredValidator},
};
use std::{
    borrow::Cow,
    fmt::Display,
    path::{MAIN_SEPARATOR, Path, PathBuf},
};

use crate::{
    commands::{Configs, queries::project::ProjectProjectServicesEdgesNode},
    controllers::project::ProjectServiceInstanceNode,
    controllers::variables::Variable,
};
use anyhow::{Context, Result};

/// Set while a full-screen TUI holds the terminal — raw mode, alternate
/// screen, and its own crossterm event reader. An inquire prompt started then
/// can never complete: the TUI's event stream consumes the keystrokes while
/// the prompt blocks forever, and the next frame paints over its output.
static TERMINAL_OWNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Mark the terminal as owned (or released) by a full-screen TUI. While set,
/// every prompt in this module fails fast instead of deadlocking, and
/// `reporter::warn` holds its output back rather than painting over the frame.
///
/// Releasing flushes those held-back warnings, so a TUI gets that for free from
/// its existing `restore_terminal` and cannot forget to ask for it.
pub fn set_terminal_owned(owned: bool) {
    TERMINAL_OWNED.store(owned, std::sync::atomic::Ordering::SeqCst);
    if !owned {
        crate::util::reporter::flush_deferred();
    }
}

/// Whether a full-screen TUI currently owns the terminal. Code that would
/// prompt as a fallback should treat this the same as a non-interactive stdin.
pub fn terminal_owned() -> bool {
    TERMINAL_OWNED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Refuse to prompt while a TUI owns the terminal: loud in debug builds so
/// the call site gets fixed, a clean error in release builds so the user gets
/// a message instead of a hang.
fn ensure_promptable() -> Result<()> {
    if terminal_owned() {
        debug_assert!(
            false,
            "interactive prompt attempted while a TUI owns the terminal"
        );
        anyhow::bail!("an interactive prompt can't run while the TUI holds the terminal");
    }
    Ok(())
}

pub fn prompt_options<T: Display>(message: &str, options: Vec<T>) -> Result<T> {
    ensure_promptable()?;
    let select = inquire::Select::new(message, options);
    select
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for options")
}

pub fn prompt_options_skippable<T: Display>(message: &str, options: Vec<T>) -> Result<Option<T>> {
    ensure_promptable()?;
    let select = inquire::Select::new(message, options);
    select
        .with_render_config(Configs::get_render_config())
        .prompt_skippable()
        .context("Failed to prompt for options")
}

pub fn prompt_options_skippable_with_default<T: Display>(
    message: &str,
    options: Vec<T>,
    default_index: usize,
) -> Result<Option<T>> {
    ensure_promptable()?;
    let select = inquire::Select::new(message, options);
    select
        .with_render_config(Configs::get_render_config())
        .with_starting_cursor(default_index)
        .prompt_skippable()
        .context("Failed to prompt for options")
}

pub fn prompt_text(message: &str) -> Result<String> {
    ensure_promptable()?;
    let select = inquire::Text::new(message);
    select
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for options")
}

/// Prompt for a secret (token, key): input is masked as it's typed and never
/// echoed back, with no confirmation round-trip.
pub fn prompt_secret(message: &str) -> Result<String> {
    ensure_promptable()?;
    inquire::Password::new(message)
        .with_render_config(Configs::get_render_config())
        .with_display_mode(inquire::PasswordDisplayMode::Masked)
        .without_confirmation()
        .prompt()
        .context("Failed to prompt for secret")
}

pub fn prompt_u64_with_placeholder_and_validation_and_cancel(
    message: &str,
    placeholder: &str,
) -> Result<Option<String>> {
    ensure_promptable()?;
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
    ensure_promptable()?;
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
    ensure_promptable()?;
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
    ensure_promptable()?;
    let select = inquire::Text::new(message);
    select
        .with_render_config(Configs::get_render_config())
        .with_placeholder(placeholder)
        .prompt_skippable()
        .context("Failed to prompt for options")
}

pub fn prompt_confirm_with_default(message: &str, default: bool) -> Result<bool> {
    ensure_promptable()?;
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
    ensure_promptable()?;
    let confirm = inquire::Confirm::new(message);
    confirm
        .with_default(default)
        .with_render_config(Configs::get_render_config())
        .prompt_skippable()
        .context("Failed to prompt for confirm")
}

pub fn prompt_multi_options<T: Display>(message: &str, options: Vec<T>) -> Result<Vec<T>> {
    ensure_promptable()?;
    let multi_select = inquire::MultiSelect::new(message, options);
    multi_select
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for multi options")
}

pub fn prompt_select<T: Display>(message: &str, options: Vec<T>) -> Result<T> {
    ensure_promptable()?;
    inquire::Select::new(message, options)
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for select")
}

pub fn prompt_select_with_cancel<T: Display>(message: &str, options: Vec<T>) -> Result<Option<T>> {
    ensure_promptable()?;
    inquire::Select::new(message, options)
        .with_render_config(Configs::get_render_config())
        .prompt_skippable()
        .context("Failed to prompt for select")
}

pub fn fake_select(message: &str, selected: &str) {
    eprintln!("{} {} {}", ">".green(), message, selected.cyan().bold());
}

pub fn prompt_variables(service_name: Option<&str>) -> Result<Vec<Variable>> {
    let mut variables: Vec<Variable> = Vec::new();
    loop {
        if let Some(variable) = prompt_text_with_placeholder_disappear_skippable(
            if let Some(ref service_name) = service_name {
                format!("Enter a variable for {service_name} <esc to skip>")
            } else {
                String::from("Enter a variable <esc to skip>")
            }
            .as_str(),
            "<KEY=VALUE, press esc to finish>",
        )? {
            if variable.is_empty() {
                break Ok(variables);
            }
            match variable.parse::<Variable>() {
                Ok(v) => variables.push(v),
                Err(err) => eprintln!("{} {:?}", "Warn".yellow(), err),
            }
        } else {
            break Ok(variables);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptService<'a>(pub &'a ProjectProjectServicesEdgesNode);

impl Display for PromptService<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptServiceInstance<'a>(pub &'a ProjectServiceInstanceNode);

impl Display for PromptServiceInstance<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.service_name)
    }
}

/// Bash style completion of paths
#[derive(Clone)]
pub struct PathAutocompleter;

impl PathAutocompleter {
    /// Parse input path and extract directory and filename prefix
    fn parse_input(input: &str) -> (Cow<'_, Path>, Cow<'_, str>) {
        if input.is_empty() {
            return (Cow::Borrowed(Path::new(".")), Cow::Borrowed(""));
        }

        let path = Path::new(input);

        // Check if input ends with a path separator
        if input.ends_with(MAIN_SEPARATOR) {
            (Cow::Borrowed(path), Cow::Borrowed(""))
        } else if !input.contains(MAIN_SEPARATOR) {
            // Input is just a filename with no path separators - search in current directory
            (Cow::Borrowed(Path::new(".")), Cow::Borrowed(input))
        } else {
            let parent = path.parent().unwrap_or(Path::new("."));
            let prefix = path.file_name().and_then(|s| s.to_str()).unwrap_or("");

            (Cow::Borrowed(parent), Cow::Borrowed(prefix))
        }
    }

    /// Build the completion path from components
    fn build_completion(input: &str, dir: &Path, filename: &str, is_dir: bool) -> String {
        let mut result = if input.ends_with(MAIN_SEPARATOR) {
            // Input ends with separator, append filename directly
            format!("{input}{filename}")
        } else if dir == Path::new(".") && !input.contains(MAIN_SEPARATOR) {
            // Current directory and input has no path separators, use filename only
            filename.to_string()
        } else if dir == Path::new(".") {
            // Current directory but input had separators, preserve the ./ format
            format!(".{MAIN_SEPARATOR}{filename}")
        } else {
            // Build full path
            let mut path = dir.to_string_lossy().into_owned();
            if !path.ends_with(MAIN_SEPARATOR) {
                path.push(MAIN_SEPARATOR);
            }
            path.push_str(filename);
            path
        };

        if is_dir {
            result.push(MAIN_SEPARATOR);
        }

        result
    }
}

impl Autocomplete for PathAutocompleter {
    fn get_suggestions(&mut self, _input: &str) -> Result<Vec<String>, inquire::CustomUserError> {
        Ok(vec![]) // Hide suggestion list for bash-style completion
    }

    fn get_completion(
        &mut self,
        input: &str,
        _highlighted_suggestion: Option<String>,
    ) -> Result<inquire::autocompletion::Replacement, inquire::CustomUserError> {
        let (dir, prefix) = Self::parse_input(input);

        // Early return if directory doesn't exist or can't be read
        if !dir.exists() || !dir.is_dir() {
            return Ok(inquire::autocompletion::Replacement::None);
        }

        let entries = match std::fs::read_dir(&*dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(inquire::autocompletion::Replacement::None),
        };

        let mut matches = Vec::new();

        // Collect all matching entries
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name_str) = file_name.to_str() else {
                continue;
            };

            if !name_str.starts_with(&*prefix) {
                continue;
            }

            let is_dir = entry.file_type().is_ok_and(|ft| ft.is_dir());
            let completion = Self::build_completion(input, &dir, name_str, is_dir);
            matches.push((name_str.to_string(), completion));
        }

        if matches.is_empty() {
            return Ok(inquire::autocompletion::Replacement::None);
        }

        // Check for exact match first (e.g., "test" matches "test" exactly, not "test-two")
        if let Some((_, completion)) = matches.iter().find(|(name, _)| name == &*prefix) {
            return Ok(inquire::autocompletion::Replacement::Some(
                completion.clone(),
            ));
        }

        // Find the closest match (shortest name that starts with prefix)
        let closest_match = matches.iter().min_by_key(|(name, _)| name.len()).unwrap();

        Ok(inquire::autocompletion::Replacement::Some(
            closest_match.1.clone(),
        ))
    }
}

pub fn prompt_path(message: &str) -> Result<PathBuf> {
    ensure_promptable()?;
    inquire::Text::new(message)
        .with_autocomplete(PathAutocompleter)
        .with_render_config(Configs::get_render_config())
        .prompt()
        .map(PathBuf::from)
        .context("Failed to prompt for path")
}

pub fn prompt_path_with_default(message: &str, default: &str) -> Result<PathBuf> {
    ensure_promptable()?;
    inquire::Text::new(message)
        .with_autocomplete(PathAutocompleter)
        .with_render_config(Configs::get_render_config())
        .with_default(default)
        .prompt()
        .map(PathBuf::from)
        .context("Failed to prompt for path")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard is what turns "prompt under the TUI" from a silent deadlock
    /// (two crossterm readers racing for the same keystrokes) into a loud
    /// failure: an assert in debug builds, a clean error in release.
    #[test]
    fn prompts_refuse_while_a_tui_owns_the_terminal() {
        set_terminal_owned(true);
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(ensure_promptable);
        std::panic::set_hook(hook);
        set_terminal_owned(false);
        match result {
            // Release builds: the prompt call fails instead of hanging.
            Ok(outcome) => assert!(outcome.is_err()),
            // Debug builds: the debug_assert fired, naming the call site.
            Err(_) => {}
        }
        assert!(ensure_promptable().is_ok());
    }
}
