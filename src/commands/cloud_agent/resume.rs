//! `railway code --resume <agent>:<session>` — back into one exact
//! conversation.
//!
//! Two halves, decided by whether the durable session is still alive. While
//! it is, this is `railway ca ssh --session` with the agent pinned: attach by
//! name and the very terminal comes back, scrollback and all. Once it has
//! ended — the agent slept, the VM restarted, the harness exited — there is
//! nothing to attach to, but the conversation is still recoverable: the
//! harness reported its own session id while it ran (see
//! [`ca::SessionThread`]), the transcript that id names lives on the agent's
//! disk, and the disk survives sleep. The revive starts a fresh durable
//! session running the harness's native resume against that id.
//!
//! The reference is exactly what a disconnect prints, and what makes a
//! session recoverable from outside this process — a terminal manager that
//! stored the reference can put its pane back into the same conversation
//! after a reboot, the way it would a local agent.

use std::time::Duration;

use anyhow::{Result, bail};
use colored::Colorize;

use crate::client::GQLClient;
use crate::commands::cloud_agent::lifecycle::{describe_sessions, listed_session_name, scope};
use crate::commands::cloud_agent::telemetry;
use crate::commands::cloud_agent::tui::session;
use crate::commands::code::{self, LaunchArgs};
use crate::commands::ssh::native;
use crate::config::Configs;
use crate::controllers::cloud_agent as ca;
use crate::util::progress::create_spinner;

/// A parsed `<agent>:<session>` reference. The agent half is a name or id
/// (whatever `railway ca` accepts); the session half is the durable session's
/// name. Split on the first colon: neither agent ids nor generated names
/// contain one, and splitting late would let an odd session name eat the
/// agent.
#[derive(Debug)]
pub struct SessionRef {
    pub agent: String,
    pub session: String,
}

impl SessionRef {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.split_once(':') {
            Some((agent, session)) if !agent.is_empty() && !session.is_empty() => Ok(Self {
                agent: agent.to_string(),
                session: session.to_string(),
            }),
            _ => bail!(
                "--resume takes <agent>:<session> — the reference a disconnect prints, e.g. `my-agent:claude-x3k9f2`."
            ),
        }
    }
}

pub async fn command(reference: &str, launch: &LaunchArgs) -> Result<()> {
    let started = std::time::Instant::now();
    let result = connect(reference, launch).await;
    let message = result.as_ref().err().map(|e| format!("{e:#}"));
    telemetry::track_lifecycle("resume", started.elapsed(), message.as_deref()).await;

    // A non-zero remote exit is the session's result, not a failure of ours —
    // same contract as `railway ca ssh`.
    match result? {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

async fn connect(reference: &str, launch: &LaunchArgs) -> Result<i32> {
    let target = SessionRef::parse(reference)?;
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (configs, client) = (&mut configs, &client);
    let scoped = scope(
        configs,
        client,
        launch.project.clone(),
        launch.environment.clone(),
    )
    .await?;
    let backboard = configs.get_backboard();
    let (agent, _) = ca::resolve(configs, client, Some(&target.agent), scoped.as_deref()).await?;

    let was_running = matches!(agent.status, ca::Status::Running);
    // The platform's records answer before the VM has to: which sessions the
    // relay still holds, and what ran inside them. Both survive the agent
    // sleeping, so the plan is settled up front — a reference that cannot be
    // resumed must not cost a wake.
    let (sessions, threads) =
        ca::session_threads(client, &backboard, &agent.id, &agent.environment_id).await?;

    // Same zombie rule as `railway ca ssh`: sleeping stopped every process on
    // the VM, so a session the platform still lists as running on an agent
    // that was not is a record that outlived its process. Attaching to it
    // streams nothing and the screen stays blank — revive instead.
    let attachable = was_running
        && sessions
            .iter()
            .any(|s| s.name == target.session && s.running);

    // `Some` carries the revive: the command to run and the durable session
    // name to run it under. Settled before the wake for the reason above.
    let revival = match attachable {
        true => None,
        false => {
            let Some(thread) = newest_thread(&threads, &target.session) else {
                if sessions.iter().any(|s| s.name == target.session) {
                    bail!(
                        "Session {} on {} isn't running, and no harness ever reported a conversation from inside it (a plain shell, or a run that predates thread reports) — there is nothing to resume.\n`railway ca ssh {}` starts a fresh session.",
                        target.session,
                        agent.name,
                        agent.name,
                    );
                }
                bail!(
                    "Agent {} has no record of a session named {:?}.{}",
                    agent.name,
                    target.session,
                    describe_sessions(&sessions)
                );
            };
            let Some(remote_cmd) = code::resume_remote_command(
                &thread.harness,
                &thread.session_id,
                code::SessionStyle::FullTerminal,
            ) else {
                bail!(
                    "Session {} ran {}, which has no resume this CLI knows to be safe.\n`railway ca ssh {}` starts a fresh session.",
                    target.session,
                    thread.harness,
                    agent.name,
                );
            };
            let session_name = session::durable_name(durable_prefix(&thread.harness));
            Some((thread.harness.clone(), remote_cmd, session_name))
        }
    };

    // The wake mirror of `railway ca ssh`: probe the route rather than poll
    // status to RUNNING, wake first when asleep, and refuse terminal states.
    let spinner = (!was_running).then(|| create_spinner(format!("Waking agent {}", agent.name)));
    let ready = match agent.status {
        ca::Status::Running => Ok(()),
        ca::Status::Starting => match code::relay_access().await {
            Ok(access) => code::wait_until_connectable(
                client,
                &backboard,
                &agent.environment_id,
                &agent.id,
                &access,
                // Caught mid-boot: it may be routable right now.
                Duration::ZERO,
            )
            .await
            .map(|_| ()),
            Err(e) => Err(e),
        },
        ca::Status::Sleeping => match ca::wake(client, &backboard, &agent.id).await {
            Ok(()) => match code::relay_access().await {
                Ok(access) => code::wait_until_connectable(
                    client,
                    &backboard,
                    &agent.environment_id,
                    &agent.id,
                    &access,
                    // The wake's physical floor; see the wait's doc.
                    Duration::from_millis(350),
                )
                .await
                .map(|_| ()),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        },
        _ => Err(anyhow::anyhow!(
            "Agent {} is {} — it cannot be connected to, and its sessions went with it.",
            agent.name,
            agent.status.label(),
        )),
    };
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }
    ready?;
    ca::remember(configs, &agent)?;

    let (session_name, remote) = match &revival {
        None => {
            telemetry::track_lifecycle_detached("resume_attach");
            (target.session.clone(), None)
        }
        Some((harness, remote_cmd, session_name)) => {
            telemetry::track_lifecycle_detached("resume_revive");
            println!(
                "{}",
                format!(
                    "Session {} has ended — resuming its {harness} conversation in a new session.",
                    target.session
                )
                .dimmed()
            );
            (session_name.clone(), Some(vec![remote_cmd.clone()]))
        }
    };
    println!(
        "{}",
        format!("Attaching to {} · {}", agent.name, session_name).dimmed()
    );

    let connected = {
        let info = code::connect_info(&agent.environment_id, &agent.id).await?;
        let session_name = session_name.clone();
        tokio::task::spawn_blocking(move || {
            native::run_native_ssh_with_opts(
                &info.ssh_target,
                remote.as_deref(),
                info.identity.as_deref(),
                Some(native::DurableResume {
                    session_name: &session_name,
                    resume_from_last_read: false,
                }),
                &info.relay_opts,
            )
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    };
    native::clear_mouse_tracking();
    crate::commands::ssh::tel::drain_detached(Duration::from_secs(2)).await;

    // A connection that never happened still woke a machine with no idle
    // timeout, so put back what this run changed — but only that (see
    // `railway ca ssh`, whose rule this is).
    let exit_code = match connected {
        Ok(code) => code,
        Err(e) => {
            if !was_running {
                let _ = ca::sleep(client, &backboard, &agent.environment_id, &agent.id).await;
            }
            return Err(e);
        }
    };

    println!(
        "\nDisconnected — agent {} is still running. `railway ca sleep {}` stops the compute bill.",
        agent.name.cyan(),
        agent.name
    );
    // A revive moved the conversation to a new durable session, so the old
    // reference is spent. Verified against the listing before it is
    // advertised: a freshly minted name may not be the one the platform
    // recorded (see `listed_session_name`).
    if let Some(name) = listed_session_name(client, &backboard, &agent.id, &session_name).await {
        println!("Back into this exact conversation:");
        println!("  railway code --resume {}:{name}", agent.name);
    }

    Ok(exit_code)
}

/// The newest report attributed to this durable session. Newest because one
/// console session can host several runs over its life (`/clear`, a second
/// `claude`), and the conversation someone wants back is the one they were
/// last in. ISO-8601 timestamps, so lexicographic max is chronological max —
/// the same comparison the manage TUI's thread join makes.
fn newest_thread<'a>(
    threads: &'a [ca::SessionThread],
    session: &str,
) -> Option<&'a ca::SessionThread> {
    threads
        .iter()
        .filter(|t| t.session_name.as_deref() == Some(session))
        .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
}

/// The durable-name prefix for a revived session, from the reported harness
/// label. Identical except for Railway's own agent, which reports as
/// "railway-agent" but names its sessions "railway" — see
/// [`crate::commands::code`]'s `Agent::slug`.
fn durable_prefix(harness: &str) -> &str {
    match harness {
        "railway-agent" => "railway",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(session_name: Option<&str>, harness: &str, id: &str, at: &str) -> ca::SessionThread {
        ca::SessionThread {
            session_name: session_name.map(str::to_string),
            harness: harness.to_string(),
            session_id: id.to_string(),
            updated_at: at.to_string(),
        }
    }

    #[test]
    fn reference_splits_on_the_first_colon() {
        let parsed = SessionRef::parse("my-agent:claude-x3k9f2").unwrap();
        assert_eq!(parsed.agent, "my-agent");
        assert_eq!(parsed.session, "claude-x3k9f2");

        // An id works as the agent half, and any later colon stays with the
        // session — the agent half is the one whose grammar we control.
        let parsed = SessionRef::parse("6cbd52a5-8f3b-4a0e-9a5e-0e8f1c2d3e4f:odd:name").unwrap();
        assert_eq!(parsed.agent, "6cbd52a5-8f3b-4a0e-9a5e-0e8f1c2d3e4f");
        assert_eq!(parsed.session, "odd:name");
    }

    #[test]
    fn a_reference_missing_either_half_is_refused_with_the_shape() {
        for raw in ["claude-x3k9f2", ":claude-x3k9f2", "my-agent:", ":"] {
            let err = SessionRef::parse(raw).unwrap_err().to_string();
            assert!(err.contains("<agent>:<session>"), "{raw}: {err}");
        }
    }

    #[test]
    fn the_newest_report_for_the_session_wins() {
        // One console session hosted two runs; the one someone wants back is
        // the one they were last in.
        let threads = vec![
            thread(
                Some("claude-abc123"),
                "claude",
                "old",
                "2026-08-27T10:00:00Z",
            ),
            thread(
                Some("claude-abc123"),
                "claude",
                "new",
                "2026-08-28T09:00:00Z",
            ),
            thread(
                Some("claude-zzz999"),
                "claude",
                "other",
                "2026-08-28T12:00:00Z",
            ),
            thread(None, "claude", "unattributed", "2026-08-28T13:00:00Z"),
        ];
        let found = newest_thread(&threads, "claude-abc123").unwrap();
        assert_eq!(found.session_id, "new");
        assert!(newest_thread(&threads, "codex-nope").is_none());
    }

    #[test]
    fn railways_reports_map_back_to_its_session_prefix() {
        assert_eq!(durable_prefix("railway-agent"), "railway");
        assert_eq!(durable_prefix("claude"), "claude");
        assert_eq!(durable_prefix("codex"), "codex");
    }
}
