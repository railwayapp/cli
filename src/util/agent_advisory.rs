use std::{cmp::Ordering, fs::File, io::Read, path::PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};

use super::compare_semver::compare_semver;
use crate::{telemetry, util};

const STATE_VERSION: u32 = 1;
const DISABLE_ENV: &str = "RAILWAY_AGENT_ADVISORY";
const FORCE_ENV: &str = "RAILWAY_AGENT";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentState {
    #[serde(default)]
    setup: SetupState,
    #[serde(default)]
    advisory: AdvisoryState,
    #[serde(default)]
    upgrade_nudge: UpgradeNudgeState,
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
    last_shown_cli_version: Option<String>,
    last_shown_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpgradeNudgeState {
    last_nudged_version: Option<String>,
    last_nudged_at: Option<DateTime<Utc>>,
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
    matches!(
        std::env::var(DISABLE_ENV).as_deref(),
        Ok("0" | "false" | "off")
    )
}

fn is_agent_environment() -> bool {
    if matches!(
        std::env::var(FORCE_ENV).as_deref(),
        Ok("1" | "true" | "yes")
    ) {
        return true;
    }

    const AGENT_ENV_VARS: &[&str] = &[
        "AIDER",
        "AMP_CURRENT_THREAD_ID",
        "COPILOT_AGENT_SESSION_ID",
        "COPILOT_CLI",
        "CLAUDECODE",
        "CLAUDE_CODE",
        "CURSOR_AGENT",
        "FACTORY_DROID",
        "GEMINI_CLI",
        "OPENCODE",
        "OPENAI_AGENT",
        "PI_CODING_AGENT",
        "REPLIT_AGENT",
    ];

    const AGENT_ENV_PREFIXES: &[&str] = &[
        "AMP_",
        "CLAUDE_CODE_",
        "CODEX_",
        "COPILOT_",
        "GEMINI_",
        "OPENCODE_",
    ];

    if std::env::var("AGENT")
        .map(|value| value.eq_ignore_ascii_case("amp"))
        .unwrap_or(false)
    {
        return true;
    }

    AGENT_ENV_VARS
        .iter()
        .any(|name| std::env::var_os(name).is_some())
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

// Suppress the advisory if we've already shown it for the currently running
// CLI version. Upgrading the CLI re-arms the advisory exactly once.
fn advisory_already_shown_for_current_cli(state: &AgentState) -> bool {
    state
        .advisory
        .last_shown_cli_version
        .as_deref()
        .is_some_and(|shown| shown == env!("CARGO_PKG_VERSION"))
}

pub async fn maybe_show(raw_args: &[String], command: Option<&str>) {
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
    if agent_setup_is_current(&state) || advisory_already_shown_for_current_cli(&state) {
        return;
    }

    eprintln!(
        "\n{}\n{}",
        "Railway agent tooling (skills + MCP) isn't installed.".yellow(),
        "Run `railway setup agent` to configure it.".dimmed(),
    );

    state.advisory = AdvisoryState {
        last_shown_cli_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        last_shown_at: Some(Utc::now()),
    };

    let _ = write_state(&state);

    telemetry::send(telemetry::CliTrackEvent {
        command: "agent_advisory".to_string(),
        sub_command: Some(command.to_string()),
        success: true,
        error_message: None,
        duration_ms: 0,
        cli_version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        is_ci: crate::config::Configs::env_is_ci(),
    })
    .await;
}

/// Agent-facing counterpart of the TTY-only "new version available" banner.
///
/// Agent-driven machines are structurally unable to upgrade through the
/// normal flow: staged-binary apply is TTY-gated (main.rs) so a host whose
/// railway usage is 100% agent-driven downloads updates forever without
/// applying them, and the banner that would tell a human to act never
/// prints. Warehouse audit 2026-06-09: the largest plain agent_unknown
/// population was pre-5.5.0 versions that newer detection had already
/// fixed. This nudge tells the agent itself — once per pending version, on
/// stderr — which agents reliably surface to the user or act on directly.
pub async fn maybe_show_upgrade_nudge(
    raw_args: &[String],
    command: Option<&str>,
    latest_version: Option<&str>,
    skipped_version: Option<&str>,
) {
    let Some(latest) = latest_version else {
        return;
    };
    if disabled_by_env() || should_skip_for_args(raw_args) {
        return;
    }
    let Some(command) = command else {
        return;
    };
    if command_is_exempt(command) {
        return;
    }
    // Respect a rollback: don't nudge agents toward the version the user
    // explicitly backed out of (mirrors the TTY banner's skip logic).
    if skipped_version == Some(latest) {
        return;
    }
    if !matches!(
        compare_semver(env!("CARGO_PKG_VERSION"), latest),
        Ordering::Less
    ) {
        return;
    }
    // Last (most expensive) gate: process-tree-aware agent detection.
    if !telemetry::is_agent() {
        return;
    }

    let mut state = read_state();
    if state.upgrade_nudge.last_nudged_version.as_deref() == Some(latest) {
        return;
    }

    eprintln!(
        "\n{}\n{}",
        format!(
            "A newer Railway CLI is available: v{} (current: v{}).",
            latest,
            env!("CARGO_PKG_VERSION"),
        )
        .yellow(),
        "Run `railway upgrade --yes` to update.".dimmed(),
    );

    state.upgrade_nudge = UpgradeNudgeState {
        last_nudged_version: Some(latest.to_string()),
        last_nudged_at: Some(Utc::now()),
    };
    let _ = write_state(&state);

    telemetry::send(telemetry::CliTrackEvent {
        command: "upgrade_nudge".to_string(),
        sub_command: Some(command.to_string()),
        success: true,
        error_message: None,
        duration_ms: 0,
        cli_version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        is_ci: crate::config::Configs::env_is_ci(),
    })
    .await;
}

pub fn record_setup_complete() -> Result<()> {
    let mut state = read_state();
    state.setup = SetupState {
        version: Some(STATE_VERSION),
        last_run_at: Some(Utc::now()),
    };
    write_state(&state)
}
