use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use clap::Parser;
use is_terminal::IsTerminal;

use crate::client::{GQLClient, post_graphql};
use crate::commands::ssh::{ensure_ssh_key, run_native_ssh};
use crate::config::{Configs, StoredSandbox};
use crate::controllers::environment::get_matched_environment;
use crate::controllers::project::get_project;
use crate::gql::{mutations, queries};
use crate::util::progress::{create_shimmer_spinner, fail_spinner};
use crate::util::prompt::{prompt_options, prompt_options_skippable};

/// Manage ephemeral sandboxes
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway sandbox create            # create + remember it as active\n  railway sandbox list              # list sandboxes in the environment\n  railway sandbox ssh               # connect to the active (last) sandbox\n  railway sandbox ssh --id <id>     # connect to a specific sandbox\n  railway sandbox exec --id <id> -- ls -la\n  railway sandbox destroy --id <id>\n\nNote: requires the PROJECT_SANDBOXES feature to be enabled."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Environment name or ID (defaults to the linked environment)
    #[clap(long, short, global = true)]
    environment: Option<String>,

    /// Project ID (defaults to the linked project)
    #[clap(long, short, global = true)]
    project: Option<String>,
}

#[derive(Parser)]
enum Commands {
    /// Create a sandbox and remember it as the active sandbox
    #[clap(visible_alias = "new")]
    Create(CreateArgs),

    /// List sandboxes in the environment
    #[clap(visible_alias = "ls")]
    List(ListArgs),

    /// Connect to a sandbox over SSH (defaults to the active sandbox)
    #[clap(visible_alias = "connect")]
    Ssh(SshArgs),

    /// Run a single command inside a sandbox (defaults to the active sandbox)
    Exec(ExecArgs),

    /// Destroy a sandbox (defaults to the active sandbox)
    #[clap(visible_alias = "rm", visible_alias = "delete")]
    Destroy(DestroyArgs),
}

#[derive(Parser)]
struct CreateArgs {
    /// Minutes the sandbox may sit idle before it is auto-destroyed
    #[clap(long)]
    idle_timeout_minutes: Option<i64>,

    /// Output the created sandbox as JSON
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct ListArgs {
    /// Output as JSON
    #[clap(long)]
    json: bool,
}

/// `railway sandbox ssh [--id <id>] [-- command...]`. The id is a flag (not a
/// positional) so it's unambiguous against the trailing command; omitted → the
/// active sandbox.
#[derive(Parser)]
struct SshArgs {
    /// Sandbox ID to connect to (defaults to the active sandbox)
    #[clap(long = "id", value_name = "ID")]
    id: Option<String>,

    /// Path to an identity (private key) file, like `ssh -i`
    #[clap(short = 'i', long = "identity-file", value_name = "PATH")]
    identity_file: Option<std::path::PathBuf>,

    /// Command to run instead of an interactive shell
    #[clap(trailing_var_arg = true)]
    command: Vec<String>,
}

#[derive(Parser)]
struct ExecArgs {
    /// Sandbox ID to run in (defaults to the active sandbox)
    #[clap(long = "id", value_name = "ID")]
    id: Option<String>,

    /// Per-command timeout in seconds
    #[clap(long)]
    timeout: Option<i64>,

    /// Command to run (everything after `--`)
    #[clap(trailing_var_arg = true, required = true)]
    command: Vec<String>,
}

/// Destroy has no trailing command, so a positional id is unambiguous; `--id`
/// is also accepted. Omitted → the active sandbox.
#[derive(Parser)]
struct DestroyArgs {
    /// Sandbox ID to destroy (defaults to the active sandbox)
    #[clap(value_name = "ID")]
    id_positional: Option<String>,

    /// Sandbox ID (alternative to the positional argument)
    #[clap(long = "id", value_name = "ID")]
    id: Option<String>,
}

impl DestroyArgs {
    fn explicit_id(&self) -> Option<String> {
        self.id.clone().or_else(|| self.id_positional.clone())
    }
}

pub async fn command(args: Args) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let project = args.project;
    let environment = args.environment;

    match args.command {
        Commands::Create(sub) => create(&mut configs, &client, project, environment, sub).await,
        Commands::List(sub) => list(&mut configs, &client, project, environment, sub).await,
        Commands::Ssh(sub) => ssh(&mut configs, &client, project, environment, sub).await,
        Commands::Exec(sub) => exec(&mut configs, &client, project, environment, sub).await,
        Commands::Destroy(sub) => destroy(&mut configs, &client, project, environment, sub).await,
    }
}

/// A selectable `{id, name}` shown by name in interactive pickers.
struct Choice {
    id: String,
    name: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// Resolve `(project_id, environment_id)` for create/list. Precedence:
/// explicit `--project`/`--environment` flags → the linked project/environment
/// → an interactive picker (when attached to a TTY) → a helpful error in
/// non-interactive contexts.
async fn resolve_project_and_env(
    configs: &Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
) -> Result<(String, String)> {
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();

    // The linked project only matters when a flag is missing. Swallow its
    // errors (no link, or the RAILWAY_ENVIRONMENT_ID-without-PROJECT_ID guard)
    // and fall back to prompting when interactive.
    let linked = if project.is_none() || environment.is_none() {
        configs.get_linked_project().await.ok()
    } else {
        None
    };

    // No project from a flag or the link: run the full workspace → project →
    // environment picker, which returns both ids.
    let project_id = match project.or_else(|| linked.as_ref().map(|l| l.project.clone())) {
        Some(id) => id,
        None if interactive => return prompt_workspace_project_env(client, configs).await,
        None => {
            bail!("No project selected. Pass --project and --environment, or run `railway link`.")
        }
    };

    let project_obj = get_project(client, configs, project_id).await?;

    let environment_id = if let Some(env) = environment {
        get_matched_environment(&project_obj, env)?.id
    } else if let Some(env_id) = linked
        .as_ref()
        .filter(|l| l.project == project_obj.id)
        .and_then(|l| l.environment.clone())
    {
        get_matched_environment(&project_obj, env_id)?.id
    } else if interactive {
        prompt_environment(&project_obj)?
    } else {
        bail!("No environment selected. Pass --environment, or run `railway link`.");
    };

    Ok((project_obj.id, environment_id))
}

/// Full interactive picker: workspace → project → environment. `Esc` steps back
/// to the previous selection (`Esc` at the workspace level cancels). Returns
/// `(project_id, environment_id)`. Uses the OAuth-safe `UserProjects` listing
/// (what `railway list` uses) — the `projects(workspaceId:)` root field is not
/// authorized for plain user tokens.
async fn prompt_workspace_project_env(
    client: &reqwest::Client,
    configs: &Configs,
) -> Result<(String, String)> {
    let workspaces = crate::workspace::workspaces_with_client(client, configs).await?;
    if workspaces.is_empty() {
        bail!("No workspaces found. Create a project at https://railway.com/new");
    }

    // Workspace level. Esc here cancels the whole operation.
    loop {
        let ws_choices: Vec<Choice> = workspaces
            .iter()
            .map(|w| Choice {
                id: w.id().to_string(),
                name: w.name().to_string(),
            })
            .collect();
        let ws_id = match prompt_options_skippable("Select a workspace", ws_choices)? {
            Some(choice) => choice.id,
            None => bail!("Cancelled."),
        };
        let workspace = workspaces
            .iter()
            .find(|w| w.id() == ws_id)
            .expect("selected workspace exists");
        let projects = workspace.projects();
        if projects.is_empty() {
            eprintln!("That workspace has no projects.");
            continue; // back to workspace selection
        }

        // Project level. Esc steps back to workspace selection.
        'project: loop {
            let proj_choices: Vec<Choice> = projects
                .iter()
                .map(|p| Choice {
                    id: p.id().to_string(),
                    name: p.name().to_string(),
                })
                .collect();
            let project_id = match prompt_options_skippable("Select a project", proj_choices)? {
                Some(choice) => choice.id,
                None => break 'project,
            };

            let project_obj = get_project(client, configs, project_id).await?;
            let env_choices: Vec<Choice> = project_obj
                .environments
                .edges
                .iter()
                .filter(|e| e.node.can_access)
                .map(|e| Choice {
                    id: e.node.id.clone(),
                    name: e.node.name.clone(),
                })
                .collect();
            if env_choices.is_empty() {
                eprintln!("That project has no accessible environments.");
                continue 'project;
            }

            // Environment level. Esc steps back to project selection.
            match prompt_options_skippable("Select an environment", env_choices)? {
                Some(choice) => return Ok((project_obj.id, choice.id)),
                None => continue 'project,
            }
        }
    }
}

/// Interactively pick an accessible environment from a project.
fn prompt_environment(project: &queries::RailwayProject) -> Result<String> {
    let choices: Vec<Choice> = project
        .environments
        .edges
        .iter()
        .filter(|e| e.node.can_access)
        .map(|e| Choice {
            id: e.node.id.clone(),
            name: e.node.name.clone(),
        })
        .collect();
    if choices.is_empty() {
        bail!("No accessible environments in this project.");
    }
    Ok(prompt_options("Select an environment", choices)?.id)
}

/// Resolve which sandbox a command should act on: an explicit id (using the
/// local store / flags / linked project to recover its environment), or the
/// active sandbox when none is given.
async fn resolve_target(
    configs: &Configs,
    client: &reqwest::Client,
    explicit_id: Option<String>,
    project: Option<String>,
    environment: Option<String>,
) -> Result<(String, String)> {
    match explicit_id {
        Some(id) => {
            let environment_id = if project.is_some() || environment.is_some() {
                resolve_project_and_env(configs, client, project, environment)
                    .await?
                    .1
            } else if let Some(stored) = configs.get_sandbox(&id) {
                stored.environment_id
            } else {
                resolve_project_and_env(configs, client, None, None)
                    .await?
                    .1
            };
            Ok((id, environment_id))
        }
        None => {
            let stored = configs.get_active_sandbox().ok_or_else(|| {
                anyhow!(
                    "No active sandbox. Create one with `railway sandbox create`, or pass --id <id>."
                )
            })?;
            Ok((stored.id, stored.environment_id))
        }
    }
}

async fn create(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: CreateArgs,
) -> Result<()> {
    let (project_id, environment_id) =
        resolve_project_and_env(configs, client, project, environment).await?;

    let mut spinner = create_shimmer_spinner("Creating sandbox");
    let sandbox = match post_graphql::<mutations::SandboxCreate, _>(
        client,
        configs.get_backboard(),
        mutations::sandbox_create::Variables {
            input: mutations::sandbox_create::SandboxCreateInput {
                environment_id: environment_id.clone(),
                idle_timeout_minutes: args.idle_timeout_minutes,
                template: None,
            },
        },
    )
    .await
    {
        Ok(res) => res.sandbox_create,
        Err(e) => {
            fail_spinner(&mut spinner, "Failed to create sandbox".to_string());
            return Err(e.into());
        }
    };
    spinner.finish_and_clear();

    configs.upsert_sandbox(
        StoredSandbox {
            id: sandbox.id.clone(),
            environment_id,
            project_id: Some(project_id),
            created_at: Some(sandbox.created_at.to_rfc3339()),
        },
        true,
    );
    configs.write()?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&sandbox)?);
    } else {
        println!("✓ Created sandbox {} (now active)", sandbox.id);
        println!("  status: {:?}", sandbox.status);
        println!("  region: {}", sandbox.region);
        if let Some(idle) = sandbox.idle_timeout_minutes {
            println!("  idle timeout: {idle}m");
        }
        println!("\nConnect with:\n  railway sandbox ssh");
    }
    Ok(())
}

async fn list(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: ListArgs,
) -> Result<()> {
    let (project_id, environment_id) =
        resolve_project_and_env(configs, client, project, environment).await?;

    let res = post_graphql::<queries::Sandboxes, _>(
        client,
        configs.get_backboard(),
        queries::sandboxes::Variables {
            environment_id: environment_id.clone(),
            first: Some(100),
            after: None,
        },
    )
    .await?;
    let nodes: Vec<_> = res.sandboxes.edges.into_iter().map(|e| e.node).collect();

    // Refresh the local id -> environment cache so `--id` works for any listed
    // sandbox. Does not change which sandbox is active.
    for node in &nodes {
        configs.upsert_sandbox(
            StoredSandbox {
                id: node.id.clone(),
                environment_id: environment_id.clone(),
                project_id: Some(project_id.clone()),
                created_at: Some(node.created_at.to_rfc3339()),
            },
            false,
        );
    }
    configs.write()?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&nodes)?);
        return Ok(());
    }

    if nodes.is_empty() {
        println!("No sandboxes in this environment.");
        return Ok(());
    }

    let active = configs.get_active_sandbox().map(|s| s.id);
    println!(
        "{:<38}  {:<10}  {:<10}  {:<16}",
        "ID", "STATUS", "REGION", "CREATED"
    );
    for node in nodes {
        let marker = if active.as_deref() == Some(node.id.as_str()) {
            "*"
        } else {
            " "
        };
        println!(
            "{marker} {:<38}  {:<10}  {:<10}  {:<16}",
            node.id,
            format!("{:?}", node.status),
            node.region,
            node.created_at.format("%Y-%m-%d %H:%M").to_string()
        );
    }
    Ok(())
}

async fn exec(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: ExecArgs,
) -> Result<()> {
    let (sandbox_id, environment_id) =
        resolve_target(configs, client, args.id, project, environment).await?;

    configs.set_active_sandbox(&sandbox_id);
    configs.write()?;

    let res = post_graphql::<mutations::SandboxExec, _>(
        client,
        configs.get_backboard(),
        mutations::sandbox_exec::Variables {
            id: sandbox_id,
            environment_id,
            command: args.command.join(" "),
            timeout_sec: args.timeout,
        },
    )
    .await?;
    let result = res.sandbox_exec;

    print!("{}", result.stdout);
    eprint!("{}", result.stderr);
    if result.timed_out {
        eprintln!("\n(command timed out)");
    }
    std::process::exit(result.exit_code as i32);
}

async fn destroy(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: DestroyArgs,
) -> Result<()> {
    let (sandbox_id, environment_id) =
        resolve_target(configs, client, args.explicit_id(), project, environment).await?;

    let mut spinner = create_shimmer_spinner("Destroying sandbox");
    if let Err(e) = post_graphql::<mutations::SandboxDestroy, _>(
        client,
        configs.get_backboard(),
        mutations::sandbox_destroy::Variables {
            id: sandbox_id.clone(),
            environment_id,
        },
    )
    .await
    {
        fail_spinner(&mut spinner, "Failed to destroy sandbox".to_string());
        return Err(e.into());
    }
    spinner.finish_and_clear();

    configs.remove_sandbox(&sandbox_id);
    configs.write()?;
    println!("✓ Destroyed sandbox {sandbox_id}");
    Ok(())
}

/// How often to extend the sandbox's idle lifetime during an interactive
/// session. The backboard SSH handshake extends once on connect; this keeps a
/// long-lived shell alive against the idle reaper.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

async fn ssh(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: SshArgs,
) -> Result<()> {
    let (sandbox_id, environment_id) =
        resolve_target(configs, client, args.id.clone(), project, environment).await?;

    // Reuse the native-SSH key registration flow from `railway ssh`. When the
    // user didn't pass `-i`, use the registered key it resolves so a
    // non-default-named key (e.g. ~/.ssh/raildesk_railway_ed25519) is actually
    // offered to the relay instead of just ssh's default identities.
    let auto_identity = if args.identity_file.is_none() {
        ensure_ssh_key(client, configs).await?
    } else {
        None
    };

    configs.set_active_sandbox(&sandbox_id);
    configs.write()?;

    // Relay target format (per backboard): sbx:<environmentId>:<sandboxId>.
    // `run_native_ssh` appends `@ssh.railway.com` internally.
    let target = format!("sbx:{environment_id}:{sandbox_id}");

    // Keep the sandbox alive for the duration of the session.
    let heartbeat = spawn_heartbeat(
        client.clone(),
        configs.get_backboard(),
        environment_id,
        sandbox_id,
    );

    let command = if args.command.is_empty() {
        None
    } else {
        Some(args.command.clone())
    };
    let identity = args.identity_file.clone().or(auto_identity);

    // `run_native_ssh` is blocking (inherits the terminal); run it off the
    // async runtime so the heartbeat task keeps ticking.
    let exit_code = tokio::task::spawn_blocking(move || {
        run_native_ssh(&target, command.as_deref(), identity.as_deref())
    })
    .await??;

    heartbeat.abort();

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

fn spawn_heartbeat(
    client: reqwest::Client,
    backboard: String,
    environment_id: String,
    sandbox_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
        // Skip the immediate first tick — backboard already extended on connect.
        interval.tick().await;
        loop {
            interval.tick().await;
            let _ = post_graphql::<mutations::SandboxHeartbeat, _>(
                &client,
                backboard.clone(),
                mutations::sandbox_heartbeat::Variables {
                    id: sandbox_id.clone(),
                    environment_id: environment_id.clone(),
                },
            )
            .await;
        }
    })
}
