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
//! None of the structured fields carry free text: no prompts, no
//! session/agent/project names, no repo-shaped values — only IDs (already
//! attached by [`crate::telemetry::send`]'s ambient context) and fixed slugs
//! (harness, theme, skills source). `error_message` is the one exception:
//! like every other failure event in this CLI (the generic dispatch event,
//! `commands/ssh/tel.rs`), it carries the underlying error's `Display`
//! text truncated to 256 bytes, which can include a user-supplied
//! identifier when the error type formats one in (an unknown environment or
//! service name, for instance) — this module doesn't scrub that, it just
//! doesn't add any of its own.

use std::time::Duration;

use super::prefs::AgentPrefs;
use super::tui::app::AgentOp;
use crate::config::Configs;
use crate::telemetry::{self, CliTrackEvent};

fn truncate(message: &str) -> String {
    telemetry::truncate_message(message)
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

/// One `railway ca` lifecycle verb run from the command line — list, create,
/// ssh, wake, sleep, delete — fired once per attempt so volume and failure rate
/// are visible per verb.
///
/// Its own `cli_` prefix rather than sharing [`track_agent_op`]'s slugs: those
/// are the same mutations reached from the TUI's manage screen, and rolling the
/// two together would hide which surface people actually reach for. The generic
/// per-dispatch event only says `command="cloud_agent"`, which cannot answer
/// that either.
///
/// `kind` is a fixed slug — never a name, id, or anything the caller typed.
/// Detail slugs (`ssh_attach`, `sleep_all`) pass [`Duration::ZERO`]: they record
/// which path was taken, and the verb's own event already carries the timing.
pub async fn track_lifecycle(kind: &str, duration: Duration, error: Option<&str>) {
    telemetry::send(event(
        "cloud_agent",
        lifecycle_sub_command(kind, error.is_some()),
        duration.as_millis() as u64,
        error.is_none(),
        error.map(truncate),
    ))
    .await;
}

/// The slug a lifecycle verb reports under. Split out so the shape dashboards
/// group on is pinned by a test rather than by reading the call sites.
fn lifecycle_sub_command(kind: &str, failed: bool) -> String {
    match failed {
        true => format!("cli_{kind}_failed"),
        false => format!("cli_{kind}"),
    }
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

/// `railway ca` was run with no credential and sent the user through the login
/// flow before doing what they asked. Fired once per forwarded run, so the
/// share of `ca` invocations that are somebody's first Railway command — and
/// how often that hand-off fails — is visible without inferring it from the
/// gap between a `login` event and a `cloud_agent` one.
pub async fn track_login_forwarded(error: Option<&str>) {
    telemetry::send(event(
        "cloud_agent",
        "login_forwarded".to_string(),
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
    fn lifecycle_slugs_are_prefixed_and_marked() {
        assert_eq!(lifecycle_sub_command("create", false), "cli_create");
        assert_eq!(lifecycle_sub_command("create", true), "cli_create_failed");
    }

    #[test]
    fn lifecycle_slugs_do_not_collide_with_the_tui_ops() {
        // Same three mutations, two surfaces. `agent_sleep` is the manage
        // screen's; `cli_sleep` is the command line's. Merging them would hide
        // which one people actually use.
        for kind in ["sleep", "wake", "delete"] {
            assert_ne!(lifecycle_sub_command(kind, false), format!("agent_{kind}"));
        }
    }

    #[test]
    fn truncate_caps_long_messages() {
        let long = "x".repeat(300);
        assert_eq!(truncate(&long).len(), 256);
    }

    #[test]
    fn truncate_leaves_short_messages_alone() {
        assert_eq!(truncate("boom"), "boom");
    }
}
