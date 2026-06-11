use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use clap::Parser;
use is_terminal::IsTerminal;

use crate::client::{GQLClient, post_graphql};
use crate::commands::ssh::{
    DurableResume, PortForward, ensure_ssh_key, run_native_ssh, run_native_ssh_forward, tel,
};
use crate::config::{Configs, StoredSandbox, StoredSandboxTemplate};
use crate::controllers::environment::get_matched_environment;
use crate::controllers::project::get_project;
use crate::controllers::sandbox_exec::{self, ExecOutcome};
use crate::controllers::variables::Variable;
use crate::gql::{mutations, queries};
use crate::util::progress::{create_shimmer_spinner, fail_spinner};
use crate::util::prompt::{
    prompt_confirm_with_default_with_cancel, prompt_options, prompt_options_skippable,
};
use crate::util::shell::shell_join;

/// Manage ephemeral sandboxes
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway sandbox create            # create + remember it as active\n  railway sandbox create --variable FOO=bar,DB_URL=postgres.DATABASE_URL\n  railway sandbox create --env-file .env\n  railway sandbox template build --name dev -c 'npm i -g pnpm' --wait\n  railway sandbox create --template dev   # boot from the pre-built snapshot\n  railway sandbox checkpoint create my-setup       # capture the active sandbox's disk\n  railway sandbox create --checkpoint my-setup     # boot a new sandbox from it\n  railway sandbox checkpoint list   # list named checkpoints in the environment\n  railway sandbox list              # list sandboxes in the environment\n  railway sandbox ssh               # connect to the active (last) sandbox\n  railway sandbox ssh --id <id>     # connect to a specific sandbox\n  railway sandbox exec --id <id> -- ls -la\n  railway sandbox exec --detach -- npm run build   # leave it running, prints a session name\n  railway sandbox exec --session <name>            # reattach to a detached/disconnected command\n  railway sandbox forward 3000      # localhost:3000 → port 3000 in the active sandbox\n  railway sandbox forward 8080:3000 # localhost:8080 → port 3000 (explicit local port)\n  railway sandbox forward 3000 5432 # several ports over one connection\n  railway sandbox fork              # fork the active sandbox; the fork becomes active\n  railway sandbox fork <id> --variable FOO=bar\n  railway sandbox destroy --id <id>\n\nNote: requires the PROJECT_SANDBOXES feature to be enabled."
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

    /// Fork an existing sandbox into a new one and make it active
    Fork(ForkArgs),

    /// Manage sandbox templates (pre-built filesystem snapshots)
    Template(TemplateArgs),

    /// Manage sandbox checkpoints (named disk snapshots captured from running
    /// sandboxes, bootable with `create --checkpoint`)
    Checkpoint(CheckpointArgs),

    /// List sandboxes in the environment
    #[clap(visible_alias = "ls")]
    List(ListArgs),

    /// Connect to a sandbox over SSH (defaults to the active sandbox)
    #[clap(visible_alias = "connect")]
    Ssh(SshArgs),

    /// Run a single command inside a sandbox (defaults to the active sandbox)
    Exec(ExecArgs),

    /// Forward local ports into a sandbox (defaults to the active sandbox)
    #[clap(visible_alias = "port-forward", visible_alias = "fwd")]
    Forward(ForwardArgs),

    /// Destroy a sandbox (defaults to the active sandbox)
    #[clap(visible_alias = "rm", visible_alias = "delete")]
    Destroy(DestroyArgs),
}

#[derive(Parser)]
struct CreateArgs {
    /// Minutes the sandbox may sit idle before it is auto-destroyed
    #[clap(long)]
    idle_timeout_minutes: Option<i64>,

    /// Set a variable on the sandbox (repeatable, comma-separable). Values may
    /// reference other variables — `DB_URL=postgres.DATABASE_URL` or the full
    /// `${{postgres.DATABASE_URL}}` form — resolved server-side at create time
    #[clap(long = "variable", value_name = "KEY=VALUE[,KEY=VALUE...]")]
    variables: Vec<String>,

    /// Load variables from a .env file (repeatable). `--variable` flags
    /// override file entries with the same key
    #[clap(long = "env-file", value_name = "PATH")]
    env_files: Vec<std::path::PathBuf>,

    /// Create from a built template, by local name or template id (see
    /// `railway sandbox template build`)
    #[clap(long, value_name = "NAME_OR_ID")]
    template: Option<String>,

    /// Create from a named checkpoint (see `railway sandbox checkpoint
    /// create`). Checkpoints are stored server-side, so this works from any
    /// machine
    #[clap(long, value_name = "NAME", conflicts_with = "template")]
    checkpoint: Option<String>,

    /// Join the environment's private network (default: isolated, public
    /// egress only). Needed to reach internal hosts like
    /// `postgres.railway.internal`
    #[clap(long)]
    private_network: bool,

    /// Output the created sandbox as JSON
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct TemplateArgs {
    #[clap(subcommand)]
    command: TemplateCommands,
}

#[derive(Parser)]
enum TemplateCommands {
    /// Build a template from shell instructions. Templates are
    /// content-addressed and cached server-side, so re-running the same
    /// build is an instant cache hit
    #[clap(visible_alias = "create", visible_alias = "new")]
    Build(TemplateBuildArgs),

    /// Show the build status of a template
    Status(TemplateStatusArgs),

    /// List templates this CLI has built
    #[clap(visible_alias = "ls")]
    List(TemplateListArgs),
}

#[derive(Parser)]
struct TemplateBuildArgs {
    /// Shell instruction to run while building (repeatable, runs in order;
    /// each step must exit 0 within 10 minutes)
    #[clap(
        short = 'c',
        long = "command",
        value_name = "SHELL_COMMAND",
        required = true
    )]
    commands: Vec<String>,

    /// Local name for the template, usable with `railway sandbox create
    /// --template <name>`
    #[clap(long)]
    name: Option<String>,

    /// Base image digest to build on (defaults to the standard sandbox image)
    #[clap(long, value_name = "DIGEST")]
    base_image_digest: Option<String>,

    /// Wait for the build to finish (polls until READY or FAILED)
    #[clap(long)]
    wait: bool,

    /// Output as JSON
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct CheckpointArgs {
    #[clap(subcommand)]
    command: CheckpointCommands,
}

#[derive(Parser)]
enum CheckpointCommands {
    /// Capture a sandbox's current disk into a named checkpoint. Synchronous:
    /// the checkpoint is bootable as soon as this returns. Reusing a name
    /// replaces the previous checkpoint
    #[clap(visible_alias = "capture", visible_alias = "save")]
    Create(CheckpointCreateArgs),

    /// List named checkpoints in the environment
    #[clap(visible_alias = "ls")]
    List(CheckpointListArgs),

    /// Rename a checkpoint
    Rename(CheckpointRenameArgs),

    /// Delete a checkpoint and its underlying disk snapshot
    #[clap(visible_alias = "rm")]
    Delete(CheckpointDeleteArgs),
}

#[derive(Parser)]
struct CheckpointCreateArgs {
    /// Name for the checkpoint, usable with `railway sandbox create
    /// --checkpoint <name>` (64-character hex names are reserved for
    /// template hashes)
    #[clap(value_name = "NAME")]
    name: String,

    /// Source sandbox ID (defaults to the active sandbox)
    #[clap(long = "id", value_name = "ID")]
    id: Option<String>,

    /// Output as JSON
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct CheckpointListArgs {
    /// Output as JSON
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct CheckpointRenameArgs {
    /// Current checkpoint name
    #[clap(value_name = "NAME")]
    name: String,

    /// New checkpoint name
    #[clap(value_name = "NEW_NAME")]
    new_name: String,

    /// Output as JSON
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct CheckpointDeleteArgs {
    /// Checkpoint name to delete
    #[clap(value_name = "NAME")]
    name: String,
}

#[derive(Parser)]
struct TemplateStatusArgs {
    /// Template id or local name
    #[clap(value_name = "ID_OR_NAME")]
    template: String,

    /// Output as JSON
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct TemplateListArgs {
    /// Output as JSON
    #[clap(long)]
    json: bool,
}

/// Fork has no trailing command, so a positional id is unambiguous; `--id` is
/// also accepted. Omitted → the active sandbox is the fork source.
#[derive(Parser)]
struct ForkArgs {
    /// Source sandbox ID to fork (defaults to the active sandbox)
    #[clap(value_name = "ID")]
    id_positional: Option<String>,

    /// Source sandbox ID (alternative to the positional argument)
    #[clap(long = "id", value_name = "ID")]
    id: Option<String>,

    /// Minutes the new sandbox may sit idle before it is auto-destroyed
    #[clap(long)]
    idle_timeout_minutes: Option<i64>,

    /// Set a variable on the fork (repeatable, comma-separable). The fork does
    /// not inherit the source's variables; values may reference other
    /// variables — `DB_URL=postgres.DATABASE_URL` or the full
    /// `${{postgres.DATABASE_URL}}` form — resolved server-side at fork time
    #[clap(long = "variable", value_name = "KEY=VALUE[,KEY=VALUE...]")]
    variables: Vec<String>,

    /// Load variables from a .env file (repeatable). `--variable` flags
    /// override file entries with the same key
    #[clap(long = "env-file", value_name = "PATH")]
    env_files: Vec<std::path::PathBuf>,

    /// Join the environment's private network (default: isolated, public
    /// egress only). The fork does not inherit the source's network mode
    #[clap(long)]
    private_network: bool,

    /// Output the created sandbox as JSON
    #[clap(long)]
    json: bool,
}

impl ForkArgs {
    fn explicit_id(&self) -> Option<String> {
        self.id.clone().or_else(|| self.id_positional.clone())
    }
}

#[derive(Parser)]
struct ListArgs {
    /// Output as JSON
    #[clap(long)]
    json: bool,

    /// Include destroyed sandboxes (hidden by default)
    #[clap(long)]
    all: bool,
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

    /// Resume a durable session by name (the relay announces the name on
    /// connect: "Railway durable session: <name>")
    #[clap(long, value_name = "NAME")]
    session: Option<String>,

    /// When resuming, continue from the last-read position instead of
    /// replaying the retained scrollback
    #[clap(long, requires = "session")]
    resume_from_last_read: bool,

    /// Command to run instead of an interactive shell
    #[clap(trailing_var_arg = true)]
    command: Vec<String>,
}

#[derive(Parser)]
struct ExecArgs {
    /// Sandbox ID to run in (defaults to the active sandbox)
    #[clap(long = "id", value_name = "ID")]
    id: Option<String>,

    /// Client-side deadline in seconds; on expiry the command is terminated
    /// and the CLI exits 124
    #[clap(long)]
    timeout: Option<i64>,

    /// Reattach to a durable session by name (a command is then optional)
    #[clap(long, value_name = "NAME", conflicts_with = "detach")]
    session: Option<String>,

    /// When reattaching, resume from the last-read position instead of
    /// replaying the retained output
    #[clap(long, requires = "session")]
    resume_from_last_read: bool,

    /// Start the command, print its durable session name to stdout, and exit
    /// while it keeps running (reattach with --session)
    #[clap(long)]
    detach: bool,

    /// Command to run (everything after `--`; optional with --session).
    /// A single argument runs as a shell command; multiple arguments run
    /// as argv with each argument quoted intact
    #[clap(trailing_var_arg = true)]
    command: Vec<String>,
}

/// `railway sandbox forward 3000 5432` / `railway sandbox forward 8080:3000`.
/// Ports are positional so the common case stays short; `--id` selects a
/// sandbox other than the active one.
#[derive(Parser)]
struct ForwardArgs {
    /// Ports to forward: `REMOTE` (same port locally) or `LOCAL:REMOTE`
    #[clap(value_name = "[LOCAL:]REMOTE", required = true)]
    ports: Vec<String>,

    /// Sandbox ID to forward into (defaults to the active sandbox)
    #[clap(long = "id", value_name = "ID")]
    id: Option<String>,

    /// Path to an identity (private key) file, like `ssh -i`
    #[clap(short = 'i', long = "identity-file", value_name = "PATH")]
    identity_file: Option<std::path::PathBuf>,

    /// Fail if a requested local port is busy instead of picking a nearby
    /// free one
    #[clap(long)]
    strict: bool,
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
    use colored::Colorize;
    eprintln!(
        "{}",
        "Warning: Railway sandboxes are experimental and APIs may change or break during testing."
            .yellow()
    );

    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let project = args.project;
    let environment = args.environment;

    match args.command {
        Commands::Create(sub) => create(&mut configs, &client, project, environment, sub).await,
        Commands::Fork(sub) => fork(&mut configs, &client, project, environment, sub).await,
        Commands::Template(sub) => template(&mut configs, &client, project, environment, sub).await,
        Commands::Checkpoint(sub) => {
            checkpoint(&mut configs, &client, project, environment, sub).await
        }
        Commands::List(sub) => list(&mut configs, &client, project, environment, sub).await,
        Commands::Ssh(sub) => ssh(&mut configs, &client, project, environment, sub).await,
        Commands::Exec(sub) => exec(&mut configs, &client, project, environment, sub).await,
        Commands::Forward(sub) => forward(&mut configs, &client, project, environment, sub).await,
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
    configs: &mut Configs,
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
/// `(project_id, environment_id)`. Only runs when the directory has no link,
/// so after a selection it offers to save it as the link (declining just
/// proceeds without one). Uses the OAuth-safe `UserProjects` listing (what
/// `railway list` uses) — the `projects(workspaceId:)` root field is not
/// authorized for plain user tokens.
async fn prompt_workspace_project_env(
    client: &reqwest::Client,
    configs: &mut Configs,
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
                Some(choice) => {
                    offer_to_link(configs, &project_obj.id, &project_obj.name, &choice)?;
                    return Ok((project_obj.id, choice.id));
                }
                None => continue 'project,
            }
        }
    }
}

/// Offer to remember a picker selection as the directory's linked
/// project/environment, so future commands (sandbox and otherwise) skip the
/// prompts. Esc or "no" proceeds without linking.
fn offer_to_link(
    configs: &mut Configs,
    project_id: &str,
    project_name: &str,
    environment: &Choice,
) -> Result<()> {
    let confirmed = prompt_confirm_with_default_with_cancel(
        &format!(
            "Link this directory to {project_name} ({})?",
            environment.name
        ),
        true,
    )?
    .unwrap_or(false);
    if !confirmed {
        return Ok(());
    }
    configs.link_project(
        project_id.to_string(),
        Some(project_name.to_string()),
        environment.id.clone(),
        Some(environment.name.clone()),
    )?;
    configs.write()?;
    eprintln!(
        "Linked to {project_name} ({}). Run `railway unlink` to undo.",
        environment.name
    );
    Ok(())
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
    configs: &mut Configs,
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

/// Parse repeatable `--variable` values into key/value pairs. Each argument is
/// a single `KEY=VALUE` or a comma-separated list of them (`A=1,B=2`). A comma
/// only splits when every segment carries its own `=` — `ALLOWED=a.com,b.com`
/// stays one variable whose value contains the comma. Repeating the flag is
/// the unambiguous form for values that mix commas and `=`.
fn parse_variable_args(args: &[String]) -> Result<Vec<Variable>> {
    let mut vars = Vec::new();
    for arg in args {
        let segments: Vec<&str> = arg.split(',').collect();
        if segments.len() > 1 && segments.iter().all(|s| s.contains('=')) {
            for segment in segments {
                vars.push(Variable::from_str(segment)?);
            }
        } else {
            vars.push(Variable::from_str(arg)?);
        }
    }
    Ok(vars)
}

/// Wrap a bare Railway reference (`name.VAR`) in `${{...}}` so users can write
/// `--variable DB_URL=postgres.DATABASE_URL` without shell-quoting the full
/// `${{postgres.DATABASE_URL}}` form. Only an exact `<name>.<VAR>` value is
/// wrapped — `name` alphanumeric/`_`/`-` starting with a letter, `VAR` in
/// UPPER_SNAKE starting with an uppercase letter — so plain values like `1.5`,
/// `example.com`, or `file.txt` pass through untouched, as does anything
/// already containing `${{`. The `shared.` namespace is unmistakable, so its
/// var segment may be any case (`shared.char`), not just UPPER_SNAKE.
fn auto_wrap_reference(value: &str) -> String {
    if value.contains("${{") {
        return value.to_string();
    }
    let Some((name, var)) = value.split_once('.') else {
        return value.to_string();
    };
    let name_ok = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let var_ok = if name == "shared" {
        var.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    } else {
        var.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && var
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    };
    if name_ok && var_ok {
        format!("${{{{{name}.{var}}}}}")
    } else {
        value.to_string()
    }
}

/// Parse a dotenv-style file into key/value pairs. Supports `KEY=VALUE` lines,
/// blank lines, `#` comments, an optional `export ` prefix, single/double
/// quoted values (kept verbatim inside the quotes), and trailing ` #` comments
/// on unquoted values. Multiline values are not supported.
fn parse_env_file(path: &std::path::Path) -> Result<Vec<Variable>> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("Failed to read env file {}: {e}", path.display()))?;
    let mut vars = Vec::new();
    for (i, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            bail!(
                "{}:{}: expected KEY=VALUE, got `{raw_line}`",
                path.display(),
                i + 1
            );
        };
        let key = key.trim();
        if key.is_empty() {
            bail!("{}:{}: empty variable name", path.display(), i + 1);
        }
        let value = value.trim();
        let value = if (value.starts_with('"') && value.ends_with('"') && value.len() >= 2)
            || (value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2)
        {
            &value[1..value.len() - 1]
        } else {
            // Unquoted: strip a trailing ` # comment`.
            value.split(" #").next().unwrap_or(value).trim_end()
        };
        vars.push(Variable {
            key: key.to_string(),
            value: value.to_string(),
        });
    }
    Ok(vars)
}

/// Convert `--env-file` and `--variable` args into the `EnvironmentVariables`
/// scalar, wrapping bare references. Files load first (in order), then flags —
/// so a `--variable` overrides a file entry with the same key. `None` when
/// empty so `skip_serializing_none` omits the field from the mutation input.
fn variables_to_input(
    env_files: &[std::path::PathBuf],
    args: &[String],
) -> Result<Option<BTreeMap<String, String>>> {
    let mut vars = Vec::new();
    for path in env_files {
        vars.extend(parse_env_file(path)?);
    }
    vars.extend(parse_variable_args(args)?);
    if vars.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        vars.into_iter()
            .map(|v| (v.key, auto_wrap_reference(&v.value)))
            .collect(),
    ))
}

/// Run `sandboxCreate` with the given input, persist the result as the active
/// sandbox (create and fork both retarget `ssh`/`exec` at the new sandbox),
/// and print create-style output.
async fn create_and_store(
    configs: &mut Configs,
    client: &reqwest::Client,
    project_id: String,
    environment_id: String,
    input: mutations::sandbox_create::SandboxCreateInput,
    json: bool,
    forked: bool,
) -> Result<()> {
    let (doing, did, failed) = if forked {
        ("Forking sandbox", "Forked", "Failed to fork sandbox")
    } else {
        ("Creating sandbox", "Created", "Failed to create sandbox")
    };

    let mut spinner = create_shimmer_spinner(doing);
    let sandbox = match post_graphql::<mutations::SandboxCreate, _>(
        client,
        configs.get_backboard(),
        mutations::sandbox_create::Variables { input },
    )
    .await
    {
        Ok(res) => res.sandbox_create,
        Err(e) => {
            fail_spinner(&mut spinner, failed.to_string());
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

    if json {
        println!("{}", serde_json::to_string_pretty(&sandbox)?);
    } else {
        println!("✓ {did} sandbox {} (now active)", sandbox.id);
        println!("  status: {:?}", sandbox.status);
        println!("  region: {}", sandbox.region);
        if let Some(idle) = sandbox.idle_timeout_minutes {
            println!("  idle timeout: {idle}m");
        }
        println!("\nConnect with:\n  railway sandbox ssh");
    }
    Ok(())
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

    // A checkpoint is referenced server-side by name alone; templates are
    // content-addressed, so sandboxCreate needs the full recipe (not just the
    // id), resolved from the local store. Both ride the same input — the
    // server treats `name` and `instructions` as mutually exclusive.
    let template = if let Some(name) = &args.checkpoint {
        Some(mutations::sandbox_create::SandboxTemplateInput {
            instructions: None,
            base_image_digest: None,
            name: Some(name.clone()),
            variables: None,
        })
    } else if let Some(handle) = &args.template {
        let stored = configs
            .find_sandbox_template(handle, Some(&environment_id))
            .ok_or_else(|| {
                anyhow!(
                    "Unknown template `{handle}` for this environment. Build it first:\n  railway sandbox template build --name {handle} -c '<command>' --wait"
                )
            })?;
        Some(mutations::sandbox_create::SandboxTemplateInput {
            instructions: Some(stored.instructions),
            base_image_digest: stored.base_image_digest,
            name: None,
            variables: None,
        })
    } else {
        None
    };

    let input = mutations::sandbox_create::SandboxCreateInput {
        environment_id: environment_id.clone(),
        idle_timeout_minutes: args.idle_timeout_minutes,
        template,
        source_sandbox_id: None,
        network_isolation: args
            .private_network
            .then_some(mutations::sandbox_create::SandboxNetworkIsolation::PRIVATE),
        variables: variables_to_input(&args.env_files, &args.variables)?,
    };
    create_and_store(
        configs,
        client,
        project_id,
        environment_id,
        input,
        args.json,
        false,
    )
    .await
}

async fn template(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: TemplateArgs,
) -> Result<()> {
    match args.command {
        TemplateCommands::Build(sub) => {
            template_build(configs, client, project, environment, sub).await
        }
        TemplateCommands::Status(sub) => {
            template_status(configs, client, project, environment, sub).await
        }
        TemplateCommands::List(sub) => {
            template_list(configs, client, project, environment, sub).await
        }
    }
}

async fn template_build(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: TemplateBuildArgs,
) -> Result<()> {
    let (_, environment_id) =
        resolve_project_and_env(configs, client, project, environment).await?;

    let res = post_graphql::<mutations::SandboxTemplateBuild, _>(
        client,
        configs.get_backboard(),
        mutations::sandbox_template_build::Variables {
            environment_id: environment_id.clone(),
            input: mutations::sandbox_template_build::SandboxTemplateInput {
                instructions: Some(args.commands.clone()),
                base_image_digest: args.base_image_digest.clone(),
                name: None,
                variables: None,
            },
        },
    )
    .await?;
    let built = res.sandbox_template_build;

    // Keep the recipe locally: `sandbox create --template` must resend the
    // instructions, since the server only caches by hash.
    configs.upsert_sandbox_template(StoredSandboxTemplate {
        id: built.id.clone(),
        name: args.name.clone(),
        environment_id: environment_id.clone(),
        instructions: args.commands,
        base_image_digest: args.base_image_digest,
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    });
    configs.write()?;

    let already_ready = matches!(
        built.status,
        mutations::sandbox_template_build::SandboxTemplateBuildStatus::READY
    );
    let status = if args.wait && !already_ready {
        wait_for_template(client, configs, &environment_id, &built.id).await?
    } else {
        format!("{:?}", built.status)
    };

    let handle = args.name.unwrap_or_else(|| built.id.clone());
    if args.json {
        let out = serde_json::json!({
            "id": built.id,
            "status": status,
            "environmentId": environment_id,
            "name": handle,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if already_ready {
        println!("✓ Template {handle} ready (cached)");
    } else if status == "READY" {
        println!("✓ Template {handle} built");
    } else {
        println!("Template {handle} status: {status}");
        println!("\nCheck progress with:\n  railway sandbox template status {handle}");
    }
    if status == "READY" {
        println!("\nCreate a sandbox from it with:\n  railway sandbox create --template {handle}");
    }
    Ok(())
}

async fn template_status(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: TemplateStatusArgs,
) -> Result<()> {
    // A locally stored template knows its environment; a raw id falls back to
    // flags / the linked environment.
    let stored = configs.find_sandbox_template(&args.template, None);
    let (id, environment_id) = match &stored {
        Some(t) => (t.id.clone(), t.environment_id.clone()),
        None => {
            let (_, environment_id) =
                resolve_project_and_env(configs, client, project, environment).await?;
            (args.template.clone(), environment_id)
        }
    };

    let res = post_graphql::<queries::SandboxTemplateBuild, _>(
        client,
        configs.get_backboard(),
        queries::sandbox_template_build::Variables { environment_id, id },
    )
    .await?;
    let tpl = res.sandbox_template_build;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&tpl)?);
        return Ok(());
    }
    if let Some(name) = stored.and_then(|t| t.name) {
        println!("Template {name} ({})", tpl.id);
    } else {
        println!("Template {}", tpl.id);
    }
    println!("  status: {:?}", tpl.status);
    Ok(())
}

async fn template_list(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: TemplateListArgs,
) -> Result<()> {
    let (_, environment_id) =
        resolve_project_and_env(configs, client, project, environment).await?;
    let templates = configs.list_sandbox_templates(Some(&environment_id));

    if templates.is_empty() {
        if args.json {
            println!("[]");
        } else {
            println!(
                "No templates built from this CLI for this environment.\nBuild one with:\n  railway sandbox template build --name <name> -c '<command>' --wait"
            );
        }
        return Ok(());
    }

    let mut rows = Vec::new();
    for t in &templates {
        let status = post_graphql::<queries::SandboxTemplateBuild, _>(
            client,
            configs.get_backboard(),
            queries::sandbox_template_build::Variables {
                environment_id: environment_id.clone(),
                id: t.id.clone(),
            },
        )
        .await
        .map(|r| format!("{:?}", r.sandbox_template_build.status))
        .unwrap_or_else(|_| "UNKNOWN".to_string());
        rows.push((t, status));
    }

    if args.json {
        let out: Vec<_> = rows
            .iter()
            .map(|(t, status)| {
                serde_json::json!({
                    "id": t.id,
                    "name": t.name,
                    "status": status,
                    "instructions": t.instructions,
                    "baseImageDigest": t.base_image_digest,
                    "createdAt": t.created_at,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!(
        "{:<20}  {:<16}  {:<10}  {:<6}",
        "NAME", "ID", "STATUS", "STEPS"
    );
    for (t, status) in rows {
        println!(
            "{:<20}  {:<16}  {:<10}  {:<6}",
            t.name.as_deref().unwrap_or("-"),
            &t.id[..t.id.len().min(16)],
            status,
            t.instructions.len()
        );
    }
    Ok(())
}

/// Poll the template status until READY (or fail on FAILED/timeout). Build
/// steps run server-side in a transient sandbox; the workflow caps out at 40m,
/// so poll a little past that.
async fn wait_for_template(
    client: &reqwest::Client,
    configs: &Configs,
    environment_id: &str,
    id: &str,
) -> Result<String> {
    let mut spinner = create_shimmer_spinner("Building template");
    let deadline = std::time::Instant::now() + Duration::from_secs(45 * 60);
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let res = post_graphql::<queries::SandboxTemplateBuild, _>(
            client,
            configs.get_backboard(),
            queries::sandbox_template_build::Variables {
                environment_id: environment_id.to_string(),
                id: id.to_string(),
            },
        )
        .await?;
        match res.sandbox_template_build.status {
            queries::sandbox_template_build::SandboxTemplateBuildStatus::READY => {
                spinner.finish_and_clear();
                return Ok("READY".to_string());
            }
            queries::sandbox_template_build::SandboxTemplateBuildStatus::FAILED => {
                fail_spinner(&mut spinner, "Template build failed".to_string());
                bail!(
                    "Template build failed. Each instruction must exit 0 within 10 minutes; fix the failing step and rebuild."
                );
            }
            _ => {}
        }
        if std::time::Instant::now() > deadline {
            fail_spinner(&mut spinner, "Timed out waiting for template".to_string());
            bail!("Timed out waiting for the template build.");
        }
    }
}

async fn checkpoint(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: CheckpointArgs,
) -> Result<()> {
    match args.command {
        CheckpointCommands::Create(sub) => {
            checkpoint_create(configs, client, project, environment, sub).await
        }
        CheckpointCommands::List(sub) => {
            checkpoint_list(configs, client, project, environment, sub).await
        }
        CheckpointCommands::Rename(sub) => {
            checkpoint_rename(configs, client, project, environment, sub).await
        }
        CheckpointCommands::Delete(sub) => {
            checkpoint_delete(configs, client, project, environment, sub).await
        }
    }
}

/// Capture is synchronous server-side: the live rootfs is flushed and its
/// dirty blocks uploaded before the mutation returns, so a large disk can
/// take a while (the request honors `RAILWAY_HTTP_TIMEOUT`).
async fn checkpoint_create(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: CheckpointCreateArgs,
) -> Result<()> {
    let (sandbox_id, environment_id) =
        resolve_target(configs, client, args.id.clone(), project, environment).await?;

    let mut spinner = create_shimmer_spinner("Capturing checkpoint");
    let created = match post_graphql::<mutations::SandboxCheckpointCreate, _>(
        client,
        configs.get_backboard(),
        mutations::sandbox_checkpoint_create::Variables {
            environment_id,
            sandbox_id: sandbox_id.clone(),
            name: args.name.clone(),
        },
    )
    .await
    {
        Ok(res) => res.sandbox_checkpoint_create,
        Err(e) => {
            fail_spinner(&mut spinner, "Failed to capture checkpoint".to_string());
            return Err(e.into());
        }
    };
    spinner.finish_and_clear();

    if args.json {
        println!("{}", serde_json::to_string_pretty(&created)?);
        return Ok(());
    }
    println!("✓ Checkpoint {} captured from {sandbox_id}", created.key);
    println!(
        "\nBoot a sandbox from it with:\n  railway sandbox create --checkpoint {}",
        created.key
    );
    Ok(())
}

async fn checkpoint_list(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: CheckpointListArgs,
) -> Result<()> {
    let (_, environment_id) =
        resolve_project_and_env(configs, client, project, environment).await?;

    let res = post_graphql::<queries::SandboxCheckpoints, _>(
        client,
        configs.get_backboard(),
        queries::sandbox_checkpoints::Variables { environment_id },
    )
    .await?;
    let checkpoints = res.sandbox_checkpoints;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&checkpoints)?);
        return Ok(());
    }
    if checkpoints.is_empty() {
        println!(
            "No checkpoints in this environment.\nCapture one with:\n  railway sandbox checkpoint create <name>"
        );
        return Ok(());
    }

    println!("{:<32}  {:<16}", "NAME", "CREATED");
    for cp in checkpoints {
        println!(
            "{:<32}  {:<16}",
            cp.key,
            cp.created_at.format("%Y-%m-%d %H:%M").to_string()
        );
    }
    Ok(())
}

async fn checkpoint_rename(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: CheckpointRenameArgs,
) -> Result<()> {
    let (_, environment_id) =
        resolve_project_and_env(configs, client, project, environment).await?;

    let res = post_graphql::<mutations::SandboxCheckpointRename, _>(
        client,
        configs.get_backboard(),
        mutations::sandbox_checkpoint_rename::Variables {
            environment_id,
            id: args.name.clone(),
            name: args.new_name.clone(),
        },
    )
    .await?;
    let renamed = res.sandbox_checkpoint_rename;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&renamed)?);
        return Ok(());
    }
    println!("✓ Renamed checkpoint {} → {}", args.name, renamed.key);
    Ok(())
}

async fn checkpoint_delete(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: CheckpointDeleteArgs,
) -> Result<()> {
    let (_, environment_id) =
        resolve_project_and_env(configs, client, project, environment).await?;

    let res = post_graphql::<mutations::SandboxCheckpointDelete, _>(
        client,
        configs.get_backboard(),
        mutations::sandbox_checkpoint_delete::Variables {
            environment_id,
            id: args.name.clone(),
        },
    )
    .await?;

    // The server reports false (rather than erroring) when no such row exists.
    if !res.sandbox_checkpoint_delete {
        bail!("No checkpoint named `{}` in this environment.", args.name);
    }
    println!("✓ Deleted checkpoint {}", args.name);
    Ok(())
}

async fn fork(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: ForkArgs,
) -> Result<()> {
    let (source_sandbox_id, environment_id) = resolve_target(
        configs,
        client,
        args.explicit_id(),
        project.clone(),
        environment.clone(),
    )
    .await?;

    // For the stored ref: prefer the source's cached project_id, else resolve
    // from flags / the linked project.
    let project_id = match configs
        .get_sandbox(&source_sandbox_id)
        .and_then(|s| s.project_id)
    {
        Some(id) => id,
        None => {
            resolve_project_and_env(configs, client, project, environment)
                .await?
                .0
        }
    };

    let input = mutations::sandbox_create::SandboxCreateInput {
        environment_id: environment_id.clone(),
        idle_timeout_minutes: args.idle_timeout_minutes,
        template: None,
        source_sandbox_id: Some(source_sandbox_id),
        network_isolation: args
            .private_network
            .then_some(mutations::sandbox_create::SandboxNetworkIsolation::PRIVATE),
        variables: variables_to_input(&args.env_files, &args.variables)?,
    };
    create_and_store(
        configs,
        client,
        project_id,
        environment_id,
        input,
        args.json,
        true,
    )
    .await
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
    let mut nodes: Vec<_> = res.sandboxes.edges.into_iter().map(|e| e.node).collect();

    // Tombstones quickly outnumber live sandboxes; hide them unless --all.
    let hidden = if args.all {
        0
    } else {
        let before = nodes.len();
        nodes.retain(|n| !matches!(n.status, queries::sandboxes::SandboxStatus::DESTROYED));
        before - nodes.len()
    };

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
        if hidden > 0 {
            println!(
                "No active sandboxes in this environment ({hidden} destroyed; use --all to show them)."
            );
        } else {
            println!("No sandboxes in this environment.");
        }
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
    if hidden > 0 {
        println!("\n({hidden} destroyed sandboxes hidden; use --all to show them)");
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
    use colored::Colorize;

    // clap can't express "required unless --session" for a trailing vararg.
    if args.command.is_empty() && args.session.is_none() {
        bail!("a command is required (or pass --session <name> to reattach)");
    }

    let (sandbox_id, environment_id) = tel::track_for(
        "sandbox",
        "exec_resolve_target",
        resolve_target(configs, client, args.id.clone(), project, environment).await,
    )
    .await?;

    configs.set_active_sandbox(&sandbox_id);
    configs.write()?;

    let mut spinner = create_shimmer_spinner("Connecting");

    // The bridge is authorized by a short-lived shell-scoped JWT carried in
    // the WebSocket subprotocol.
    let jwt = match tel::track_for(
        "sandbox",
        "exec_mint_token",
        mint_shell_token(
            client,
            configs.get_backboard(),
            &environment_id,
            &sandbox_id,
        )
        .await,
    )
    .await
    {
        Ok(jwt) => jwt,
        Err(e) => {
            fail_spinner(&mut spinner, "Failed to authorize command".to_string());
            return Err(e);
        }
    };

    let ws =
        match tel::track_for("sandbox", "exec_connect", sandbox_exec::connect(&jwt).await).await {
            Ok(ws) => ws,
            Err(e) => {
                fail_spinner(&mut spinner, "Failed to connect to sandbox".to_string());
                return Err(e);
            }
        };
    spinner.finish_and_clear();

    // Keep the sandbox alive against the idle reaper while the command runs.
    let heartbeat = spawn_heartbeat(
        client.clone(),
        configs.get_backboard(),
        environment_id,
        sandbox_id.clone(),
    );

    let options = sandbox_exec::ExecOptions {
        // A single argument passes through as a full shell command so pipes
        // and redirects work (`exec -- "ls | head"`). Multiple arguments are
        // argv: quote each so the remote shell sees the same boundaries —
        // `exec -- bash -lc 'echo a | b'` must not re-split on the pipe.
        command: match args.command.as_slice() {
            [] => None,
            [cmd] => Some(cmd.clone()),
            argv => Some(shell_join(argv)),
        },
        session: args.session.clone(),
        resume_from_last_read: args.resume_from_last_read,
        timeout: args
            .timeout
            .map(|secs| Duration::from_secs(secs.max(0) as u64)),
        detach: args.detach,
        stdin_is_tty: std::io::stdin().is_terminal(),
    };

    let outcome = tel::track_for(
        "sandbox",
        "exec_stream",
        sandbox_exec::run(ws, options).await,
    )
    .await;
    heartbeat.abort();

    match outcome? {
        ExecOutcome::Exited {
            code,
            fresh_session_suspected,
        } => {
            if fresh_session_suspected {
                eprintln!(
                    "{}",
                    "warning: that session may have expired; the server started a fresh one instead"
                        .yellow()
                );
            }
            if args.detach {
                eprintln!(
                    "{}",
                    "warning: durable sessions are unavailable for this sandbox; ran attached"
                        .yellow()
                );
            }
            if code != 0 {
                tel::report_failure_for("sandbox", "exec_exit_nonzero", &format!("exit {code}"))
                    .await;
            }
            std::process::exit(code);
        }
        ExecOutcome::TimedOut { session_name } => {
            eprintln!("\n(command timed out)");
            if let Some(name) = session_name {
                eprintln!("{}", reattach_hint(&sandbox_id, &name).dimmed());
            }
            std::process::exit(sandbox_exec::TIMEOUT_EXIT_CODE);
        }
        ExecOutcome::Detached { session_name } => {
            // stdout carries just the session name so scripts can capture it.
            println!("{session_name}");
            eprintln!("{}", reattach_hint(&sandbox_id, &session_name).dimmed());
            Ok(())
        }
        ExecOutcome::Disconnected { session_name } => {
            match session_name {
                Some(name) => {
                    eprintln!("\nDisconnected; the command may still be running.");
                    eprintln!("{}", reattach_hint(&sandbox_id, &name).dimmed());
                }
                None => eprintln!("\nConnection lost."),
            }
            std::process::exit(1);
        }
    }
}

fn reattach_hint(sandbox_id: &str, session_name: &str) -> String {
    format!("Reattach with: railway sandbox exec --id {sandbox_id} --session {session_name}")
}

async fn mint_shell_token(
    client: &reqwest::Client,
    backboard: String,
    environment_id: &str,
    sandbox_id: &str,
) -> Result<String> {
    let res = post_graphql::<mutations::GenerateShellToken, _>(
        client,
        backboard,
        mutations::generate_shell_token::Variables {
            input: mutations::generate_shell_token::ShellTokenInput {
                environment_id: environment_id.to_string(),
                instance_id: sandbox_id.to_string(),
                kind: Some("sandbox".to_string()),
                port: None,
                scope: "shell".to_string(),
                service_id: None,
            },
        },
    )
    .await?;
    Ok(res.generate_shell_token)
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

/// A forward session that survives this long was a healthy tunnel; its drop
/// resets the quick-failure budget instead of consuming it.
const FORWARD_STABLE_SESSION: Duration = Duration::from_secs(30);
/// Base delay between reconnect attempts (doubles per consecutive quick
/// failure, capped at 2^4).
const FORWARD_RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// Consecutive quick failures tolerated before the forward gives up.
const FORWARD_MAX_QUICK_FAILURES: u32 = 5;

async fn ssh(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: SshArgs,
) -> Result<()> {
    // Stage-tagged failure telemetry, mirroring `railway ssh` (tel.rs) so
    // sandbox SSH sessions land in the same stage-failure dashboards under
    // command = "sandbox".
    let (sandbox_id, environment_id) = tel::track_for(
        "sandbox",
        "ssh_resolve_target",
        resolve_target(configs, client, args.id.clone(), project, environment).await,
    )
    .await?;

    // Reuse the native-SSH key registration flow from `railway ssh`. When the
    // user didn't pass `-i`, use the registered key it resolves so a
    // non-default-named key (e.g. ~/.ssh/raildesk_railway_ed25519) is actually
    // offered to the relay instead of just ssh's default identities.
    let auto_identity = if args.identity_file.is_none() {
        tel::track_for(
            "sandbox",
            "ssh_key_setup",
            ensure_ssh_key(client, configs).await,
        )
        .await?
    } else {
        None
    };

    configs.set_active_sandbox(&sandbox_id);
    configs.write()?;

    // Relay target format (per backboard): sbx:<environmentId>:<sandboxId>.
    // `run_native_ssh` appends the environment's relay host internally.
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
    let durable_session = args.session.clone();
    let resume_from_last_read = args.resume_from_last_read;

    // `run_native_ssh` is blocking (inherits the terminal); run it off the
    // async runtime so the heartbeat task keeps ticking.
    let session = tokio::task::spawn_blocking(move || {
        let durable = durable_session.as_deref().map(|name| DurableResume {
            session_name: name,
            resume_from_last_read,
        });
        run_native_ssh(&target, command.as_deref(), identity.as_deref(), durable)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    let exit_code = tel::track_for("sandbox", "ssh_session", session).await?;

    heartbeat.abort();

    if exit_code != 0 {
        tel::report_failure_for(
            "sandbox",
            "ssh_exit_nonzero",
            &format!("ssh exited with code {exit_code}"),
        )
        .await;
        std::process::exit(exit_code);
    }
    Ok(())
}

/// A parsed `[LOCAL:]REMOTE` port spec. `local` is None when the user gave a
/// bare remote port (local defaults to the same port, with busy-port
/// fallback); an explicit `LOCAL:REMOTE` never gets remapped.
struct PortSpec {
    local: Option<u16>,
    remote: u16,
}

fn parse_port_spec(spec: &str) -> Result<PortSpec> {
    let parse_port = |s: &str, what: &str| -> Result<u16> {
        let port: u16 = s
            .trim()
            .parse()
            .map_err(|_| anyhow!("Invalid {what} port {s:?} in {spec:?} (expected 1-65535)"))?;
        if port == 0 {
            bail!("Invalid {what} port 0 in {spec:?} (expected 1-65535)");
        }
        Ok(port)
    };

    match spec.split_once(':') {
        Some((local, remote)) => Ok(PortSpec {
            local: Some(parse_port(local, "local")?),
            remote: parse_port(remote, "remote")?,
        }),
        None => Ok(PortSpec {
            local: None,
            remote: parse_port(spec, "remote")?,
        }),
    }
}

/// Pick the local port for a spec. Bare `REMOTE` specs fall back to a nearby
/// free port when the obvious one is busy (dev-server style) unless `strict`;
/// explicit `LOCAL:REMOTE` specs always fail busy. Returns `(port, remapped)`.
///
/// Bind-test then release — a small TOCTOU window before ssh re-binds, which
/// is fine for the interactive dev workflow this serves.
fn resolve_local_port(spec: &PortSpec, strict: bool) -> Result<(u16, bool)> {
    let is_free = |port: u16| std::net::TcpListener::bind(("127.0.0.1", port)).is_ok();

    let requested = spec.local.unwrap_or(spec.remote);
    if is_free(requested) {
        return Ok((requested, false));
    }

    if spec.local.is_some() || strict {
        bail!(
            "Local port {requested} is already in use.\n\
            Pick a different one with: railway sandbox forward <local>:{remote}",
            remote = spec.remote
        );
    }

    // Scan upward like dev servers do; checked_add stops the scan at 65535.
    for offset in 1..=100u16 {
        if let Some(candidate) = requested.checked_add(offset)
            && is_free(candidate)
        {
            return Ok((candidate, true));
        }
    }
    bail!("Local port {requested} is in use and no nearby free port was found");
}

async fn forward(
    configs: &mut Configs,
    client: &reqwest::Client,
    project: Option<String>,
    environment: Option<String>,
    args: ForwardArgs,
) -> Result<()> {
    use colored::Colorize;

    // Parse and de-dup before any network work so bad specs fail instantly.
    let specs = args
        .ports
        .iter()
        .map(|s| parse_port_spec(s))
        .collect::<Result<Vec<_>>>()?;

    let (sandbox_id, environment_id) = tel::track_for(
        "sandbox",
        "forward_resolve_target",
        resolve_target(configs, client, args.id.clone(), project, environment).await,
    )
    .await?;

    // Same key flow as `sandbox ssh`: resolve (or register) the key so a
    // non-default-named identity is actually offered to the relay.
    let auto_identity = if args.identity_file.is_none() {
        tel::track_for(
            "sandbox",
            "forward_key_setup",
            ensure_ssh_key(client, configs).await,
        )
        .await?
    } else {
        None
    };

    let mut forwards = Vec::with_capacity(specs.len());
    let mut remaps = Vec::new();
    let mut seen_local = std::collections::BTreeSet::new();
    for spec in &specs {
        let (local_port, remapped) = resolve_local_port(spec, args.strict)?;
        if !seen_local.insert(local_port) {
            bail!("Local port {local_port} is requested more than once");
        }
        if remapped {
            remaps.push((spec.remote, local_port));
        }
        forwards.push(PortForward {
            local_port,
            remote_port: spec.remote,
        });
    }

    configs.set_active_sandbox(&sandbox_id);
    configs.write()?;

    let target = format!("sbx:{environment_id}:{sandbox_id}");

    // Keep the sandbox alive while the forward is up; the relay extends once
    // on connect but an idle forward would otherwise hit the idle reaper.
    let heartbeat = spawn_heartbeat(
        client.clone(),
        configs.get_backboard(),
        environment_id.clone(),
        sandbox_id.clone(),
    );

    let short_id: String = sandbox_id.chars().take(8).collect();
    eprintln!();
    eprintln!(
        "{} Forwarding to sandbox {}",
        "⚡".yellow(),
        short_id.bold()
    );
    eprintln!();
    for (remapped_remote, picked) in &remaps {
        eprintln!(
            "  {} port {remapped_remote} is in use locally, using {picked} instead",
            "⚠".yellow()
        );
    }
    for f in &forwards {
        eprintln!(
            "  {}  {} {} {}",
            "➜".green(),
            format!("http://localhost:{}", f.local_port).cyan().bold(),
            "→".dimmed(),
            format!("sandbox:{}", f.remote_port).dimmed()
        );
    }
    eprintln!();
    eprintln!("  {}", "Press Ctrl+C to stop".dimmed());
    eprintln!();

    let identity = args.identity_file.clone().or(auto_identity);

    // Reconnect loop: the relay can drop a long-lived tunnel while the
    // sandbox itself is still healthy. On an unexpected ssh exit, re-check
    // the sandbox and reconnect if it is RUNNING; only tell the user to
    // create a fresh sandbox when it actually stopped. Consecutive *fast*
    // failures are bounded so a broken relay/auth setup can't hot-loop; a
    // session that held for a while resets the budget.
    let mut quick_failures: u32 = 0;
    let exit_code = loop {
        let session_target = target.clone();
        let session_identity = identity.clone();
        let session_forwards = forwards.clone();
        let started = std::time::Instant::now();
        // Blocking ssh runs off the async runtime so the heartbeat keeps ticking.
        let session = tokio::task::spawn_blocking(move || {
            run_native_ssh_forward(
                &session_target,
                session_identity.as_deref(),
                &session_forwards,
            )
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
        let exit_code = tel::track_for("sandbox", "forward_session", session).await?;

        // Clean exit is the user's Ctrl+C (ssh dies by signal in our
        // process group); anything else is an unexpected drop.
        if exit_code == 0 {
            break 0;
        }

        if started.elapsed() >= FORWARD_STABLE_SESSION {
            quick_failures = 0;
        } else {
            quick_failures += 1;
        }
        if quick_failures > FORWARD_MAX_QUICK_FAILURES {
            eprintln!(
                "\nForward keeps failing right after connecting (ssh exit code {exit_code}); giving up."
            );
            break exit_code;
        }

        match fetch_sandbox_status(client, configs, &environment_id, &sandbox_id).await {
            Ok(Some(queries::sandbox::SandboxStatus::RUNNING)) => {
                let delay = FORWARD_RECONNECT_DELAY * 2u32.saturating_pow(quick_failures.min(4));
                eprintln!(
                    "\n{} Forward dropped (ssh exit code {exit_code}); sandbox {} is still running — reconnecting in {}s...",
                    "⚠".yellow(),
                    short_id.bold(),
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
            }
            Ok(status) => {
                let status = status
                    .map(|s| format!("{s:?}"))
                    .unwrap_or_else(|| "GONE".to_string());
                eprintln!(
                    "\nForward ended: sandbox {short_id} is {status}.\n\
                    Start a fresh one with `railway sandbox create` or `railway sandbox fork`."
                );
                break exit_code;
            }
            // Can't verify the sandbox either — the old generic message is
            // the honest one here.
            Err(_) => {
                eprintln!(
                    "\nForward ended unexpectedly (ssh exit code {exit_code}).\n\
                    If the sandbox stopped, start a fresh one with `railway sandbox create` or `railway sandbox fork`."
                );
                break exit_code;
            }
        }
    };

    heartbeat.abort();

    if exit_code != 0 {
        tel::report_failure_for(
            "sandbox",
            "forward_exit_nonzero",
            &format!("ssh exited with code {exit_code}"),
        )
        .await;
        std::process::exit(exit_code);
    }
    Ok(())
}

/// Best-effort status read used to decide whether a dropped forward should
/// reconnect. `Ok(None)` means the API answered but the sandbox is gone.
async fn fetch_sandbox_status(
    client: &reqwest::Client,
    configs: &Configs,
    environment_id: &str,
    sandbox_id: &str,
) -> Result<Option<queries::sandbox::SandboxStatus>> {
    let res = post_graphql::<queries::Sandbox, _>(
        client,
        configs.get_backboard(),
        queries::sandbox::Variables {
            environment_id: environment_id.to_string(),
            id: sandbox_id.to_string(),
        },
    )
    .await?;
    Ok(res.sandbox.map(|s| s.status))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_port_spec_bare_remote() {
        let spec = parse_port_spec("3000").unwrap();
        assert!(spec.local.is_none());
        assert_eq!(spec.remote, 3000);
    }

    #[test]
    fn parse_port_spec_local_remote() {
        let spec = parse_port_spec("8080:3000").unwrap();
        assert_eq!(spec.local, Some(8080));
        assert_eq!(spec.remote, 3000);
    }

    #[test]
    fn parse_port_spec_rejects_garbage() {
        assert!(parse_port_spec("abc").is_err());
        assert!(parse_port_spec("0").is_err());
        assert!(parse_port_spec("8080:0").is_err());
        assert!(parse_port_spec(":3000").is_err());
        assert!(parse_port_spec("70000").is_err());
        assert!(parse_port_spec("8080:3000:1").is_err());
    }

    #[test]
    fn parse_single_pair() {
        let vars = parse_variable_args(&args(&["FOO=bar"])).unwrap();
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].key, "FOO");
        assert_eq!(vars[0].value, "bar");
    }

    #[test]
    fn parse_comma_separated_pairs() {
        let vars = parse_variable_args(&args(&["FOO=bar,BAZ=qux,N=1"])).unwrap();
        assert_eq!(
            vars.iter()
                .map(|v| (v.key.as_str(), v.value.as_str()))
                .collect::<Vec<_>>(),
            vec![("FOO", "bar"), ("BAZ", "qux"), ("N", "1")]
        );
    }

    #[test]
    fn comma_in_value_stays_single_pair() {
        // "b.com" has no '=', so the comma is part of the value, not a separator.
        let vars = parse_variable_args(&args(&["ALLOWED=a.com,b.com"])).unwrap();
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].key, "ALLOWED");
        assert_eq!(vars[0].value, "a.com,b.com");
    }

    #[test]
    fn repeated_flags_accumulate() {
        let vars = parse_variable_args(&args(&["A=1", "B=2,C=3"])).unwrap();
        assert_eq!(vars.len(), 3);
    }

    #[test]
    fn invalid_pair_errors() {
        assert!(parse_variable_args(&args(&["NOVALUE"])).is_err());
        assert!(parse_variable_args(&args(&["FOO=bar,NOVALUE=,BAZ=qux"])).is_err());
    }

    #[test]
    fn wraps_bare_references() {
        assert_eq!(
            auto_wrap_reference("postgres.DATABASE_URL"),
            "${{postgres.DATABASE_URL}}"
        );
        assert_eq!(auto_wrap_reference("shared.FOO"), "${{shared.FOO}}");
        assert_eq!(
            auto_wrap_reference("my-api_2.PORT_8080"),
            "${{my-api_2.PORT_8080}}"
        );
    }

    #[test]
    fn leaves_plain_values_alone() {
        for v in ["bar", "1.5", "example.com", "file.txt", "a.b.C", "2.0.1"] {
            assert_eq!(auto_wrap_reference(v), v);
        }
    }

    #[test]
    fn leaves_existing_references_alone() {
        let full = "${{postgres.DATABASE_URL}}";
        assert_eq!(auto_wrap_reference(full), full);
        let embedded = "postgres://${{postgres.PGUSER}}@host";
        assert_eq!(auto_wrap_reference(embedded), embedded);
    }

    #[test]
    fn variables_to_input_wraps_and_collects() {
        let input = variables_to_input(&[], &args(&["DB=postgres.DATABASE_URL,FOO=bar"]))
            .unwrap()
            .unwrap();
        assert_eq!(
            input.get("DB").map(String::as_str),
            Some("${{postgres.DATABASE_URL}}")
        );
        assert_eq!(input.get("FOO").map(String::as_str), Some("bar"));
    }

    #[test]
    fn variables_to_input_empty_is_none() {
        assert!(variables_to_input(&[], &[]).unwrap().is_none());
    }

    fn parse_exec(argv: &[&str]) -> std::result::Result<Args, clap::Error> {
        let full: Vec<&str> = std::iter::once("sandbox")
            .chain(argv.iter().copied())
            .collect();
        <Args as clap::Parser>::try_parse_from(full)
    }

    #[test]
    fn exec_session_without_command_parses() {
        let args = parse_exec(&["exec", "--session", "sess-1"]).unwrap();
        let Commands::Exec(exec) = args.command else {
            panic!("expected exec subcommand");
        };
        assert_eq!(exec.session.as_deref(), Some("sess-1"));
        assert!(exec.command.is_empty());
    }

    #[test]
    fn exec_session_conflicts_with_detach() {
        assert!(parse_exec(&["exec", "--session", "s", "--detach", "--", "ls"]).is_err());
    }

    #[test]
    fn exec_resume_requires_session() {
        assert!(parse_exec(&["exec", "--resume-from-last-read", "--", "ls"]).is_err());
        assert!(parse_exec(&["exec", "--session", "s", "--resume-from-last-read"]).is_ok());
    }

    #[test]
    fn exec_detach_with_command_parses() {
        let args = parse_exec(&["exec", "--detach", "--", "sleep", "300"]).unwrap();
        let Commands::Exec(exec) = args.command else {
            panic!("expected exec subcommand");
        };
        assert!(exec.detach);
        assert_eq!(exec.command, vec!["sleep", "300"]);
    }

    #[test]
    fn manually_wrapped_pairs_split_and_pass_verbatim() {
        // Users may pre-wrap references themselves; comma-splitting still
        // applies and the wrapped values are sent untouched.
        let input = variables_to_input(
            &[],
            &args(&["FOO=${{serviceName.FOO}},BAR=${{serviceName.BAR}}"]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            input.get("FOO").map(String::as_str),
            Some("${{serviceName.FOO}}")
        );
        assert_eq!(
            input.get("BAR").map(String::as_str),
            Some("${{serviceName.BAR}}")
        );
        // Embedded references inside larger values also pass through.
        let input = variables_to_input(&[], &args(&["URL=http://${{svc.HOST}}:8080"]))
            .unwrap()
            .unwrap();
        assert_eq!(
            input.get("URL").map(String::as_str),
            Some("http://${{svc.HOST}}:8080")
        );
    }

    #[test]
    fn wraps_shared_refs_any_case() {
        assert_eq!(auto_wrap_reference("shared.char"), "${{shared.char}}");
        assert_eq!(auto_wrap_reference("shared.FOO"), "${{shared.FOO}}");
        // Other namespaces still require UPPER_SNAKE vars.
        assert_eq!(auto_wrap_reference("postgres.char"), "postgres.char");
    }

    fn write_temp_env(name: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("railway-test-{}-{name}", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn env_file_parses_dotenv_format() {
        let path = write_temp_env(
            "basic.env",
            "# comment\n\nFOO=bar\nexport BAZ=qux\nQUOTED=\"hello world\"\nSINGLE='a # not comment'\nTRAIL=value # comment\nREF=postgres.DATABASE_URL\n",
        );
        let vars = parse_env_file(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let map: BTreeMap<_, _> = vars.into_iter().map(|v| (v.key, v.value)).collect();
        assert_eq!(map.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(map.get("BAZ").map(String::as_str), Some("qux"));
        assert_eq!(map.get("QUOTED").map(String::as_str), Some("hello world"));
        assert_eq!(
            map.get("SINGLE").map(String::as_str),
            Some("a # not comment")
        );
        assert_eq!(map.get("TRAIL").map(String::as_str), Some("value"));
        assert_eq!(
            map.get("REF").map(String::as_str),
            Some("postgres.DATABASE_URL")
        );
    }

    #[test]
    fn env_file_invalid_line_errors_with_location() {
        let path = write_temp_env("bad.env", "FOO=bar\nNOT A PAIR\n");
        let err = parse_env_file(&path).unwrap_err().to_string();
        std::fs::remove_file(&path).ok();
        assert!(err.contains(":2:"), "error should cite line 2: {err}");
    }

    #[test]
    fn env_file_missing_errors() {
        assert!(parse_env_file(std::path::Path::new("/nonexistent/x.env")).is_err());
    }

    /// The server treats `template.name` and `template.instructions` as
    /// mutually exclusive; `skip_serializing_none` must keep the unused
    /// boot source off the wire entirely.
    #[test]
    fn checkpoint_boot_serializes_name_only() {
        let input = mutations::sandbox_create::SandboxTemplateInput {
            instructions: None,
            base_image_digest: None,
            name: Some("my-setup".to_string()),
            variables: None,
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json, serde_json::json!({ "name": "my-setup" }));
    }

    #[test]
    fn template_boot_serializes_instructions_only() {
        let input = mutations::sandbox_create::SandboxTemplateInput {
            instructions: Some(vec!["npm i -g pnpm".to_string()]),
            base_image_digest: None,
            name: None,
            variables: None,
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "instructions": ["npm i -g pnpm"] })
        );
    }

    #[test]
    fn flags_override_env_file_entries() {
        let path = write_temp_env("override.env", "FOO=from-file\nKEEP=file-value\n");
        let input = variables_to_input(
            std::slice::from_ref(&path),
            &args(&["FOO=from-flag,REF=shared.char"]),
        )
        .unwrap()
        .unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(input.get("FOO").map(String::as_str), Some("from-flag"));
        assert_eq!(input.get("KEEP").map(String::as_str), Some("file-value"));
        assert_eq!(
            input.get("REF").map(String::as_str),
            Some("${{shared.char}}")
        );
    }
}
