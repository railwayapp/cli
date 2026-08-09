//! Additive telemetry for `railway ca` — detail events layered on top of the
//! generic per-dispatch event the `commands!` macro already fires for
//! `command="cloud_agent"` (and, for the shared launch pipeline, for
//! `command="code"` too).
//!
//! Same role as [`crate::commands::ssh::tel`]: never replaces `?`
//! propagation, only reports alongside it. Reused here rather than
//! generalized there because the shapes differ enough (a kind dimension
//! flattened into `sub_command`, not just a failing stage) to want their own
//! small mapping functions per call site.
//!
//! Nothing here logs free text: no prompts, no session/agent/project names,
//! no repo-shaped values. Only IDs (already attached by
//! [`crate::telemetry::send`]'s ambient context), fixed slugs (harness,
//! theme, skills source), and truncated error messages.

use std::time::Duration;

use super::prefs::AgentPrefs;
use super::tui::app::AgentOp;
use crate::config::Configs;
use crate::telemetry::{self, CliTrackEvent};

const TRUNCATE_AT: usize = 256;

fn truncate(message: &str) -> String {
    if message.len() > TRUNCATE_AT {
        message[..TRUNCATE_AT].to_string()
    } else {
        message.to_string()
    }
}

fn event(
    command: &str,
    sub_command: String,
    duration_ms: u64,
    success: bool,
    error_message: Option<String>,
) -> CliTrackEvent {
    CliTrackEvent {
        command: command.to_string(),
        sub_command: Some(sub_command),
        duration_ms,
        success,
        error_message,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        cli_version: env!("CARGO_PKG_VERSION"),
        is_ci: Configs::env_is_ci(),
    }
}

/// Outcome of the shared `code::prepare()` launch pipeline — fired once per
/// attempt regardless of which command reached it (`railway code`,
/// `railway ca start`, or the TUI's `start_launch`), since none of those
/// three would otherwise see the others' outcomes.
///
/// Its own pseudo-command (`cloud_agent_launch`) rather than `"cloud_agent"`
/// because it isn't 1:1 with a single top-level dispatch — same reasoning as
/// the `"mcp"` pseudo-command for MCP tool calls.
pub async fn track_launch_outcome(
    harness: &'static str,
    created: Option<bool>,
    duration: Duration,
    error: Option<&str>,
) {
    let sub_command = match (error, created) {
        (Some(_), _) => format!("{harness}_failed"),
        (None, Some(true)) => format!("{harness}_created"),
        (None, _) => format!("{harness}_reused"),
    };
    telemetry::send(event(
        "cloud_agent_launch",
        sub_command,
        duration.as_millis() as u64,
        error.is_none(),
        error.map(truncate),
    ))
    .await;
}

/// One lifecycle mutation on an agent (sleep/wake/delete) from the manage
/// screen, fired once per attempt so both volume and failure rate are
/// visible per op kind.
pub async fn track_agent_op(op: AgentOp, error: Option<&str>) {
    let kind = match op {
        AgentOp::Sleep => "sleep",
        AgentOp::Wake => "wake",
        AgentOp::Delete => "delete",
    };
    telemetry::send(event(
        "cloud_agent",
        format!("agent_{kind}"),
        0,
        error.is_none(),
        error.map(truncate),
    ))
    .await;
}

/// A named session action outside the main launch pipeline — reattaching to
/// an existing durable session, killing one by name, opening the ssh pane on
/// a freshly prepared agent, or the auto-sleep-on-quit that keeps a
/// disconnected agent from billing unattended. `kind` is a fixed label, not
/// user input.
pub async fn track_session_event(kind: &str, error: Option<&str>) {
    telemetry::send(event(
        "cloud_agent",
        kind.to_string(),
        0,
        error.is_none(),
        error.map(truncate),
    ))
    .await;
}

/// `railway ca setup` or the TUI wizard saved preferences. `entry` is
/// `"cli"` or `"wizard"` — the two call sites share this mapping so both
/// land in the same shape.
///
/// One flat event per dimension (agent / theme / project / skills), mirror
/// of `commands/service.rs`'s `track_service_source`, rather than one event
/// with all four combined: keeps `sub_command` cardinality small and each
/// fact independently queryable.
pub async fn track_setup_saved(entry: &str, prefs: &AgentPrefs) {
    let harness = prefs.agent.as_deref().unwrap_or("none");
    telemetry::send(event(
        "cloud_agent",
        format!("setup_{entry}_agent_{harness}"),
        0,
        true,
        None,
    ))
    .await;

    let theme = prefs.theme.as_deref().unwrap_or("default");
    telemetry::send(event(
        "cloud_agent",
        format!("setup_{entry}_theme_{theme}"),
        0,
        true,
        None,
    ))
    .await;

    let project_state = if prefs.default_project.is_some() {
        "set"
    } else {
        "unset"
    };
    telemetry::send(event(
        "cloud_agent",
        format!("setup_{entry}_project_{project_state}"),
        0,
        true,
        None,
    ))
    .await;

    let skills_state = if !prefs.skills.enabled {
        "off"
    } else {
        prefs.skills.source.as_deref().unwrap_or("unknown")
    };
    telemetry::send(event(
        "cloud_agent",
        format!("setup_{entry}_skills_{skills_state}"),
        0,
        true,
        None,
    ))
    .await;
}

/// `entry`'s setup flow failed to save preferences (disk error, etc.) — the
/// prompts themselves succeeded, so this is a write failure, not a decline.
pub async fn track_setup_failed(entry: &str, error: &str) {
    telemetry::send(event(
        "cloud_agent",
        format!("setup_{entry}_failed"),
        0,
        false,
        Some(truncate(error)),
    ))
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_caps_long_messages() {
        let long = "x".repeat(300);
        assert_eq!(truncate(&long).len(), TRUNCATE_AT);
    }

    #[test]
    fn truncate_leaves_short_messages_alone() {
        assert_eq!(truncate("boom"), "boom");
    }
}
