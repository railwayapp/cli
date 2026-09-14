//! The flat `railway ca` verbs: list, create, ssh, wake, sleep, delete.
//!
//! Each addresses an agent that already exists, by name or id. Only `create`
//! makes one — `ssh` connects to what is there and errors otherwise, and
//! `railway ca start` remains the create-and-launch path. That line is the
//! point of the split: a mistyped agent name should be an error, not a second
//! billed VM.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::Parser;
use colored::Colorize;
use is_terminal::IsTerminal;

use crate::client::GQLClient;
use crate::commands::cloud_agent::telemetry;
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

impl TargetArgs {
    /// Resolve creation/bootstrap scope with the launcher's directory and preference precedence.
    pub(crate) async fn resolve(
        self,
        configs: &mut Configs,
        client: &reqwest::Client,
    ) -> Result<String> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
        let mut prefs = super::prefs::AgentPrefs::load_in(&home).unwrap_or_default();
        let mut args = LaunchArgs::default();
        args.project = self.project;
        args.environment = self.environment;
        Ok(
            code::resolve_target(configs, client, &args, &mut prefs, &home)
                .await?
                .environment_id,
        )
    }
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
#[clap(after_help = r#"Examples:
  railway ca create my-box
  railway ca create my-box --bootstrap dev
  railway ca create my-box --from-checkpoint <checkpoint-id>

Creates a VM without connecting. Uses the local default bootstrap unless
--no-bootstrap or --from-checkpoint is given. --bootstrap accepts a name;
--from-checkpoint requires a cloud-agent checkpoint ID."#)]
pub struct CreateArgs {
    /// Name for the agent (defaults to a generated one)
    #[clap(value_name = "NAME")]
    name: Option<String>,

    /// Provision the public code endpoint (defaults to port 4096)
    #[clap(long)]
    code_endpoint: bool,

    /// Provision the public code endpoint on this port
    #[clap(long, value_parser = ca::parse_code_port)]
    code_port: Option<u16>,

    /// Restore this cloud-agent checkpoint into the new VM
    #[clap(long, value_name = "CHECKPOINT_ID")]
    from_checkpoint: Option<String>,

    /// Start from this named bootstrap instead of your local environment default
    #[clap(long, conflicts_with_all = ["from_checkpoint", "no_bootstrap"])]
    bootstrap: Option<String>,

    /// Create a clean VM without your local environment default bootstrap
    #[clap(long, conflicts_with = "from_checkpoint")]
    no_bootstrap: bool,

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
#[clap(after_help = r#"Examples:
  railway ca ssh my-box                    # open a shell
  railway ca ssh my-box --session          # attach to or start a session
  railway ca ssh my-box --resume           # resume the latest Claude conversation
  railway ca ssh my-box -- ls -la /app     # run a command

Disconnecting leaves the VM running. Sleep ends its processes and keeps its disk."#)]
pub struct SshArgs {
    /// Agent name or ID (defaults to this directory's, or your only one)
    #[clap(value_name = "AGENT")]
    agent: Option<String>,

    /// Attach to a durable session, optionally by name, instead of opening a shell
    #[clap(long, value_name = "NAME", num_args = 0..=1, default_missing_value = "", conflicts_with = "command")]
    session: Option<String>,

    /// Resume the most recent Claude conversation in a new terminal session
    #[clap(long, conflicts_with_all = ["session", "command"])]
    resume: bool,

    /// Accepted for compatibility; agents now always stay running on
    /// disconnect. `railway ca sleep` stops the compute bill
    #[clap(long, hide = true)]
    keep_awake: bool,

    #[clap(flatten)]
    target: TargetArgs,

    /// Run this command instead of opening a shell
    #[clap(trailing_var_arg = true)]
    command: Vec<String>,
}

impl SshArgs {
    /// A plain SSH connection always opens a shell, even on a VM configured
    /// to autostart a harness. Session attachment and resume are explicit.
    fn remote_command(&self) -> Option<Vec<String>> {
        if !self.command.is_empty() {
            Some(self.command.clone())
        } else if self.session.is_some() || self.resume {
            None
        } else {
            Some(vec![code::LOGIN_SHELL_COMMAND.to_string()])
        }
    }
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
    let environment_id = args.target.resolve(configs, client).await?;
    let variables = variables_to_input(&args.env_files, &args.variables)?
        .map(serde_json::to_value)
        .transpose()?;

    let backboard = configs.get_backboard();
    let bootstrap = crate::controllers::agent_bootstrap::resolve_for_create(
        configs,
        client,
        &backboard,
        &environment_id,
        args.bootstrap.as_deref(),
        args.no_bootstrap || args.from_checkpoint.is_some(),
    )
    .await?;
    let spinner = (!args.json).then(|| create_spinner("Creating a cloud agent".to_string()));
    let agent = match ca::create(
        client,
        &backboard,
        &environment_id,
        args.name.clone(),
        variables,
        ca::CreateOptions {
            code_port: args.code_port.or(args.code_endpoint.then_some(4096)),
            checkpoint_id: args.from_checkpoint,
            bootstrap_id: bootstrap.map(|b| b.id),
        },
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
    wake_with(&mut configs, &client, args).await
}

async fn wake_with(configs: &mut Configs, client: &reqwest::Client, args: WakeArgs) -> Result<()> {
    let (project, environment) = (args.target.project.clone(), args.target.environment.clone());
    let scoped = scope(configs, client, project, environment).await?;
    let (agent, _) = ca::resolve(configs, client, args.agent.as_deref(), scoped.as_deref()).await?;
    let backboard = configs.get_backboard();

    // The inventory is an observation. Let the mutation's live state check
    // decide whether waking is necessary, including when we last saw RUNNING.
    ca::wake(client, &backboard, &agent.id).await?;
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
        // A sleeping observation may predate a wake elsewhere. Send the intent
        // for every agent; the server handles already-asleep agents as no-ops.
        if agents.is_empty() {
            println!("No cloud agents to sleep.");
            return Ok(());
        }
        telemetry::track_lifecycle("sleep_all", Duration::ZERO, None).await;
        let spinner = create_spinner(format!("Requesting sleep for {} agents", agents.len()));
        // Concurrently: each sleep now flushes the agent's disk over ssh first,
        // and run in sequence that would make the cost-control command take a
        // second per agent — slow enough that people stop reaching for it.
        let failed: Vec<String> = futures::future::join_all(agents.iter().map(|agent| {
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
        println!(
            "✓ Sleep requested for {} agents.",
            agents.len() - failed.len()
        );
        if !failed.is_empty() {
            bail!("Some sleep requests failed:\n{}", failed.join("\n"));
        }
        return Ok(());
    }

    let (agent, _) = ca::resolve(configs, client, args.agent.as_deref(), scoped.as_deref()).await?;
    let spinner = create_spinner(format!("Sleeping agent {}", agent.name));
    let result = ca::sleep(client, &backboard, &agent.environment_id, &agent.id).await;
    spinner.finish_and_clear();
    result?;
    // Present tense, not "is asleep": the mutation returns before the agent has
    // finished transitioning, so a `railway ca list` run straight afterwards
    // still reports it running. Claiming a state the next command contradicts is
    // worse than describing the action taken.
    println!(
        "✓ Sleep requested for agent {} — its disk is kept; compute stops billing once it is asleep.",
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
    let started = std::time::Instant::now();
    let result = ssh_connect(args).await;
    let message = result.as_ref().err().map(|e| format!("{e:#}"));
    telemetry::track_lifecycle("ssh", started.elapsed(), message.as_deref()).await;

    // A non-zero remote exit is the command's result, not a failure of ours, so
    // it is reported as a success above and only then propagated as our own exit
    // status — `exit` never returns, and reporting after it would never happen.
    match result? {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

/// The TUI's shell shortcut targets an existing VM by ID. Return the SSH
/// status instead of exiting the CLI, so the manage screen can resume.
pub(super) async fn ssh_shell(agent_id: String) -> Result<i32> {
    ssh_connect(SshArgs {
        agent: Some(agent_id),
        session: None,
        resume: false,
        keep_awake: false,
        target: TargetArgs {
            environment: None,
            project: None,
        },
        command: Vec::new(),
    })
    .await
}

/// Connect, and hand back what the remote side exited with.
async fn ssh_connect(args: SshArgs) -> Result<i32> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (configs, client) = (&mut configs, &client);
    let (project, environment) = (args.target.project.clone(), args.target.environment.clone());

    let scoped = scope(configs, client, project, environment).await?;
    let backboard = configs.get_backboard();

    // A mistyped name still fails — silently minting a second billed VM
    // because of a typo is not a thing a connect command should be able to
    // do. But an account with no agents at all has no wrong machine to pick:
    // asking someone to run `railway ca create` and come back is a hoop, so
    // the first agent is made here, where setup said new agents live.
    let resolved =
        ca::resolve_or_none(configs, client, args.agent.as_deref(), scoped.as_deref()).await?;
    let (agent, _) = match resolved {
        Some(found) => found,
        None => {
            let home =
                dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to get home directory"))?;
            let default =
                super::prefs::AgentPrefs::load_in(&home).and_then(|prefs| prefs.default_project);
            // `railway code`'s order exactly (see its `choose_target`): flags,
            // then the linked directory, then the configured default. Landing
            // somewhere else than `railway code` would from the same directory
            // is the inconsistency that order exists to prevent — so the link
            // is consulted here even though `scope` deliberately does not.
            let linked = if scoped.is_none() {
                configs
                    .get_linked_project()
                    .await
                    .ok()
                    .and_then(|l| l.environment.clone())
            } else {
                None
            };
            let (environment_id, where_label) = match (&scoped, linked, &default) {
                (Some(env), _, _) => (env.clone(), "this environment".to_string()),
                (None, Some(env), _) => (env, "this linked directory's project".to_string()),
                (None, None, Some(project)) => (
                    project.environment_id.clone(),
                    format!("{} ({})", project.project_name, project.environment_name),
                ),
                (None, None, None) => bail!(
                    "You have no cloud agents. Create one with `railway ca create`, \
                     or set a default project with `railway ca setup`."
                ),
            };
            println!(
                "{}",
                format!("No cloud agents yet — creating one in {where_label}.").dimmed()
            );
            let agent = ca::create(
                client,
                &backboard,
                &environment_id,
                None,
                None,
                ca::CreateOptions::default(),
            )
            .await?;
            (agent, ca::Resolution::Sole)
        }
    };

    let was_running = matches!(agent.status, ca::Status::Running);
    let spinner = (!was_running).then(|| create_spinner(format!("Waking agent {}", agent.name)));
    // Probe the route instead of polling status to RUNNING: the platform
    // routes a shell as soon as the container exists, several seconds before
    // the status flips. STARTING means something else is already booting the
    // box, so this waits rather than issuing a second wake.
    let ready = match agent.status {
        ca::Status::Running => Ok(()),
        // relay_access errors flow into `ready` rather than `?`-ing out: the
        // spinner above is cleared only after this match, and an early return
        // would leave it animating over the error output.
        ca::Status::Starting => match code::relay_access().await {
            Ok(access) => code::wait_until_connectable(
                client,
                &backboard,
                &agent.environment_id,
                &agent.id,
                &access,
                // Caught mid-boot: it may be routable right now.
                std::time::Duration::ZERO,
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
                    std::time::Duration::from_millis(350),
                )
                .await
                .map(|_| ()),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        },
        _ => Err(anyhow::anyhow!(
            "Agent {} is reported as {} and cannot be connected to. Check `railway ca list` and retry, or use `railway ca create` to create a separate agent.",
            agent.name,
            agent.status.label()
        )),
    };
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }
    ready?;
    ca::remember(configs, &agent)?;

    let connected = if let Some(command) = args.remote_command() {
        telemetry::track_lifecycle_detached("ssh_command");
        run_command(&agent, &command).await
    } else {
        attach(
            client,
            &backboard,
            &agent,
            args.session.as_deref().filter(|name| !name.is_empty()),
            args.resume,
            was_running,
        )
        .await
    };

    // The user's work is done; let detached telemetry finish before the
    // process exits (bounded — see drain_detached).
    crate::commands::ssh::tel::drain_detached(Duration::from_secs(2)).await;

    // A connection that never happened still woke a machine with no idle
    // timeout, so put back what this run changed — but only that. An agent
    // found already running was someone's deliberate state (possibly a session
    // open in another terminal), and a failed connect here is no reason to
    // suspend it.
    let exit_code = match connected {
        Ok(code) => code,
        Err(e) => {
            if !was_running {
                let _ = ca::sleep(client, &backboard, &agent.environment_id, &agent.id).await;
            }
            return Err(e);
        }
    };

    // Disconnecting no longer sleeps the agent: sleep kills every process on
    // the VM — including the durable session just detached from — while the
    // platform keeps listing those sessions as running, so the next reattach
    // landed on a dead name and a blank screen. Sleeping is deliberate now.
    println!(
        "\nDisconnected — agent {} is still running. `railway ca sleep {}` stops the compute bill.",
        agent.name.cyan(),
        agent.name
    );

    Ok(exit_code)
}

/// Run one command on the agent instead of attaching. This is ssh's ordinary
/// trailing-command behaviour. Bare `railway ca ssh` supplies the login-shell
/// command with the harness autostart disabled.
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
    resume: bool,
    was_running: bool,
) -> Result<i32> {
    // An explicit --resume doesn't attach at all: the user is saying the
    // conversation they want has no terminal any more (a sleep or reboot took
    // it), so reopen the newest one directly.
    if resume {
        let threads = resumable_threads(client, backboard, agent).await?;
        let Some(thread) = threads.into_iter().next() else {
            bail!(
                "Agent {} has no resumable Claude conversations. \
                 `railway ca ssh {} --session` starts a fresh session.",
                agent.name,
                agent.name,
            );
        };
        telemetry::track_lifecycle_detached("ssh_resume_session");
        return start_session(agent, Some(thread.session_id)).await;
    }

    let sessions = ca::list_sessions(client, backboard, &agent.id).await?;
    let mut running: Vec<_> = sessions.into_iter().filter(|s| s.running).collect();

    // An agent that was asleep a moment ago cannot have a live session:
    // sleeping stopped every process on the VM, but the platform's session
    // records can keep saying "running". Believing them attaches to a dead
    // name — the relay resolves it, streams nothing, and the screen stays
    // blank. Skip the zombies and start fresh instead.
    if !was_running && !running.is_empty() {
        println!(
            "{}",
            format!(
                "Ignoring {} listed session{} on {} — {} ended when the agent last slept.",
                running.len(),
                if running.len() == 1 { "" } else { "s" },
                agent.name,
                if running.len() == 1 { "it" } else { "they" },
            )
            .dimmed()
        );
        running.clear();
    }

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
            0 => {
                // No terminal to attach to — but the platform may still know
                // conversations whose transcripts survived on the disk (a
                // sleep or reboot ends every terminal, never the work).
                if let Some(id) = offer_resume(client, backboard, agent).await? {
                    telemetry::track_lifecycle_detached("ssh_resume_session");
                    return start_session(agent, Some(id)).await;
                }
                telemetry::track_lifecycle_detached("ssh_new_session");
                return start_session(agent, None).await;
            }
            1 => running[0].name.clone(),
            _ => bail!(
                "Agent {} has {} running sessions. Pick one with --session:{}",
                agent.name,
                running.len(),
                describe_sessions(&running)
            ),
        },
    };

    telemetry::track_lifecycle_detached("ssh_attach");
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

/// The conversations `claude --resume <id>` can reopen on this agent, newest
/// first. Claude only: it is the one harness with a verified resume-by-id CLI.
/// Read failures degrade to "none" — resume is an offer, and a listing hiccup
/// must not block connecting.
async fn resumable_threads(
    client: &reqwest::Client,
    backboard: &str,
    agent: &ca::Agent,
) -> Result<Vec<ca::SessionThread>> {
    let threads = ca::list_session_threads(client, backboard, &agent.id, &agent.environment_id)
        .await
        .unwrap_or_default();
    Ok(threads
        .into_iter()
        .filter(|thread| thread.harness == "claude" && !thread.session_id.is_empty())
        .collect())
}

/// One line per resumable conversation, prompt first — the prompt is how the
/// user recognizes their work; the state and age qualify it.
struct ResumeChoice(ca::SessionThread);

impl std::fmt::Display for ResumeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prompt = self.0.prompt.as_deref().unwrap_or("(no prompt recorded)");
        let mut prompt: String = prompt.chars().take(56).collect();
        if self
            .0
            .prompt
            .as_deref()
            .is_some_and(|p| p.chars().count() > 56)
        {
            prompt.push('…');
        }
        write!(
            f,
            "{prompt} · {}{}",
            self.0.state,
            thread_age(&self.0.updated_at)
        )
    }
}

/// " · 3h ago" when the thread's timestamp parses, nothing when it doesn't.
fn thread_age(updated_at: &str) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(updated_at) else {
        return String::new();
    };
    let minutes = (chrono::Utc::now() - then.with_timezone(&chrono::Utc)).num_minutes();
    let age = match minutes {
        m if m < 1 => "just now".to_string(),
        m if m < 60 => format!("{m}m ago"),
        m if m < 60 * 24 => format!("{}h ago", m / 60),
        m => format!("{}d ago", m / (60 * 24)),
    };
    format!(" · {age}")
}

/// When the agent has resumable conversations and a person is at the terminal,
/// ask whether to reopen one; `None` means start fresh (the default, and the
/// only behavior for scripted callers).
async fn offer_resume(
    client: &reqwest::Client,
    backboard: &str,
    agent: &ca::Agent,
) -> Result<Option<String>> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Ok(None);
    }
    let threads = resumable_threads(client, backboard, agent).await?;
    if threads.is_empty() {
        return Ok(None);
    }
    let mut options = vec!["Start a fresh session".to_string()];
    options.extend(
        threads
            .iter()
            .take(5)
            .map(|thread| format!("Resume: {}", ResumeChoice(thread.clone()))),
    );
    let picked = crate::util::prompt::prompt_select_with_cancel(
        &format!(
            "{} has Claude conversations from before it last stopped",
            agent.name
        ),
        options.clone(),
    )?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let index = options.iter().position(|option| *option == picked);
    Ok(match index {
        Some(0) | None => None,
        Some(i) => threads.get(i - 1).map(|thread| thread.session_id.clone()),
    })
}

/// Start the agent's first session: install and configure the harness, then run
/// it under a *named* durable session so the platform tracks it, it survives
/// this ssh dying, and the next `railway ca ssh --session` reattaches instead of starting
/// a second copy. With a resume id, the session relaunches Claude continuing
/// that conversation instead of starting the default harness fresh.
async fn start_session(agent: &ca::Agent, resume_session_id: Option<String>) -> Result<i32> {
    let harness = match resume_session_id {
        // The id came from a claude thread; the default harness may differ.
        Some(_) => "claude",
        None => code::default_harness()?,
    };
    let mut launch = LaunchArgs::for_target(
        agent.project_id.clone(),
        agent.environment_id.clone(),
        harness,
        false,
        None,
        Some(agent.id.clone()),
    );
    launch.resume_session_id = resume_session_id;

    println!(
        "{}",
        if launch.resume_session_id.is_some() {
            format!("Resuming your Claude conversation on {}.", agent.name)
        } else {
            format!("No session on {} yet — starting {harness}.", agent.name)
        }
        .dimmed()
    );
    let progress = code::CliProgress::default();
    let prepared = code::prepare(&launch, &progress, code::SessionStyle::FullTerminal).await?;
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
    use crate::testkit::MockBackboard;
    use serde_json::json;

    #[test]
    fn create_accepts_checkpoint_and_optional_code_endpoint() {
        let args = CreateArgs::try_parse_from([
            "create",
            "restored",
            "--from-checkpoint",
            "checkpoint",
            "--code-port",
            "5000",
        ])
        .unwrap();
        assert_eq!(args.from_checkpoint.as_deref(), Some("checkpoint"));
        assert_eq!(args.code_port, Some(5000));
        assert!(
            !CreateArgs::try_parse_from(["create", "plain"])
                .unwrap()
                .code_endpoint
        );
        assert!(
            CreateArgs::try_parse_from(["create", "coded", "--code-endpoint"])
                .unwrap()
                .code_endpoint
        );
        assert!(CreateArgs::try_parse_from(["create", "--code-port", "8080"]).is_err());
    }

    #[tokio::test]
    async fn wake_sends_intent_even_when_the_inventory_reports_running() {
        for status in ["RUNNING", "STARTING", "SLEEPING", "FAILED", "FUTURE_STATE"] {
            let server = MockBackboard::spawn();
            let dir = tempfile::tempdir().unwrap();
            let mut configs = server.configs(&dir);
            server.stub(
                "MyCloudAgents",
                json!({"myCloudAgents": [{
                    "id": "existing", "name": "my-agent", "status": status,
                    "projectId": "project", "environmentId": "env",
                    "createdAt": "2026-09-10T00:00:00Z"
                }]}),
            );
            server.stub(
                "CloudAgentWake",
                json!({"cloudAgentWake": {
                    "id": "existing", "status": "STARTING"
                }}),
            );
            server.stub_graphql_error("CloudAgentWake", "cannot wake now");
            wake_with(
                &mut configs,
                &reqwest::Client::new(),
                WakeArgs::parse_from(["wake", "my-agent", "--no-wait"]),
            )
            .await
            .unwrap();
            assert_eq!(
                server.variables_for("CloudAgentWake"),
                vec![json!({"id": "existing"})]
            );
            assert!(
                server.variables_for("CloudAgent").is_empty(),
                "--no-wait must not poll"
            );
            assert_eq!(configs.get_code_agent("env").as_deref(), Some("existing"));

            // A newer server-side refusal must not be hidden by the old status.
            let error = wake_with(
                &mut configs,
                &reqwest::Client::new(),
                WakeArgs::parse_from(["wake", "my-agent", "--no-wait"]),
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("cannot wake now"), "{error}");
        }
    }

    #[test]
    fn ssh_defaults_to_a_shell_and_attaches_only_when_requested() {
        let bare = SshArgs::try_parse_from(["ssh", "my-agent"]).unwrap();
        assert_eq!(
            bare.remote_command(),
            Some(vec![code::LOGIN_SHELL_COMMAND.to_string()])
        );
        let attached =
            SshArgs::try_parse_from(["ssh", "my-agent", "--session", "railway-abc"]).unwrap();
        assert_eq!(attached.remote_command(), None);
        let automatic = SshArgs::try_parse_from(["ssh", "my-agent", "--session"]).unwrap();
        assert_eq!(automatic.remote_command(), None);
        assert_eq!(automatic.session.as_deref(), Some(""));
        let resumed = SshArgs::try_parse_from(["ssh", "my-agent", "--resume"]).unwrap();
        assert_eq!(resumed.remote_command(), None);
        let command =
            SshArgs::try_parse_from(["ssh", "my-agent", "--", "bash", "-lc", "pwd"]).unwrap();
        assert_eq!(
            command.remote_command(),
            Some(vec!["bash".into(), "-lc".into(), "pwd".into()])
        );
        for flag in [vec!["--resume"], vec!["--session", "railway-abc"]] {
            let mut args = vec!["ssh", "my-agent"];
            args.extend(flag);
            args.extend(["--", "bash"]);
            assert!(SshArgs::try_parse_from(args).is_err());
        }
    }

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
