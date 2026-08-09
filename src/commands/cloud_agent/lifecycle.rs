//! The flat `railway ca` verbs: list, create, ssh, wake, sleep, delete.
//!
//! Each addresses an agent that already exists, by name or id. Only `create`
//! makes one — `ssh` connects to what is there and errors otherwise, and
//! `railway ca start` remains the create-and-launch path. That line is the
//! point of the split: a mistyped agent name should be an error, not a second
//! billed VM.

use std::collections::HashMap;

use anyhow::{Result, bail};
use clap::Parser;
use colored::Colorize;
use is_terminal::IsTerminal;

use crate::client::GQLClient;
use crate::commands::cloud_agent::tui::session;
use crate::commands::code::{self, LaunchArgs, Progress};
use crate::commands::sandbox::{resolve_project_and_env, variables_to_input};
use crate::commands::ssh::native;
use crate::config::Configs;
use crate::controllers::cloud_agent as ca;
use crate::util::progress::create_spinner;
use crate::util::prompt::prompt_confirm_with_default;

/// Where to look. Flattened into each subcommand rather than made global on
/// `railway ca`, whose own flattened launch flags already own `-p`/`-e`.
#[derive(Parser)]
pub struct TargetArgs {
    /// Environment name or ID (defaults to the linked environment)
    #[clap(long, short)]
    environment: Option<String>,

    /// Project ID (defaults to the linked project)
    #[clap(long, short)]
    project: Option<String>,
}

#[derive(Parser)]
pub struct ListArgs {
    /// Include agents belonging to other members of the environment. Requires
    /// --environment, since "everyone's agents" is only a question about a
    /// place
    #[clap(long)]
    all: bool,

    /// Output as JSON
    #[clap(long)]
    json: bool,

    #[clap(flatten)]
    target: TargetArgs,
}

#[derive(Parser)]
pub struct CreateArgs {
    /// Name for the agent (defaults to a generated one)
    #[clap(value_name = "NAME")]
    name: Option<String>,

    /// Set a variable on the agent (repeatable, comma-separable). Values may
    /// reference other variables — `DB_URL=postgres.DATABASE_URL` or the full
    /// `${{postgres.DATABASE_URL}}` form — resolved server-side at create time
    #[clap(long = "variable", value_name = "KEY=VALUE[,KEY=VALUE...]")]
    variables: Vec<String>,

    /// Load variables from a .env file (repeatable). `--variable` flags
    /// override file entries with the same key
    #[clap(long = "env-file", value_name = "PATH")]
    env_files: Vec<std::path::PathBuf>,

    /// Return as soon as the agent is requested, without waiting for it to
    /// finish booting
    #[clap(long)]
    no_wait: bool,

    /// Output as JSON
    #[clap(long)]
    json: bool,

    #[clap(flatten)]
    target: TargetArgs,
}

#[derive(Parser)]
pub struct WakeArgs {
    /// Agent name or ID (defaults to this directory's, or your only one)
    #[clap(value_name = "AGENT")]
    agent: Option<String>,

    /// Return as soon as the wake is requested, without waiting for the agent
    /// to come up
    #[clap(long)]
    no_wait: bool,

    #[clap(flatten)]
    target: TargetArgs,
}

#[derive(Parser)]
pub struct SleepArgs {
    /// Agent name or ID (defaults to this directory's, or your only one)
    #[clap(value_name = "AGENT")]
    agent: Option<String>,

    /// Sleep every running agent you own (narrowed by --environment when given)
    #[clap(long, conflicts_with = "agent")]
    all: bool,

    #[clap(flatten)]
    target: TargetArgs,
}

#[derive(Parser)]
pub struct SshArgs {
    /// Agent name or ID (defaults to this directory's, or your only one)
    #[clap(value_name = "AGENT")]
    agent: Option<String>,

    /// Attach to this durable session by name, rather than the agent's only one
    #[clap(long, value_name = "NAME")]
    session: Option<String>,

    /// Leave the agent running on disconnect instead of putting it to sleep.
    /// A running agent keeps billing for compute
    #[clap(long)]
    keep_awake: bool,

    #[clap(flatten)]
    target: TargetArgs,

    /// Run this command instead of attaching to a session (`-- bash` for a
    /// plain shell)
    #[clap(trailing_var_arg = true)]
    command: Vec<String>,
}

#[derive(Parser)]
pub struct DeleteArgs {
    /// Agent name or ID (defaults to this directory's, or your only one)
    #[clap(value_name = "AGENT")]
    agent: Option<String>,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    #[clap(flatten)]
    target: TargetArgs,
}

/// Resolve the environment the caller narrowed to, if they narrowed at all.
///
/// Only runs when a flag was passed: the lifecycle verbs address agents by name
/// across the whole account, so resolving an environment unprompted would add a
/// request — and, in an unlinked directory, a picker — to commands that do not
/// need one.
async fn scope(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
) -> Result<Option<String>> {
    if project.is_none() && environment.is_none() {
        return Ok(None);
    }
    let (_, environment_id) =
        resolve_project_and_env(configs, client, project, environment).await?;
    Ok(Some(environment_id))
}

pub async fn list(args: ListArgs) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (configs, client) = (&mut configs, &client);
    let (project, environment) = (args.target.project.clone(), args.target.environment.clone());
    let scoped = scope(configs, client, project, environment).await?;
    if args.all && scoped.is_none() {
        bail!(
            "--all lists everyone's agents in one environment. Add --environment (and --project if this directory isn't linked)."
        );
    }

    let backboard = configs.get_backboard();
    let mut agents = match &scoped {
        Some(environment_id) => {
            ca::list_in_environment(client, &backboard, environment_id, !args.all).await?
        }
        None => ca::list_mine(client, &backboard).await?,
    };

    if agents.is_empty() {
        if args.json {
            println!("[]");
        } else if scoped.is_some() {
            println!("No cloud agents in this environment.");
        } else {
            println!(
                "No cloud agents. Create one with {}.",
                "railway ca create".cyan()
            );
        }
        return Ok(());
    }

    let names = place_names(client, configs).await;
    agents.sort_by(|a, b| {
        let place = |agent: &ca::Agent| names.get(&agent.project_id).cloned().unwrap_or_default();
        place(a).cmp(&place(b)).then_with(|| a.name.cmp(&b.name))
    });

    if args.json {
        let out: Vec<_> = agents
            .iter()
            .map(|a| {
                serde_json::json!({
                    "id": a.id,
                    "name": a.name,
                    "status": a.status.label(),
                    "projectId": a.project_id,
                    "project": names.get(&a.project_id),
                    "environmentId": a.environment_id,
                    "environment": names.get(&a.environment_id),
                    "createdAt": a.created_at.to_rfc3339(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!(
        "  {:<24}  {:<9}  {:<22}  {:<14}  {}",
        "NAME".dimmed(),
        "STATUS".dimmed(),
        "PROJECT".dimmed(),
        "ENVIRONMENT".dimmed(),
        "AGE".dimmed()
    );
    let mut marked = false;
    for agent in &agents {
        let current = configs.get_code_agent(&agent.environment_id).as_deref() == Some(&agent.id);
        marked |= current;
        let place = |id: &String| names.get(id).cloned().unwrap_or_else(|| truncate(id, 22));
        println!(
            "{} {:<24}  {:<9}  {:<22}  {:<14}  {}",
            if current { "*" } else { " " },
            truncate(&agent.name, 24),
            colorize_status(&agent.status),
            truncate(&place(&agent.project_id), 22),
            truncate(&place(&agent.environment_id), 14),
            ca::humanize_age(agent.created_at)
        );
    }
    if marked {
        println!("\n{}", "* this directory's agent".dimmed());
    }
    Ok(())
}

pub async fn create(args: CreateArgs) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (configs, client) = (&mut configs, &client);
    let (project, environment) = (args.target.project.clone(), args.target.environment.clone());
    let (_, environment_id) =
        resolve_project_and_env(configs, client, project, environment).await?;
    let variables = variables_to_input(&args.env_files, &args.variables)?
        .map(serde_json::to_value)
        .transpose()?;

    let backboard = configs.get_backboard();
    let spinner = (!args.json).then(|| create_spinner("Creating a cloud agent".to_string()));
    let agent = match ca::create(
        client,
        &backboard,
        &environment_id,
        args.name.clone(),
        variables,
    )
    .await
    {
        Ok(agent) => agent,
        Err(e) => {
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }
            return Err(e);
        }
    };

    // Remembered before the box is up: a create that succeeds and then times
    // out waiting has still spent a VM, and the pointer is the only handle the
    // next command has on it.
    ca::remember(configs, &agent)?;

    let agent = if args.no_wait {
        agent
    } else {
        match ca::wait_until_running(client, &backboard, &environment_id, &agent.id).await {
            Ok(running) => running,
            Err(e) => {
                if let Some(spinner) = spinner {
                    spinner.finish_and_clear();
                }
                return Err(e);
            }
        }
    };
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": agent.id,
                "name": agent.name,
                "status": agent.status.label(),
                "projectId": agent.project_id,
                "environmentId": agent.environment_id,
                "createdAt": agent.created_at.to_rfc3339(),
            }))?
        );
        return Ok(());
    }

    println!(
        "✓ Created agent {} ({})",
        agent.name.cyan(),
        agent.status.label()
    );
    println!(
        "\nIt has no coding agent on it yet — {} installs one and drops you in.",
        format!("railway ca ssh {}", agent.name).cyan()
    );
    println!(
        "{}",
        format!(
            "Agents have no idle timeout: `railway ca sleep {}` stops the compute bill, `railway ca delete {}` takes the disk with it.",
            agent.name, agent.name
        )
        .dimmed()
    );
    Ok(())
}

pub async fn wake(args: WakeArgs) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (configs, client) = (&mut configs, &client);
    let (project, environment) = (args.target.project.clone(), args.target.environment.clone());
    let scoped = scope(configs, client, project, environment).await?;
    let (agent, _) = ca::resolve(configs, client, args.agent.as_deref(), scoped.as_deref()).await?;
    let backboard = configs.get_backboard();

    match agent.status {
        ca::Status::Running => {
            println!("Agent {} is already awake.", agent.name.cyan());
            ca::remember(configs, &agent)?;
            return Ok(());
        }
        ca::Status::Sleeping => ca::wake(client, &backboard, &agent.id).await?,
        // Something else is already booting it; a second wake would be noise.
        ca::Status::Starting => {}
        _ => bail!(
            "Agent {} is {} and cannot be woken.",
            agent.name,
            agent.status.label()
        ),
    }
    ca::remember(configs, &agent)?;

    if args.no_wait {
        println!("Waking agent {}…", agent.name.cyan());
        return Ok(());
    }

    let spinner = create_spinner(format!("Waking agent {}", agent.name));
    let result = ca::wait_until_running(client, &backboard, &agent.environment_id, &agent.id).await;
    spinner.finish_and_clear();
    result?;
    println!(
        "✓ Agent {} is running — your work is on its disk.",
        agent.name.cyan()
    );
    Ok(())
}

pub async fn sleep(args: SleepArgs) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (configs, client) = (&mut configs, &client);
    let (project, environment) = (args.target.project.clone(), args.target.environment.clone());
    let scoped = scope(configs, client, project, environment).await?;
    let backboard = configs.get_backboard();

    if args.all {
        let agents = match &scoped {
            Some(environment_id) => {
                ca::list_in_environment(client, &backboard, environment_id, true).await?
            }
            None => ca::list_mine(client, &backboard).await?,
        };
        let awake: Vec<_> = agents
            .into_iter()
            .filter(|a| matches!(a.status, ca::Status::Running | ca::Status::Starting))
            .collect();
        if awake.is_empty() {
            println!("No running agents to sleep.");
            return Ok(());
        }
        let spinner = create_spinner(format!("Sleeping {} agents", awake.len()));
        // Concurrently: each sleep now flushes the agent's disk over ssh first,
        // and run in sequence that would make the cost-control command take a
        // second per agent — slow enough that people stop reaching for it.
        let failed: Vec<String> = futures::future::join_all(awake.iter().map(|agent| {
            let backboard = backboard.clone();
            async move {
                ca::sleep(client, &backboard, &agent.environment_id, &agent.id)
                    .await
                    .err()
                    .map(|e| format!("  {} — {e}", agent.name))
            }
        }))
        .await
        .into_iter()
        .flatten()
        .collect();
        spinner.finish_and_clear();
        println!("✓ Slept {} agents.", awake.len() - failed.len());
        if !failed.is_empty() {
            bail!("Some agents are still running:\n{}", failed.join("\n"));
        }
        return Ok(());
    }

    let (agent, _) = ca::resolve(configs, client, args.agent.as_deref(), scoped.as_deref()).await?;
    match agent.status {
        ca::Status::Sleeping => {
            println!("Agent {} is already asleep.", agent.name.cyan());
            return Ok(());
        }
        ca::Status::Running | ca::Status::Starting => {}
        _ => bail!(
            "Agent {} is {} — there is nothing running to sleep.",
            agent.name,
            agent.status.label()
        ),
    }

    let spinner = create_spinner(format!("Sleeping agent {}", agent.name));
    let result = ca::sleep(client, &backboard, &agent.environment_id, &agent.id).await;
    spinner.finish_and_clear();
    result?;
    // Present tense, not "is asleep": the mutation returns before the agent has
    // finished transitioning, so a `railway ca list` run straight afterwards
    // still reports it running. Claiming a state the next command contradicts is
    // worse than describing the action taken.
    println!(
        "✓ Sleeping agent {} — its disk is kept, compute stops billing.",
        agent.name.cyan()
    );
    Ok(())
}

pub async fn delete(args: DeleteArgs) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (configs, client) = (&mut configs, &client);
    let (project, environment) = (args.target.project.clone(), args.target.environment.clone());
    let scoped = scope(configs, client, project, environment).await?;
    let (agent, _) = ca::resolve(configs, client, args.agent.as_deref(), scoped.as_deref()).await?;

    if !args.yes {
        // Deleting takes the disk with it, and there is no undo. A pipe cannot
        // answer the prompt, so it is told what to pass instead of hanging on a
        // read nobody will satisfy.
        if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
            bail!(
                "Refusing to delete agent {} without confirmation. Pass --yes.",
                agent.name
            );
        }
        let confirmed = prompt_confirm_with_default(
            &format!(
                "Delete agent {} and everything on its disk?",
                agent.name.cyan()
            ),
            false,
        )?;
        if !confirmed {
            println!("Left agent {} alone.", agent.name);
            return Ok(());
        }
    }

    let backboard = configs.get_backboard();
    let spinner = create_spinner(format!("Deleting agent {}", agent.name));
    let result = ca::delete(client, &backboard, &agent.id).await;
    spinner.finish_and_clear();

    // Forget the pointer whether or not the mutation reported success: a delete
    // that fails on an already-gone agent must not leave the CLI reaching for
    // it forever.
    ca::forget(configs, &agent.environment_id)?;
    result?;
    println!("✓ Deleted agent {}", agent.name.cyan());
    Ok(())
}

pub async fn ssh(args: SshArgs) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (configs, client) = (&mut configs, &client);
    let (project, environment) = (args.target.project.clone(), args.target.environment.clone());

    let scoped = scope(configs, client, project, environment).await?;
    let (agent, _) = ca::resolve(configs, client, args.agent.as_deref(), scoped.as_deref()).await?;
    let backboard = configs.get_backboard();

    // Never creates. The caller named a machine — or has exactly one — and
    // silently minting a second billed VM because a name was mistyped is not a
    // thing a connect command should be able to do.
    let was_running = matches!(agent.status, ca::Status::Running);
    let spinner = (!was_running).then(|| create_spinner(format!("Waking agent {}", agent.name)));
    let ready = ca::ensure_running(client, &backboard, &agent).await;
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }
    let agent = ready?;
    ca::remember(configs, &agent)?;

    let connected = if !args.command.is_empty() {
        run_command(&agent, &args.command).await
    } else {
        attach(client, &backboard, &agent, args.session.as_deref()).await
    };

    // A connection that never happened still woke a machine with no idle
    // timeout, so put back what this run changed — but only that. An agent
    // found already running was someone's deliberate state (possibly a session
    // open in another terminal), and a failed connect here is no reason to
    // suspend it.
    let exit_code = match connected {
        Ok(code) => code,
        Err(e) => {
            if !was_running && !args.keep_awake {
                let _ = ca::sleep(client, &backboard, &agent.environment_id, &agent.id).await;
            }
            return Err(e);
        }
    };

    if args.keep_awake {
        println!(
            "\nDisconnected — agent {} is still running (--keep-awake).",
            agent.name.cyan()
        );
    } else {
        // Agents have no idle timeout, so nothing else will ever do this.
        let spinner = create_spinner("Sleeping the agent".to_string());
        let slept = ca::sleep(client, &backboard, &agent.environment_id, &agent.id).await;
        spinner.finish_and_clear();
        match slept {
            Ok(()) => println!("\nDisconnected — sleeping agent {}.", agent.name.cyan()),
            Err(e) => eprintln!(
                "{}",
                format!(
                    "Agent {} is still running and billing compute. `railway ca sleep {}` to stop it. ({e})",
                    agent.name, agent.name
                )
                .yellow()
            ),
        }
    }

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

/// Run one command on the agent instead of attaching. This is ssh's ordinary
/// trailing-command behaviour, and `railway ca ssh -- bash` is how you get a
/// plain shell on a box whose default is a coding agent.
async fn run_command(agent: &ca::Agent, command: &[String]) -> Result<i32> {
    let info = code::connect_info(&agent.environment_id, &agent.id).await?;
    let command = command.to_vec();
    let code = tokio::task::spawn_blocking(move || {
        native::run_native_ssh_with_opts(
            &info.ssh_target,
            Some(&command),
            info.identity.as_deref(),
            None,
            &info.relay_opts,
        )
    })
    .await??;
    native::clear_mouse_tracking();
    Ok(code)
}

/// Attach to a durable session, or start one when the agent has none.
///
/// Attaching deliberately skips provisioning: the credential, the skills and
/// the harness were settled when the session was started, and walking that
/// pipeline again to arrive at a box that was ready the whole time is the
/// difference between a reattach that is instant and one that is not.
async fn attach(
    client: &reqwest::Client,
    backboard: &str,
    agent: &ca::Agent,
    requested: Option<&str>,
) -> Result<i32> {
    let sessions = ca::list_sessions(client, backboard, &agent.id).await?;
    let running: Vec<_> = sessions.into_iter().filter(|s| s.running).collect();

    let session_name = match requested {
        Some(name) => {
            if !running.iter().any(|s| s.name == name) {
                bail!(
                    "Agent {} has no running session named {name:?}.{}",
                    agent.name,
                    describe_sessions(&running)
                );
            }
            name.to_string()
        }
        None => match running.len() {
            0 => return start_session(agent).await,
            1 => running[0].name.clone(),
            _ => bail!(
                "Agent {} has {} running sessions. Pick one with --session:{}",
                agent.name,
                running.len(),
                describe_sessions(&running)
            ),
        },
    };

    println!(
        "{}",
        format!("Attaching to {} · {}", agent.name, session_name).dimmed()
    );
    let info = code::connect_info(&agent.environment_id, &agent.id).await?;
    let code = tokio::task::spawn_blocking(move || {
        native::run_native_ssh_with_opts(
            &info.ssh_target,
            None,
            info.identity.as_deref(),
            Some(native::DurableResume {
                session_name: &session_name,
                resume_from_last_read: false,
            }),
            &info.relay_opts,
        )
    })
    .await??;
    native::clear_mouse_tracking();
    Ok(code)
}

/// Start the agent's first session: install and configure the harness, then run
/// it under a *named* durable session so the platform tracks it, it survives
/// this ssh dying, and the next `railway ca ssh` reattaches instead of starting
/// a second copy.
async fn start_session(agent: &ca::Agent) -> Result<i32> {
    let harness = code::default_harness()?;
    let launch = LaunchArgs::for_target(
        agent.project_id.clone(),
        agent.environment_id.clone(),
        harness,
        false,
        None,
        Some(agent.id.clone()),
    );

    println!(
        "{}",
        format!("No session on {} yet — starting {harness}.", agent.name).dimmed()
    );
    let progress = code::CliProgress::default();
    let prepared = code::prepare(&launch, &progress).await?;
    progress.finish();

    let session_name = session::durable_name(prepared.harness);
    let remote = vec![prepared.remote_cmd.clone()];
    let target = prepared.ssh_target.clone();
    let identity = prepared.identity.clone();
    let opts = prepared.relay_opts.clone();
    let code = tokio::task::spawn_blocking(move || {
        native::run_native_ssh_with_opts(
            &target,
            Some(&remote),
            identity.as_deref(),
            Some(native::DurableResume {
                session_name: &session_name,
                resume_from_last_read: false,
            }),
            &opts,
        )
    })
    .await??;
    native::clear_mouse_tracking();
    Ok(code)
}

/// The running sessions, for an error that has to be actionable — the name is
/// what `--session` takes, so the name is what this leads with.
fn describe_sessions(sessions: &[ca::ConsoleSession]) -> String {
    if sessions.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n");
    for session in sessions {
        out.push_str(&format!(
            "  {}{}  {}\n",
            session.name,
            if session.attached { " (attached)" } else { "" },
            session.command.dimmed()
        ));
    }
    out
}

/// Project and environment names, keyed by id, for display.
///
/// Best-effort: the workspace listing is a second request, and a list that
/// prints ids because it failed is better than a list that errors.
async fn place_names(client: &reqwest::Client, configs: &Configs) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let Ok(workspaces) = crate::workspace::workspaces_with_client(client, configs).await else {
        return names;
    };
    for workspace in workspaces {
        for project in workspace.projects() {
            names.insert(project.id().to_string(), project.name().to_string());
            for environment in project.environments() {
                names.insert(environment.id, environment.name);
            }
        }
    }
    names
}

fn colorize_status(status: &ca::Status) -> colored::ColoredString {
    let label = status.label();
    match status {
        ca::Status::Running => label.green(),
        ca::Status::Sleeping => label.dimmed(),
        ca::Status::Starting => label.yellow(),
        _ => label.red(),
    }
}

/// Keep a column a column. Padding with `{:<width$}` widens on long values,
/// which turns one long project name into a table with no columns at all.
fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    let kept: String = value.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_values_alone() {
        assert_eq!(truncate("prod", 10), "prod");
        assert_eq!(truncate("exactly-10", 10), "exactly-10");
    }

    #[test]
    fn truncate_marks_elision_within_the_column() {
        let out = truncate("a-very-long-project-name", 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        // A byte-based truncate would both mis-measure this and risk slicing a
        // multi-byte char in half.
        assert_eq!(truncate("ünïcödé", 10), "ünïcödé");
        assert_eq!(truncate("ünïcödé-project", 8).chars().count(), 8);
    }
}
