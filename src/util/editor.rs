use std::{env, fmt, path::Path};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::{Context, Result, bail};
use is_terminal::IsTerminal;

use crate::{config::Configs, util::prompt::prompt_select};

pub fn resolve_editor_command(editor_override: Option<&str>) -> Result<String> {
    if let Some(editor) = editor_override
        .map(str::trim)
        .filter(|editor| !editor.is_empty())
    {
        return Ok(editor.to_string());
    }

    let mut configs = Configs::new()?;
    if let Some(editor) = configs
        .root_config
        .editor
        .as_deref()
        .map(str::trim)
        .filter(|editor| !editor.is_empty())
    {
        return Ok(editor.to_string());
    }

    if !std::io::stdout().is_terminal() {
        bail!("No editor configured. Re-run interactively or pass --editor.");
    }

    let choices = editor_choices(command_exists);
    if choices.is_empty() {
        bail!(
            "No supported editor found on PATH. Install VS Code, Cursor, Zed, Nano, or Vim, or pass --editor."
        );
    }

    let choice = prompt_select("Select an editor", choices)?;
    let editor = choice.command().to_string();

    configs.root_config.editor = Some(editor.clone());
    configs
        .write()
        .context("Failed to save editor preference")?;

    Ok(editor)
}

fn editor_choices(command_exists: impl Fn(&str) -> bool) -> Vec<EditorChoice> {
    let mut choices = Vec::new();

    if cfg!(target_os = "windows") {
        if let Some(choice) = EditorChoice::from_command_if_available(
            "Notepad",
            "notepad",
            "notepad",
            &command_exists,
        ) {
            choices.push(choice);
        }
    }

    choices.extend(
        [
            EditorChoice::from_command_if_available(
                "VS Code",
                "code",
                "code --wait",
                &command_exists,
            ),
            EditorChoice::from_command_if_available(
                "Cursor",
                "cursor",
                "cursor --wait",
                &command_exists,
            ),
            EditorChoice::from_command_if_available("Zed", "zed", "zed --wait", &command_exists),
            EditorChoice::from_command_if_available("Nano", "nano", "nano", &command_exists),
            EditorChoice::from_command_if_available("Vim", "vim", "vim", &command_exists),
        ]
        .into_iter()
        .flatten(),
    );

    choices
}

#[derive(Clone)]
enum EditorChoice {
    Command {
        label: &'static str,
        command: &'static str,
    },
}

impl EditorChoice {
    fn from_command_if_available(
        label: &'static str,
        executable: &str,
        command: &'static str,
        command_exists: impl Fn(&str) -> bool,
    ) -> Option<Self> {
        command_exists(executable).then_some(Self::Command { label, command })
    }

    fn command(&self) -> &'static str {
        match self {
            Self::Command { command, .. } => command,
        }
    }
}

impl fmt::Display for EditorChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditorChoice::Command { label, .. } => write!(f, "{label}"),
        }
    }
}

fn command_exists(command: &str) -> bool {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return is_executable(command_path);
    }

    let Some(paths) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&paths)
        .any(|path| candidate_names(command).any(|candidate| is_executable(&path.join(candidate))))
}

#[cfg(windows)]
fn candidate_names(command: &str) -> Box<dyn Iterator<Item = String> + '_> {
    if Path::new(command).extension().is_some() {
        return Box::new(std::iter::once(command.to_string()));
    }

    let pathext = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let candidates = pathext
        .split(';')
        .filter(|ext| !ext.is_empty())
        .map(|ext| format!("{command}{ext}"))
        .collect::<Vec<_>>();

    Box::new(candidates.into_iter())
}

#[cfg(not(windows))]
fn candidate_names(command: &str) -> Box<dyn Iterator<Item = String> + '_> {
    Box::new(std::iter::once(command.to_string()))
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    path.is_file()
        && path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
