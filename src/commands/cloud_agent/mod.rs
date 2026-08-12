//! `railway ca` — the cloud agent front door.
//!
//! Bare `railway ca` on a terminal opens the TUI; everything else is a
//! subcommand. `railway code` is the same launcher without the TUI, and both
//! read the same preferences file, so the choice between them is only whether
//! you want to browse first.

pub mod access;
pub mod desktop;
pub mod lifecycle;
pub mod prefs;
pub mod setup;
pub mod skills_sync;
pub mod telemetry;
pub mod tui;

use std::future::Future;

use anyhow::{Context, Result};
use clap::Parser;
use colored::Colorize;

use crate::client::GQLClient;
use crate::commands::code::LaunchArgs;
use crate::config::Configs;
use crate::errors::RailwayError;
use crate::macros::is_stdout_terminal;
use crate::util::progress::create_spinner;
use prefs::AgentPrefs;
use tui::{App, Outcome};

/// Manage Railway cloud agents
#[derive(Parser)]
#[clap(
    args_conflicts_with_subcommands = true,
    after_help = "Examples:\n\n  railway ca                        # browse and launch agents (TUI)\n  railway ca manage                 # jump straight into the manage screen\n  railway ca setup                  # choose your default agent and skills\n  railway ca setup --show           # print current preferences\n  railway ca desktop --claude       # drive an agent from Claude Code Desktop\n  railway ca desktop --codex        # …or from the Codex app\n  railway ca start --claude         # skip the TUI and launch\n\n  railway ca list                   # every agent you own, everywhere\n  railway ca list -e production     # just this environment\n  railway ca create my-agent        # a VM, without connecting to it\n  railway ca ssh my-agent           # connect to it (starts a session if none)\n  railway ca ssh my-agent -- bash   # a plain shell instead of the agent\n  railway ca sleep my-agent         # stop the compute bill, keep the disk\n  railway ca sleep --all            # every running agent you own\n  railway ca delete my-agent        # the agent and its disk\n\nAgents are addressed by name or id. With neither, commands use this\ndirectory's agent, or your only one, and otherwise list the candidates.\n\n`railway code` is the launcher on its own — same flags, same preferences, no\nTUI. Preferences live in ~/.railway/agent-prefs.json; a flag always wins over\nthem, and RAILWAY_CA_AGENT overrides the saved default for one run. A\ndirectory linked with `railway link` wins over the saved default project too\n— new agents land there instead.\n\nNote: requires the CLOUD_AGENTS feature to be enabled."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Command>,

    /// Launch flags. Passing any of these skips the TUI and launches directly,
    /// so `railway ca --claude` behaves like `railway code --claude`.
    #[clap(flatten)]
    launch: LaunchArgs,
}

#[derive(Parser)]
enum Command {
    /// Configure how cloud agents are launched (default agent, skills)
    Setup(setup::Args),

    /// Set up a desktop coding app to work on a cloud agent over SSH
    Desktop(desktop::Args),

    /// Open the TUI directly on the manage screen, skipping the menu
    Manage,

    /// Launch a coding agent on a cloud agent VM, without the TUI
    Start(LaunchArgs),

    /// List your cloud agents
    #[clap(visible_alias = "ls")]
    List(lifecycle::ListArgs),

    /// Create a cloud agent VM, without connecting to it
    #[clap(visible_alias = "new")]
    Create(lifecycle::CreateArgs),

    /// Connect to an existing cloud agent over SSH
    #[clap(visible_alias = "connect")]
    Ssh(lifecycle::SshArgs),

    /// Wake a sleeping agent
    Wake(lifecycle::WakeArgs),

    /// Put an agent to sleep, keeping its disk and stopping the compute bill
    Sleep(lifecycle::SleepArgs),

    /// Delete an agent and everything on its disk
    #[clap(visible_alias = "rm")]
    Delete(lifecycle::DeleteArgs),
}

/// Time one lifecycle verb and report its outcome, passing the result through
/// unchanged.
///
/// At the dispatch rather than inside each verb because the shape is identical
/// for all of them. `ssh` is the exception and tracks itself: it ends in
/// `std::process::exit` to propagate a remote exit status, which would skip
/// anything wrapped around it.
async fn tracked(kind: &'static str, run: impl Future<Output = Result<()>>) -> Result<()> {
    let started = std::time::Instant::now();
    let result = run.await;
    let message = result.as_ref().err().map(|e| format!("{e:#}"));
    telemetry::track_lifecycle(kind, started.elapsed(), message.as_deref()).await;
    result
}

pub async fn command(args: Args) -> Result<()> {
    // `railway ca` is often the first Railway command someone runs, so a logged
    // out user gets the login flow inline instead of an error telling them to
    // run `railway login` and type this again. The command they asked for then
    // continues on the credential that flow just wrote.
    let interactive = is_stdout_terminal();
    if needs_credential(args.command.as_ref(), interactive) {
        ensure_logged_in(interactive).await?;
    }

    match args.command {
        Some(Command::Setup(a)) => setup::command(a).await,
        Some(Command::Desktop(a)) => tracked("desktop", desktop::command(a)).await,
        Some(Command::Manage) => browse_into(Some(tui::Screen::Manage)).await,
        Some(Command::Start(a)) => crate::commands::code::launch(a).await,
        Some(Command::List(a)) => tracked("list", lifecycle::list(a)).await,
        Some(Command::Create(a)) => tracked("create", lifecycle::create(a)).await,
        Some(Command::Ssh(a)) => lifecycle::ssh(a).await,
        Some(Command::Wake(a)) => tracked("wake", lifecycle::wake(a)).await,
        Some(Command::Sleep(a)) => tracked("sleep", lifecycle::sleep(a)).await,
        Some(Command::Delete(a)) => tracked("delete", lifecycle::delete(a)).await,
        None if args.launch.is_bare() && is_stdout_terminal() => browse().await,
        // Flags given, or no terminal to draw on: behave like `railway code`.
        // A TUI in a pipe would be gibberish, and erroring instead would break
        // scripted callers that reasonably expect the launcher.
        None => crate::commands::code::launch(args.launch).await,
    }
}

/// Whether this invocation will talk to the API, and so needs a credential
/// before it starts. Only `setup` can get by without one — everything else
/// (the TUI, `start`, a bare launch flag) opens with a query.
fn needs_credential(command: Option<&Command>, interactive: bool) -> bool {
    match command {
        Some(Command::Setup(a)) => a.needs_credential(interactive),
        _ => true,
    }
}

/// Run the login flow in place when there is no credential to work with.
///
/// Any credential counts, including a project token: those callers are already
/// authenticated as far as this command is concerned, and `railway login`
/// short-circuits on `RAILWAY_TOKEN` anyway, so sending them there would be a
/// detour to nowhere. An expired login is not a case to handle here — `main`
/// refreshes and clears dead credentials before dispatch, so it reaches this
/// check as no credential at all.
async fn ensure_logged_in(interactive: bool) -> Result<()> {
    if Configs::new()?.has_auth_credentials() {
        return Ok(());
    }
    // Piped or scripted: the login flow would sit on a device code nobody is
    // watching. Fail the way every other command does instead.
    if !interactive {
        return Err(RailwayError::Unauthorized.into());
    }

    println!("{}", "Log in to Railway to continue.".bold());
    let result = crate::commands::login::prompt_login().await;
    telemetry::track_login_forwarded(result.as_ref().err().map(|e| format!("{e:#}")).as_deref())
        .await;
    result?;
    println!();
    Ok(())
}

/// The TUI loop. `run` gives the terminal back whenever something needs the
/// whole screen; we do that thing and re-enter with the app state intact, which
/// is what makes connecting feel like stepping into a session and back out
/// rather than restarting the command.
async fn browse() -> Result<()> {
    browse_into(None).await
}

/// Like [`browse`], but opens straight on `initial_screen` instead of the menu
/// when one is given. `railway ca manage` uses this to land on the manage
/// screen without a detour through the menu — an explicit ask, so it also
/// skips the first-run "set up cloud agents?" nudge that a bare `railway ca`
/// would show.
async fn browse_into(initial_screen: Option<tui::Screen>) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let backboard = configs.get_backboard();

    // All under one spinner: the flag check and the key check are small
    // queries against the same client, and running them beside the tree load
    // rather than before it keeps them off the clock.
    let spinner = create_spinner("Loading your projects".to_string());
    let (loaded, ssh_key) = tokio::join!(
        async {
            tokio::try_join!(
                access::ensure_enabled(&client, &configs),
                tui::load_tree(&client, &configs),
            )
        },
        check_ssh_key(&client, &configs),
    );
    spinner.finish_and_clear();
    let (_, tree) = loaded?;
    if tree.is_empty() {
        println!(
            "No projects with environments you can use. Create one with {} first.",
            "railway init".cyan()
        );
        return Ok(());
    }

    let home = dirs::home_dir().context("Unable to get home directory")?;
    let stored = AgentPrefs::load_in(&home);
    let first_run = stored.is_none();
    let saved = stored.unwrap_or_default();
    // A linked directory wins: `railway link` (or a linked service checkout) is
    // an explicit, per-directory statement of "this is the project I'm working
    // in", which outranks a person-wide preference chosen once from wherever
    // the terminal happened to be. The configured default is still the
    // fallback when this directory has no link.
    let linked = linked_target(&mut configs, &client, &tree).await;
    let target = linked.clone().or_else(|| {
        saved.default_project.as_ref().map(|project| tui::Target {
            project_id: project.project_id.clone(),
            project_name: project.project_name.clone(),
            environment_id: project.environment_id.clone(),
            environment_name: project.environment_name.clone(),
        })
    });
    // Same order for the "(default)" label in the tree: a linked project shows
    // as the default over whatever is in the preferences file.
    let default_project_id = linked
        .as_ref()
        .map(|t| t.project_id.clone())
        .or_else(|| saved.default_project.as_ref().map(|p| p.project_id.clone()));
    let mut app = App::new(
        tree,
        target,
        saved.agent.as_deref(),
        saved.theme.as_deref(),
        default_project_id,
        !first_run,
    );

    // A launch that needs a Claude token minted comes back out here, gets one
    // with the real terminal, and goes straight back in — H1's step-out-and-
    // return, now only for the rare case that actually needs it.
    // No preferences yet: offer to set them up rather than dropping someone in
    // front of a prompt whose target, agent and skills are all unanswered.
    // Environments this machine has launched an agent in, so they load without
    // the user going looking. `railway ca` no longer scans every project.
    app.known_environments = configs.code_agent_environments();
    app.skills_source = skills_sync::populated_sources(&home)
        .first()
        .map(|(source, _)| source.slug.to_string());
    // Mirrored so the ⌥s settings card opens showing the saved answer.
    app.skills_enabled = saved.skills.enabled;
    // What the key check learned. Connects gate on this in-frame: an
    // unregistered key raises a register question instead of a hung prompt.
    app.ssh_key = ssh_key;
    // No preferences yet: ask whether to set them up, rather than dropping
    // someone in front of a prompt whose target, agent and skills are all
    // unanswered. Choosing Setup from the menu skips that question — they have
    // already answered it by choosing it. An explicit initial screen is its
    // own answer to "what should I see first" and skips the nudge too.
    if let Some(screen) = initial_screen {
        app.screen = screen;
    } else if first_run {
        app.start_wizard(true);
    }

    let mut pending: Option<tui::LaunchRequest> = None;
    loop {
        match tui::run(&mut app, client.clone(), backboard.clone(), pending.take()).await? {
            Outcome::Quit => {
                // A theme picked with ⌥t is a preference, not a session
                // setting — persist it on the way out rather than making the
                // user set it again next time. Best-effort: failing to save it
                // is not worth an error on exit.
                persist_theme(&home, app.theme.slug);
                return Ok(());
            }
            Outcome::FullScreen(req) => {
                println!(
                    "\n{}",
                    format!("Attaching to {} · {}", req.agent_name, req.session_name).dimmed()
                );
                // The relay resumes a durable session by name, so this is the
                // same session the pane had — the full terminal, none of the
                // TUI's chrome, and no second copy of the work.
                let result = crate::commands::ssh::native::run_native_ssh_with_opts(
                    &req.ssh_target,
                    None,
                    req.identity.as_deref(),
                    Some(crate::commands::ssh::native::DurableResume {
                        session_name: &req.session_name,
                        resume_from_last_read: false,
                    }),
                    &req.relay_opts,
                );
                if let Err(err) = result {
                    eprintln!("{} {err:#}", "Session ended with an error:".red().bold());
                }
                crate::commands::ssh::native::clear_mouse_tracking();
                pause_for_reentry();
            }
            Outcome::NeedsCredential(req) => {
                println!(
                    "\n{}",
                    "Claude needs a one-time token for the agent — this opens your browser."
                        .dimmed()
                );
                match crate::commands::code::ensure_claude_credential_cached(&req.harness) {
                    Ok(()) => pending = Some(req),
                    Err(err) => {
                        eprintln!("{} {err:#}", "Couldn't mint a credential:".red().bold());
                        pause_for_reentry();
                    }
                }
            }
        }
    }
}

/// What the TUI needs to know about the user's SSH key, without prompting.
///
/// The interactive half of `ensure_ssh_key` — pick a key, confirm, register —
/// belongs to the TUI now (its gate card), so this only looks. Check failures
/// come back as `Unknown` rather than an error: the launch pipeline re-checks
/// and is the better place to fail, with a message instead of a blocked
/// startup. With several local keys the offer is the first, the same
/// preferred-key order the non-interactive `ssh keys add` uses.
async fn check_ssh_key(client: &reqwest::Client, configs: &Configs) -> tui::app::SshKeyState {
    use crate::controllers::ssh::keys::{find_local_ssh_keys, get_registered_ssh_keys};
    use tui::app::{SshKeyOffer, SshKeyState};

    let (local, registered) = tokio::join!(
        find_local_ssh_keys(),
        get_registered_ssh_keys(client, configs, None),
    );
    let (Ok(local), Ok(registered)) = (local, registered) else {
        return SshKeyState::Unknown;
    };
    if local.is_empty() {
        return SshKeyState::NoLocalKeys;
    }
    if local
        .iter()
        .any(|l| registered.iter().any(|r| r.fingerprint == l.fingerprint))
    {
        return SshKeyState::Ready;
    }
    let key = &local[0];
    SshKeyState::NeedsRegistration(SshKeyOffer {
        name: key.key_name().to_string(),
        fingerprint: key.fingerprint.clone(),
        public_key: key.public_key.to_string(),
    })
}

fn persist_theme(home: &std::path::Path, slug: &str) {
    let mut prefs = AgentPrefs::load_in(home).unwrap_or_default();
    if prefs.theme.as_deref() == Some(slug) {
        return;
    }
    prefs.theme = Some(slug.to_string());
    let _ = prefs.save_in(home);
}

/// Hold the restored terminal until the user is ready, so whatever the launcher
/// printed — an error, the sleep confirmation, the reconnect hint — isn't wiped
/// by the alternate screen a frame later.
fn pause_for_reentry() {
    use std::io::{BufRead, Write};
    print!("\n{}", "Press enter to return to railway ca…".dimmed());
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}

/// Seed the prompt target from the linked project, when this directory has one
/// and it appears in the tree. Best-effort: an unlinked directory just opens
/// with no target, and the first launch asks for one.
async fn linked_target(
    configs: &mut Configs,
    client: &reqwest::Client,
    tree: &[tui::app::WorkspaceNode],
) -> Option<tui::Target> {
    let linked = configs.get_linked_project().await.ok()?;
    let env_id = linked.environment.clone()?;
    let _ = client;
    for ws in tree {
        for project in &ws.projects {
            if project.id != linked.project {
                continue;
            }
            let env = project.envs.iter().find(|e| e.id == env_id)?;
            return Some(tui::Target {
                project_id: project.id.clone(),
                project_name: project.name.clone(),
                environment_id: env.id.clone(),
                environment_name: env.name.clone(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn setup_args(argv: &[&str]) -> Command {
        Command::Setup(setup::Args::parse_from(
            std::iter::once("setup").chain(argv.iter().copied()),
        ))
    }

    #[test]
    fn launch_paths_need_a_credential() {
        assert!(needs_credential(None, true));
        assert!(needs_credential(None, false));
        assert!(needs_credential(
            Some(&Command::Start(LaunchArgs::parse_from(["start"]))),
            true
        ));
    }

    #[test]
    fn local_setup_runs_logged_out() {
        assert!(!needs_credential(Some(&setup_args(&["--show"])), true));
        assert!(!needs_credential(Some(&setup_args(&["-y"])), true));
        // Piped setup takes the same non-interactive path as `-y`.
        assert!(!needs_credential(Some(&setup_args(&[])), false));
    }

    #[test]
    fn interactive_setup_needs_a_credential_for_the_project_picker() {
        assert!(needs_credential(Some(&setup_args(&[])), true));
    }
}
