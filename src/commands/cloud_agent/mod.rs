//! `railway ca` — the cloud agent front door.
//!
//! Bare `railway ca` on a terminal opens the TUI; everything else is a
//! subcommand. `railway code` is the same launcher without the TUI, and both
//! read the same preferences file, so the choice between them is only whether
//! you want to browse first.

pub mod prefs;
pub mod setup;
pub mod skills_sync;
pub mod tui;

use anyhow::{Context, Result};
use clap::Parser;
use colored::Colorize;

use crate::client::GQLClient;
use crate::commands::code::LaunchArgs;
use crate::config::Configs;
use crate::macros::is_stdout_terminal;
use crate::util::progress::create_spinner;
use prefs::AgentPrefs;
use tui::{App, Outcome};

/// Manage Railway cloud agents
#[derive(Parser)]
#[clap(
    args_conflicts_with_subcommands = true,
    after_help = "Examples:\n\n  railway ca                        # browse and launch agents (TUI)\n  railway ca setup                  # choose your default agent and skills\n  railway ca setup --show           # print current preferences\n  railway ca start --claude         # skip the TUI and launch\n\n`railway code` is the launcher on its own — same flags, same preferences, no\nTUI. Preferences live in ~/.railway/agent-prefs.json; a flag always wins over\nthem, and RAILWAY_CA_AGENT overrides the saved default for one run.\n\nNote: requires the CLOUD_AGENTS feature to be enabled."
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

    /// Launch a coding agent on a cloud agent VM, without the TUI
    Start(LaunchArgs),
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Some(Command::Setup(a)) => setup::command(a).await,
        Some(Command::Start(a)) => crate::commands::code::launch(a).await,
        None if args.launch.is_bare() && is_stdout_terminal() => browse().await,
        // Flags given, or no terminal to draw on: behave like `railway code`.
        // A TUI in a pipe would be gibberish, and erroring instead would break
        // scripted callers that reasonably expect the launcher.
        None => crate::commands::code::launch(args.launch).await,
    }
}

/// The TUI loop. `run` gives the terminal back whenever something needs the
/// whole screen; we do that thing and re-enter with the app state intact, which
/// is what makes connecting feel like stepping into a session and back out
/// rather than restarting the command.
async fn browse() -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let backboard = configs.get_backboard();

    let spinner = create_spinner("Loading your projects".to_string());
    let tree = tui::load_tree(&client, &configs).await;
    spinner.finish_and_clear();
    let tree = tree?;
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
    // The configured default wins: it is the answer to "where do agents go",
    // and a linked directory is about deploys, not agents. A linked project is
    // still the fallback when no default has been set.
    let target = saved
        .default_project
        .as_ref()
        .map(|project| tui::Target {
            project_id: project.project_id.clone(),
            project_name: project.project_name.clone(),
            environment_id: project.environment_id.clone(),
            environment_name: project.environment_name.clone(),
        })
        .or(linked_target(&mut configs, &client, &tree).await);
    let mut app = App::new(
        tree,
        target,
        saved.agent.as_deref(),
        saved.theme.as_deref(),
        saved
            .default_project
            .as_ref()
            .map(|project| project.project_id.clone()),
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
    // No preferences yet: ask whether to set them up, rather than dropping
    // someone in front of a prompt whose target, agent and skills are all
    // unanswered. Choosing Setup from the menu skips that question — they have
    // already answered it by choosing it.
    if first_run {
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
