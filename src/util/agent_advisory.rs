use std::{collections::BTreeMap, fs::File, io::Read, path::PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};

use crate::util;

const STATE_VERSION: u32 = 1;
const ADVISORY_INTERVAL_HOURS: i64 = 24;
const DISABLE_ENV: &str = "RAILWAY_AGENT_ADVISORY";
const FORCE_ENV: &str = "RAILWAY_AGENT";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentState {
    #[serde(default)]
    setup: SetupState,
    #[serde(default)]
    advisories: BTreeMap<String, AdvisoryState>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupState {
    version: Option<u32>,
    last_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdvisoryState {
    last_shown_at: Option<DateTime<Utc>>,
}

fn state_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".railway").join("agent-state.json"))
}

fn read_state() -> AgentState {
    let Ok(path) = state_path() else {
        return AgentState::default();
    };
    let Ok(mut file) = File::open(path) else {
        return AgentState::default();
    };

    let mut contents = vec![];
    if file.read_to_end(&mut contents).is_err() {
        return AgentState::default();
    }

    serde_json::from_slice(&contents).unwrap_or_default()
}

fn write_state(state: &AgentState) -> Result<()> {
    let path = state_path()?;
    let contents = serde_json::to_string_pretty(state)?;
    util::write_atomic(&path, &contents)
}

fn agent_setup_is_current(state: &AgentState) -> bool {
    state.setup.version.unwrap_or_default() >= STATE_VERSION
}

fn disabled_by_env() -> bool {
    matches!(std::env::var(DISABLE_ENV).as_deref(), Ok("0" | "false" | "off"))
}

fn is_agent_environment() -> bool {
    if matches!(std::env::var(FORCE_ENV).as_deref(), Ok("1" | "true" | "yes")) {
        return true;
    }

    const AGENT_ENV_VARS: &[&str] = &[
        "AIDER",
        "CLAUDECODE",
        "CLAUDE_CODE",
        "CURSOR_AGENT",
        "FACTORY_DROID",
        "OPENCODE",
        "OPENAI_AGENT",
        "REPLIT_AGENT",
    ];

    const AGENT_ENV_PREFIXES: &[&str] = &["CLAUDE_CODE_", "CODEX_", "OPENCODE_"];

    AGENT_ENV_VARS.iter().any(|name| std::env::var_os(name).is_some())
        || std::env::vars_os().any(|(key, _)| {
            key.to_str()
                .map(|key| {
                    AGENT_ENV_PREFIXES
                        .iter()
                        .any(|prefix| key.starts_with(prefix))
                })
                .unwrap_or(false)
        })
}

fn command_is_exempt(command: &str) -> bool {
    matches!(
        command,
        "autoupdate"
            | "check_updates"
            | "check-updates"
            | "completion"
            | "docs"
            | "help"
            | "login"
            | "logout"
            | "mcp"
            | "setup"
            | "skills"
            | "telemetry"
            | "telemetry_cmd"
            | "upgrade"
            | "whoami"
    )
}

fn should_skip_for_args(raw_args: &[String]) -> bool {
    raw_args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--json" | "--help" | "-h" | "--version" | "-V"
        )
    })
}

fn advisory_is_recent(state: &AgentState, command: &str) -> bool {
    state
        .advisories
        .get(command)
        .and_then(|advisory| advisory.last_shown_at)
        .map(|last_shown_at| {
            Utc::now() - last_shown_at < chrono::Duration::hours(ADVISORY_INTERVAL_HOURS)
        })
        .unwrap_or(false)
}

pub fn maybe_show(raw_args: &[String], command: Option<&str>) {
    if disabled_by_env() || should_skip_for_args(raw_args) || !is_agent_environment() {
        return;
    }

    let Some(command) = command else {
        return;
    };
    if command_is_exempt(command) {
        return;
    }

    let mut state = read_state();
    if agent_setup_is_current(&state) || advisory_is_recent(&state, command) {
        return;
    }

    eprintln!(
        "\n{}\n\n{}\n\n{}",
        "IMPORTANT: Railway agent tooling not detected.".yellow().bold(),
        "Ask the user if they would like this agent to run `railway setup agent -y` to configure the Railway agent tooling (agent skills and MCP configurations)."
            .yellow(),
        "Your agent will work much more effectively in Railway with these installed.".yellow()
            .yellow()
    );

    state.advisories.insert(
        command.to_string(),
        AdvisoryState {
            last_shown_at: Some(Utc::now()),
        },
    );

    let _ = write_state(&state);
}

pub fn record_setup_complete() -> Result<()> {
    let mut state = read_state();
    state.setup = SetupState {
        version: Some(STATE_VERSION),
        last_run_at: Some(Utc::now()),
    };
    write_state(&state)
}
