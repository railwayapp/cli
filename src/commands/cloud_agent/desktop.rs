//! `railway ca desktop` — hand a cloud agent to a desktop coding app.
//!
//! Claude Code and Codex drive a remote machine over
//! ordinary SSH, and the relay already is one: `agent:<env>:<name>@ssh.railway.com`
//! is a complete destination, resolved by name so it survives a recreated VM.
//! So this command writes config rather than building a transport — an OpenSSH
//! block for both apps, plus a `sshConfigs` entry for Claude and a declarative
//! SSH/project import for Codex. Codex applies the saved import on its next startup.
//!
//! What it does beyond writing files is provision: the app expects to arrive at
//! a machine where its harness is already signed in, so this runs the same
//! credential and skills pipeline as `railway code`. That pass also marks the
//! agent `app_mode`, which is what
//! keeps the `~/.profile` autostart from putting a harness where the app expects
//! a login shell.
//!
//! OpenCode uses an HTTP server: this command starts `opencode serve`
//! in the background and saves the agent's public HTTPS connection and project
//! in OpenCode Desktop. See the `opencode` and `opencode_config` submodules.
//!
//! The shared launch pipeline wakes a reused agent. OpenCode's server and
//! public endpoint are checked before setup exits.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;
use serde_json::{Value as JsonValue, json};

use crate::client::GQLClient;
use crate::commands::code::{self, LaunchArgs, Progress as _};
use crate::commands::ssh::config as ssh_config;
use crate::config::Configs;
use crate::controllers::cloud_agent as ca;
use crate::util::shell::shell_join;

use super::opencode;
mod codex_config;
mod opencode_config;
pub(crate) use opencode_config::configure_installed as configure_installed_opencode;

/// Set up a desktop coding app to work on a cloud agent over SSH
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway ca desktop --claude\n  railway ca desktop --codex\n  railway ca desktop --opencode\n  railway ca desktop --opencode --new\n  railway ca desktop --opencode2 --new\n  railway ca desktop --opencode --agent my-box\n  railway ca desktop --claude --codex\n\nReuses and wakes the selected agent, creating one if needed. --new always\ncreates a fresh agent. --agent selects an existing box.\n\nClaude and Codex use the generated SSH configuration. Restart Claude after setup.\nCodex also imports the SSH connection and remote project through\n$CODEX_HOME/codex-app/config.json (default ~/.codex/codex-app/config.json).\nSetup saves configuration in the background without opening Codex Desktop.\nThe app imports it on its next startup.\nFind Railway: <agent-name> in Codex's project sidebar.\n\nOpenCode setup requests a code endpoint on newly created VMs (default port 4096).\nExisting VMs use their configured endpoint, or the app port (8080) without one.\nSetup saves the URL, credentials, default server, and project in\nstandard OpenCode with --opencode, or OpenCode2 [Beta] with --opencode2.\nBeta downloads its latest official runtime onto the agent at startup.\nYou may need to restart OpenCode Desktop to load the updated configuration.\nOpen Home → Projects → Railway: <agent-name> → /app (or --dir) → New session.\nSetting a default server does not move existing chats.\nYou can close this terminal after setup. Rerun setup after sleeping or\nrestarting the agent. An occupied server port fails without stopping its process.\n\n--dry-run previews setup without creating, waking, or changing an agent.\n--remove stops the managed OpenCode server on a running agent and removes\nlocal desktop configuration, including the managed OpenCode server entry.\nFor Codex it removes the import declaration; remove already-imported hosts\nand projects in Codex Settings → Connections and the sidebar.\n--ssh-config selects the SSH file used during setup.\n\nThe agent stays awake after setup. `railway ca sleep <name>` stops its compute bill."
)]
pub struct Args {
    /// Configure Claude Code Desktop
    #[clap(long)]
    claude: bool,

    /// Alias for `railway code --codex desktop-only` when selected alone
    #[clap(long)]
    codex: bool,

    /// Start OpenCode on the agent and configure its Desktop app connection
    #[clap(long)]
    opencode: bool,

    /// Download the latest OpenCode2 Beta on the agent and configure Beta Desktop
    #[clap(long, conflicts_with = "opencode")]
    opencode2: bool,

    /// Agent to point the app at, by name or id (defaults to this
    /// environment's, creating one if there is none)
    #[clap(long, value_name = "NAME_OR_ID")]
    agent: Option<String>,

    /// Always create a fresh agent instead of reusing this environment's
    #[clap(long, conflicts_with_all = ["agent", "remove"])]
    new: bool,

    /// Directory sessions open in on the agent
    #[clap(long, default_value = "/app", value_name = "PATH")]
    dir: String,

    /// Host alias to write (Codex: railway-<name>; other apps: railway-agent-<name>)
    #[clap(long)]
    alias: Option<String>,

    /// SSH config file to write
    #[clap(long, default_value = "~/.ssh/config", value_name = "PATH")]
    ssh_config: PathBuf,

    /// Print the changes without writing anything
    #[clap(long)]
    dry_run: bool,

    /// Remove what this command wrote, for the same agent
    #[clap(long, conflicts_with_all = ["dry_run", "dir"])]
    remove: bool,

    /// Skip SSH probes (backend endpoint authentication is always checked)
    #[clap(long)]
    no_verify: bool,

    /// Environment name or ID (defaults to the linked environment)
    #[clap(long, short)]
    environment: Option<String>,

    /// Project ID (defaults to the linked project)
    #[clap(long, short)]
    project: Option<String>,
}

/// A desktop app this command knows how to configure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum App {
    Claude,
    Codex,
    OpenCode,
    OpenCode2,
}

impl App {
    fn is_opencode(self) -> bool {
        matches!(self, Self::OpenCode | Self::OpenCode2)
    }

    /// The harness slug `railway code` uses, so provisioning seeds the
    /// credential this app's remote half will look for.
    fn harness(self) -> &'static str {
        match self {
            App::Claude => "claude",
            App::Codex => "codex",
            App::OpenCode => "opencode",
            App::OpenCode2 => "opencode2",
        }
    }

    fn name(self) -> &'static str {
        match self {
            App::Claude => "Claude Code Desktop",
            App::Codex => "Codex",
            App::OpenCode => "OpenCode Desktop",
            App::OpenCode2 => "OpenCode2 [Beta] Desktop",
        }
    }

    /// The binary the app expects to find on the agent's `PATH`.
    fn remote_bin(self) -> &'static str {
        match self {
            App::Claude => "claude",
            App::Codex => "codex",
            App::OpenCode => "opencode",
            App::OpenCode2 => "opencode2",
        }
    }

    /// Where the app finds the connection once this command has written it.
    fn where_it_appears(self) -> &'static str {
        match self {
            App::Claude => "the environment dropdown, under the name below",
            App::Codex => "Connections and the project sidebar",
            App::OpenCode | App::OpenCode2 => "the server picker",
        }
    }
}

pub async fn command(args: Args) -> Result<()> {
    let apps = selected_apps(&args)?;
    if let Some(alias) = &args.alias
        && !ssh_config::is_valid_agent_name(alias)
    {
        bail!("--alias must be a single SSH host name (letters, digits, '.', '_' or '-').");
    }
    let home = dirs::home_dir().context("Unable to get home directory")?;
    let ssh_config_path = std::path::absolute(ssh_config::expand_tilde(&args.ssh_config)?)?;

    if args.remove {
        return remove(&args, &apps, &home, &ssh_config_path).await;
    }
    if args.dry_run {
        return dry_run(&args, &apps, &home, &ssh_config_path).await;
    }
    if apps == [App::Codex] {
        return code::codex_desktop_only(args.codex_launch_args(), args.codex_options()).await;
    }
    if apps.iter().any(|app| app.is_opencode()) {
        opencode_config::preflight(args.opencode2)?;
    }
    if apps.contains(&App::Codex) {
        preflight_codex_desktop()?;
    }
    let opencode_password = apps
        .iter()
        .any(|app| app.is_opencode())
        .then(opencode::generate_password);

    // Same preflight as a launch, and for the same reason: without the flag the
    // create at the end of provisioning is refused, after a credential has
    // already been spent on it.
    {
        let configs = Configs::new()?;
        let client = GQLClient::new_authorized(&configs)?;
        super::access::ensure_enabled(&client, &configs).await?;
    }

    // Provision once per app, on one agent. The first pass resolves (or creates)
    // it; later passes are pinned to that id, so `--claude --codex` seeds two
    // credentials on one machine rather than spending two VMs.
    let pinned = args.pinned_agent().await?;
    // Resolve once, then carry both IDs through every app's provisioning pass.
    // An environment alone still triggers the project picker in an unlinked
    // directory, even when --agent already identifies the whole target.
    let (project_id, environment_id, local_name_project) = if let Some(agent) = &pinned {
        (agent.project_id.clone(), agent.environment_id.clone(), None)
    } else {
        let mut configs = Configs::new()?;
        let client = GQLClient::new_authorized(&configs)?;
        let target = code::resolve_launch(
            &args.launch_args(apps[0], None, None),
            &mut configs,
            &client,
        )
        .await?;
        (
            target.project_id,
            target.environment_id,
            target.local_name_project,
        )
    };
    let mut prepared: Option<code::Prepared> = None;
    for app in &apps {
        let progress = code::CliProgress::default();
        let mut launch = args.launch_args(
            *app,
            prepared
                .as_ref()
                .map(|p| p.agent_id.clone())
                .or_else(|| pinned.as_ref().map(|a| a.id.clone())),
            Some((&project_id, &environment_id)),
        );
        launch.local_name_project = local_name_project.clone();
        if let Some(password) = &opencode_password {
            launch
                .boot_variables
                .insert("OPENCODE_SERVER_USERNAME".into(), "opencode".into());
            launch
                .boot_variables
                .insert("OPENCODE_SERVER_PASSWORD".into(), password.clone());
        }
        // Desktop never opens the session it provisions — FullTerminal is the
        // non-pane style; the remote command is unused after prepare returns.
        let result = code::prepare(&launch, &progress, code::SessionStyle::FullTerminal).await;
        progress.finish();
        prepared = Some(result.with_context(|| format!("Preparing the agent for {}", app.name()))?);
    }
    let prepared = prepared.expect("selected_apps guarantees at least one app");

    let alias = register_ssh(
        &ssh_config_path,
        &prepared.agent_name,
        &prepared.environment_id,
        prepared.identity.as_deref(),
        args.alias.as_deref(),
        apps.contains(&App::Codex),
    )?;
    let claude_entry = apps
        .contains(&App::Claude)
        .then(|| claude_ssh_entry(&prepared.agent_id, &prepared.agent_name, &alias, &args.dir));

    if let Some(entry) = claude_entry {
        upsert_claude_ssh_config(&home, entry)?;
    }

    // Verify against the file we just wrote. With the default path that is also
    // what the apps read; with `--ssh-config` it is not, and the summary says so
    // rather than passing a check the apps would then fail.
    let custom_config = ssh_config_path != default_ssh_config_path()?;
    let checks = if args.no_verify {
        Vec::new()
    } else {
        let ssh_apps = apps
            .iter()
            .copied()
            .filter(|app| !app.is_opencode())
            .collect::<Vec<_>>();
        if ssh_apps.is_empty() {
            Vec::new()
        } else {
            verify(&alias, &ssh_apps, &ssh_config_path).await
        }
    };
    if custom_config && apps.iter().any(|app| !app.is_opencode()) {
        println!(
            "\n{} {} is not where the apps look. Make sure {} pulls it in:\n    {}",
            "!".yellow().bold(),
            ssh_config_path.display(),
            default_ssh_config_path()?.display(),
            format!("Include {}", ssh_config_path.display()).cyan()
        );
    }

    if apps.contains(&App::Codex) {
        require_codex_checks(&checks)?;
        codex_config::configure(&alias, &prepared.agent_name, &args.dir, &ssh_config_path).await?;
    }

    let connection = if let Some(password) = &opencode_password {
        if args.opencode2 {
            println!(
                "\nStarting OpenCode2 [Beta] and checking its HTTPS connection. The first startup downloads the latest Beta and may take several minutes..."
            );
        } else {
            println!("\nStarting OpenCode and checking its HTTPS connection...");
        }
        Some(
            opencode::start(
                &alias,
                &args.dir,
                &ssh_config_path,
                password,
                args.opencode2,
            )
            .await?,
        )
    } else {
        None
    };

    summarize(
        &prepared,
        &apps,
        &alias,
        &args,
        &ssh_config_path,
        &home,
        &checks,
    );
    super::telemetry::track_desktop_configured(
        &apps
            .iter()
            .map(|a| a.harness())
            .collect::<Vec<_>>()
            .join(","),
    )
    .await;
    if let Some(connection) = connection {
        println!(
            "\n{}",
            if connection.reused {
                "OpenCode is already running"
            } else {
                "OpenCode is ready"
            }
            .bold()
        );
        let desktop = opencode_config::configure(
            args.opencode2,
            &connection,
            &prepared.agent_id,
            &prepared.agent_name,
        )
        .await
        .map(|()| true);
        code::save_desktop_configuration(&prepared, Some(&connection), args.opencode2, &desktop);
        opencode::show_connection(
            &connection,
            args.opencode2,
            &prepared.agent_name,
            desktop.is_ok(),
        )?;
        desktop.context("OpenCode is running, but Desktop configuration failed. Rerun this command to finish setup.")?;
        println!(
            "You can close this terminal. Rerun this command after sleeping or restarting the agent."
        );
    } else {
        code::save_desktop_configuration(&prepared, None, false, &Ok(true));
    }

    Ok(())
}

impl Args {
    fn codex_launch_args(&self) -> LaunchArgs {
        LaunchArgs::for_codex_desktop(
            self.project.clone(),
            self.environment.clone(),
            self.agent.clone(),
            self.dir.clone(),
            self.new,
        )
    }

    fn codex_options(&self) -> CodexOptions {
        CodexOptions {
            alias: self.alias.clone(),
            ssh_config: Some(self.ssh_config.clone()),
            no_verify: self.no_verify,
        }
    }

    fn launch_args(
        &self,
        app: App,
        agent_id: Option<String>,
        target: Option<(&str, &str)>,
    ) -> LaunchArgs {
        let mut launch = LaunchArgs::for_app_mode(
            app.harness(),
            target
                .map(|(project, _)| project.to_owned())
                .or_else(|| self.project.clone()),
            target
                .map(|(_, environment)| environment.to_owned())
                .or_else(|| self.environment.clone()),
        );
        // Only the first app creates. All later apps seed the same agent.
        launch.new = self.new && agent_id.is_none();
        launch.agent_id = agent_id;
        // A preceding Claude/Codex SSH pass may create the VM before the
        // OpenCode pass, so request its endpoint on the first pass too.
        launch.code_endpoint = self.opencode || self.opencode2;
        launch
    }

    /// Resolve `--agent` before any provisioning, so a name that does not exist
    /// fails before a VM is created rather than after.
    async fn pinned_agent(&self) -> Result<Option<ca::Agent>> {
        let Some(selector) = self.agent.as_deref() else {
            return Ok(None);
        };
        let configs = Configs::new()?;
        let client = GQLClient::new_authorized(&configs)?;
        let (agent, _) = ca::resolve(&configs, &client, Some(selector), None).await?;
        Ok(Some(agent))
    }
}

/// Where OpenSSH — and so both apps — read per-user config from.
fn default_ssh_config_path() -> Result<PathBuf> {
    ssh_config::expand_tilde(Path::new("~/.ssh/config"))
}

/// The file-writing path shared by Desktop setup and the Codex terminal client.
fn register_ssh(
    path: &Path,
    agent_name: &str,
    environment_id: &str,
    identity: Option<&Path>,
    alias: Option<&str>,
    codex: bool,
) -> Result<String> {
    let alias = registered_ssh_alias(path, agent_name, environment_id, alias, codex)?;
    let marker = ssh_config::agent_marker(environment_id, agent_name);
    let block = render_block(agent_name, environment_id, &alias, identity)?;
    ssh_config::upsert_marked_block(path, &marker, &block)
        .with_context(|| format!("Failed to update {}", path.display()))?;
    Ok(alias)
}

fn registered_ssh_alias(
    path: &Path,
    agent_name: &str,
    environment_id: &str,
    alias: Option<&str>,
    codex: bool,
) -> Result<String> {
    if alias.is_some_and(|alias| !ssh_config::is_valid_agent_name(alias)) {
        bail!("SSH alias must be a single host name (letters, digits, '.', '_' or '-').");
    }
    let marker = ssh_config::agent_marker(environment_id, agent_name);
    if let Some(alias) = alias {
        return Ok(alias.to_owned());
    }
    let previous = ssh_config::marked_host_alias(path, &marker)?;
    let legacy_default = ssh_config::agent_alias(agent_name);
    if codex
        && previous
            .as_deref()
            .is_none_or(|alias| alias == legacy_default)
    {
        return Ok(ssh_config::codex_agent_alias(agent_name));
    }
    Ok(previous.unwrap_or(legacy_default))
}

pub(crate) fn preflight_codex_desktop() -> Result<()> {
    codex_config::preflight()
}

/// Compatibility options carried by the Desktop command into the shared launcher.
#[derive(Default)]
pub(crate) struct CodexOptions {
    pub alias: Option<String>,
    pub ssh_config: Option<PathBuf>,
    pub no_verify: bool,
}

impl CodexOptions {
    pub(crate) fn config_path(&self) -> Result<PathBuf> {
        std::path::absolute(ssh_config::expand_tilde(
            self.ssh_config
                .as_deref()
                .unwrap_or(Path::new("~/.ssh/config")),
        )?)
        .map_err(Into::into)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self
            .alias
            .as_deref()
            .is_some_and(|alias| !ssh_config::is_valid_agent_name(alias))
        {
            bail!("--alias must be a single SSH host name (letters, digits, '.', '_' or '-').");
        }
        self.config_path()?;
        Ok(())
    }
}

/// Register and verify SSH, then import the remote project into Codex Desktop.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct CodexDesktop {
    pub ssh_alias: String,
    pub ssh_config_path: PathBuf,
    pub config_path: PathBuf,
    pub project_label: String,
    pub remote_path: String,
    pub apply_url: String,
    pub apply_sent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_error: Option<String>,
}

pub(crate) async fn configure_codex(
    agent_name: &str,
    environment_id: &str,
    identity: Option<&Path>,
    directory: &str,
    options: &CodexOptions,
) -> Result<CodexDesktop> {
    let path = options.config_path()?;
    let alias = register_ssh(
        &path,
        agent_name,
        environment_id,
        identity,
        options.alias.as_deref(),
        true,
    )?;
    if !options.no_verify {
        require_codex_checks(&verify(&alias, &[App::Codex], &path).await)?;
    }
    if path != default_ssh_config_path()? {
        eprintln!(
            "Codex reads {}. Make sure it contains: Include {}",
            default_ssh_config_path()?.display(),
            path.display()
        );
    }
    codex_config::configure(&alias, agent_name, directory, &path).await
}

fn require_codex_checks(checks: &[Check]) -> Result<()> {
    for check in checks {
        if !check.ok {
            bail!(
                "Codex Desktop SSH check failed ({}): {}",
                check.label,
                check.detail.as_deref().unwrap_or_default()
            );
        }
    }
    Ok(())
}

fn render_block(
    agent_name: &str,
    environment_id: &str,
    alias: &str,
    identity_file: Option<&Path>,
) -> Result<String> {
    // Fail closed: never serialize a server-returned name that isn't a safe
    // single token into ssh config. Backboard validates this at create, but the
    // sink is here, so we re-check to cover a legacy agent named before that
    // shipped, or a server regression. We refuse rather than sanitize because
    // the relay resolves the agent by this exact name.
    if !ssh_config::is_valid_agent_name(agent_name) {
        bail!(
            "Refusing to write an SSH config for agent {agent_name:?}: its name is not a safe \
             single token (letters, digits, and '.', '_' or '-', starting with a letter or \
             digit). An agent created before name validation shipped can have such a name; \
             recreate it with a valid name and re-run."
        );
    }
    let known_hosts = code::relay_known_hosts()?;
    Ok(ssh_config::render_agent_config_block(
        &ssh_config::AgentBlock {
            agent_name,
            environment_id,
            alias,
            identity_file,
            known_hosts: &known_hosts,
        },
    ))
}

/// Print what a real run would write, and touch nothing.
///
/// Deliberately not the write path with the writes skipped: provisioning would
/// still create or wake a VM, and a flag documented as "write nothing" that
/// quietly bills for a machine is worse than no flag. Read an existing agent,
/// or show placeholders for --new, and read the local key without registering
/// it. The trade is that the identity shown is a best guess
/// at what a real run would register, which is why it is labelled.
async fn dry_run(args: &Args, apps: &[App], home: &Path, ssh_config_path: &Path) -> Result<()> {
    let (agent_id, agent_name, environment_id) = if args.new {
        println!(
            "Would create a fresh cloud agent (--new). Agent and environment IDs below are placeholders."
        );
        println!(
            "Target: project {}, environment {}",
            args.project
                .as_deref()
                .unwrap_or("linked or configured default"),
            args.environment
                .as_deref()
                .unwrap_or("linked or configured default")
        );
        (
            "NEW_AGENT_ID".into(),
            "new-agent".into(),
            "ENVIRONMENT_ID".into(),
        )
    } else {
        let configs = Configs::new()?;
        let client = GQLClient::new_authorized(&configs)?;
        let (agent, _) = ca::resolve(&configs, &client, args.agent.as_deref(), None).await?;
        (agent.id, agent.name, agent.environment_id)
    };

    let identity = preferred_local_key().await;
    let alias = registered_ssh_alias(
        ssh_config_path,
        &agent_name,
        &environment_id,
        args.alias.as_deref(),
        apps.contains(&App::Codex),
    )?;
    let block = render_block(&agent_name, &environment_id, &alias, identity.as_deref())?;

    println!("\n{}", ssh_config_path.display().to_string().cyan());
    print!("{block}");
    if apps.contains(&App::Codex) {
        if apps == [App::Codex] {
            println!(
                "Would request a code endpoint for a new VM, then start or reuse the authenticated Codex App Server on its configured port (default 4096; legacy connections use 8080) and verify its public WebSocket endpoint."
            );
        }
        codex_config::preview(&alias, &agent_name, &args.dir)?;
    }
    if apps.contains(&App::Claude) {
        let entry = claude_ssh_entry(&agent_id, &agent_name, &alias, &args.dir);
        println!(
            "\n{}",
            claude_settings_path(home).display().to_string().cyan()
        );
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "sshConfigs": [entry] }))?
        );
    }
    if apps.iter().any(|app| app.is_opencode()) {
        println!("\nWould generate credentials for a new agent, or reuse its saved credentials.");
        println!(
            "Would request a code endpoint for a new VM, then start password-protected OpenCode on its configured port (default 4096; legacy connections use 8080) from {}.",
            args.dir
        );
        println!(
            "Would verify the agent's existing HTTPS address and print its connection details."
        );
        for target in opencode_config::targets(args.opencode2)? {
            println!(
                "Would save the authenticated server, default server, and project in {} ({}).",
                target.name(),
                target.root.display()
            );
        }
        println!("You may need to restart OpenCode Desktop to load the updated configuration.");
    }
    if identity.is_none() {
        println!(
            "\n{}",
            "No local key file found, so no IdentityFile line — a real run registers a key and may add one."
                .dimmed()
        );
    }
    println!(
        "\n{}",
        "Nothing written, no agent created or woken, and no process started (--dry-run).".dimmed()
    );
    Ok(())
}

/// The key path a real run would most likely use, without registering anything.
///
/// Same preferred-key order as the non-interactive `ssh keys add`. `None` covers
/// both "no keys" and "keys only in an ssh-agent" — neither has a path to write,
/// and the block is correct without one.
async fn preferred_local_key() -> Option<PathBuf> {
    use crate::controllers::ssh::keys::{SshKeySource, find_local_ssh_keys};
    find_local_ssh_keys()
        .await
        .ok()?
        .into_iter()
        .find_map(|key| match key.source {
            SshKeySource::File(path) => Some(path),
            SshKeySource::Agent => None,
        })
}

/// Which apps this run is about. At least one, because there is no sensible
/// default: configuring both when asked for neither would write files nobody
/// asked for.
fn selected_apps(args: &Args) -> Result<Vec<App>> {
    let mut apps = Vec::new();
    if args.claude {
        apps.push(App::Claude);
    }
    if args.codex {
        apps.push(App::Codex);
    }
    if args.opencode {
        apps.push(App::OpenCode);
    }
    if args.opencode2 {
        apps.push(App::OpenCode2);
    }
    if apps.is_empty() {
        bail!(
            "Name an app: --claude, --codex, --opencode, or --opencode2. OpenCode editions must be configured separately."
        );
    }
    Ok(apps)
}

async fn remove(args: &Args, apps: &[App], home: &Path, ssh_config_path: &Path) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    // Removal names the agent the same way everything else does, but must not
    // create one — an agent that is already gone leaves config behind, and
    // `--agent` is how that is cleaned up.
    let (agent, _) = ca::resolve(&configs, &client, args.agent.as_deref(), None).await?;

    let removed_codex = if apps.contains(&App::Codex) {
        let alias = registered_ssh_alias(
            ssh_config_path,
            &agent.name,
            &agent.environment_id,
            args.alias.as_deref(),
            false,
        )?;
        let mut removed = codex_config::remove(&alias)?;
        if args.alias.is_none() {
            removed |= codex_config::remove(&ssh_config::codex_agent_alias(&agent.name))?;
        }
        removed
    } else {
        false
    };
    let removed_opencode = if apps.iter().any(|app| app.is_opencode()) {
        opencode_config::remove(&agent.id, args.opencode2).await?
    } else {
        false
    };
    let stopped_opencode =
        apps.iter().any(|app| app.is_opencode()) && agent.status == ca::Status::Running;
    if stopped_opencode {
        let alias = registered_ssh_alias(
            ssh_config_path,
            &agent.name,
            &agent.environment_id,
            args.alias.as_deref(),
            false,
        )?;
        opencode::stop(&alias, ssh_config_path, args.opencode2).await?;
    }
    let marker = ssh_config::agent_marker(&agent.environment_id, &agent.name);
    let removed_block = ssh_config::remove_marked_block(ssh_config_path, &marker)?;
    let removed_entry = if apps.contains(&App::Claude) {
        remove_claude_ssh_config(home, &agent.id)?
    } else {
        false
    };
    match (
        removed_block,
        removed_entry,
        stopped_opencode,
        removed_opencode,
        removed_codex,
    ) {
        (false, false, false, false, false) => println!(
            "No `railway ca desktop` config found for agent {}.",
            agent.name.cyan()
        ),
        _ => {
            if removed_block {
                println!(
                    "{} Removed the SSH block from {}",
                    "✓".green(),
                    ssh_config_path.display()
                );
            }
            if removed_entry {
                println!(
                    "{} Removed the connection from {}",
                    "✓".green(),
                    claude_settings_path(home).display()
                );
            }
            if stopped_opencode {
                println!("{} Stopped the managed OpenCode server.", "✓".green());
            }
            if removed_opencode {
                println!(
                    "{} Removed the managed OpenCode Desktop connection and project.",
                    "✓".green()
                );
            }
            if removed_codex {
                println!(
                    "{} Removed the Codex Desktop import declaration. Remove the already-imported host in Codex Settings → Connections and its project from the sidebar.",
                    "✓".green()
                );
            }
            println!(
                "\nThe agent itself is untouched. {} deletes it.",
                format!("railway ca delete {}", agent.name).cyan()
            );
        }
    }
    Ok(())
}

// --- Claude Code Desktop's own settings file ------------------------------
//
// Claude keeps SSH connections in `sshConfigs` in ~/.claude/settings.json —
// note that is not the ~/.claude.json that `railway mcp install` writes. Only
// `id`, `name` and `sshHost` are required; `sshPort` and `sshIdentityFile` are
// deliberately omitted so the OpenSSH block stays the single source of truth for
// port and key. A dev-environment CLI writing the relay's :2222 then needs no
// second place to be right.

fn claude_settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

fn claude_ssh_entry(agent_id: &str, agent_name: &str, alias: &str, dir: &str) -> JsonValue {
    json!({
        "id": claude_entry_id(agent_id),
        "name": format!("Railway · {agent_name}"),
        "sshHost": alias,
        "startDirectory": dir,
    })
}

/// Keyed on the agent id so re-running replaces its own entry instead of adding
/// a second one, and so a rename does not orphan the old entry.
fn claude_entry_id(agent_id: &str) -> String {
    format!("railway-agent-{agent_id}")
}

fn upsert_claude_ssh_config(home: &Path, entry: JsonValue) -> Result<()> {
    let path = claude_settings_path(home);
    let mut root = read_json_or_empty(&path)?;
    let id = entry.get("id").and_then(JsonValue::as_str).unwrap_or("");

    let configs = root
        .as_object_mut()
        .context("settings.json is not a JSON object")?
        .entry("sshConfigs")
        .or_insert_with(|| json!([]));
    let list = configs
        .as_array_mut()
        .context("`sshConfigs` in settings.json is not an array")?;
    // Replace in place when it is already there, so a re-run doesn't move the
    // user's connection to the bottom of their dropdown.
    match list
        .iter()
        .position(|e| e.get("id").and_then(JsonValue::as_str) == Some(id))
    {
        Some(at) => list[at] = entry,
        None => list.push(entry),
    }

    write_json_pretty(&path, &root)
}

fn remove_claude_ssh_config(home: &Path, agent_id: &str) -> Result<bool> {
    let path = claude_settings_path(home);
    if !path.exists() {
        return Ok(false);
    }
    let mut root = read_json_or_empty(&path)?;
    let id = claude_entry_id(agent_id);
    let Some(list) = root.get_mut("sshConfigs").and_then(JsonValue::as_array_mut) else {
        return Ok(false);
    };
    let before = list.len();
    list.retain(|e| e.get("id").and_then(JsonValue::as_str) != Some(id.as_str()));
    if list.len() == before {
        return Ok(false);
    }
    write_json_pretty(&path, &root)?;
    Ok(true)
}

/// An unreadable settings file is an error, but a missing one is just a first
/// run. Same split as `railway mcp install` makes.
fn read_json_or_empty(path: &Path) -> Result<JsonValue> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse existing JSON at {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

fn write_json_pretty(path: &Path, value: &JsonValue) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    crate::util::write_atomic(path, &text)
        .with_context(|| format!("Failed to write {}", path.display()))
}

// --- Verification ---------------------------------------------------------

struct Check {
    label: String,
    ok: bool,
    detail: Option<String>,
}

/// Prove the entry works before claiming it does.
///
/// Every failure here is one a desktop app would swallow: it spawns `ssh`
/// without a terminal, so a rejected key, an unreachable relay or a missing
/// binary all surface as an unexplained connection error inside a GUI. Better to
/// find them in the shell that just wrote the config.
async fn verify(alias: &str, apps: &[App], ssh_config_path: &Path) -> Vec<Check> {
    let mut checks = Vec::new();
    let connect = ssh_alias(alias, &["true"], ssh_config_path).await;
    let connected = connect.is_ok();
    checks.push(Check {
        label: format!("ssh {alias}"),
        ok: connected,
        detail: connect.err().map(|e| format!("{e:#}")),
    });
    if !connected {
        // The PATH probes all fail the same way, and three copies of one
        // connection error reads as three problems.
        return checks;
    }

    for app in apps {
        let bin = app.remote_bin();
        // Through a login shell specifically: that is how Codex bootstraps its
        // remote server, and the agent image puts the harnesses on PATH in
        // /root/.profile rather than for non-login shells.
        let probe = ssh_alias(
            alias,
            &["bash", "-lc", &format!("command -v {bin}")],
            ssh_config_path,
        )
        .await;
        checks.push(Check {
            label: format!("{bin} on PATH (login shell)"),
            ok: probe.is_ok(),
            detail: probe.err().map(|e| format!("{e:#}")),
        });
        if app.is_opencode() {
            let probe = ssh_alias(
                alias,
                &["bash", "-lc", "opencode serve --help"],
                ssh_config_path,
            )
            .await;
            checks.push(Check {
                label: "opencode serve available".into(),
                ok: probe.is_ok(),
                detail: probe.err().map(|e| format!("{e:#}")),
            });
        }
    }
    checks
}

/// Run one command through the alias we just wrote, with the system `ssh` and
/// nothing else — the same way the desktop apps will.
///
/// `BatchMode=yes` matters: without it a misconfigured entry can sit on a
/// password prompt with no one watching.
async fn ssh_alias(alias: &str, command: &[&str], ssh_config_path: &Path) -> Result<()> {
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.arg("-F")
        .arg(ssh_config_path)
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=20")
        .arg("-T")
        .arg("--")
        .arg(alias)
        // OpenSSH joins argv with spaces before invoking the remote shell.
        // Quote the whole command so `bash -lc` receives one script argument.
        .arg(shell_join(
            &command.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        ))
        .stdin(std::process::Stdio::null());
    let out = cmd.output().await.context("Failed to run `ssh`")?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The last non-empty line: ssh's actual complaint, after any banner.
    let reason = stderr
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("ssh failed with no output")
        .trim();
    bail!("{reason}")
}

// --- Output ---------------------------------------------------------------

fn summarize(
    prepared: &code::Prepared,
    apps: &[App],
    alias: &str,
    args: &Args,
    ssh_config_path: &Path,
    home: &Path,
    checks: &[Check],
) {
    let dir = &args.dir;
    println!("\n{}", "Cloud agent ready for your desktop app".bold());
    println!("  {}  {}", "Agent  ".dimmed(), prepared.agent_name.bold());
    println!("  {}  {}", "Host   ".dimmed(), alias.bold());
    println!("  {}  {}", "Opens  ".dimmed(), dir.bold());
    println!(
        "  {}  {}",
        "Wrote  ".dimmed(),
        ssh_config_path.display().to_string().bold()
    );
    if apps.contains(&App::Claude) {
        println!(
            "  {}  {}",
            "       ".dimmed(),
            claude_settings_path(home).display().to_string().bold()
        );
    }
    if !checks.is_empty() {
        println!();
        for check in checks {
            let mark = if check.ok { "✓".green() } else { "✗".red() };
            match &check.detail {
                Some(detail) => println!("  {mark} {}  {}", check.label, detail.dimmed()),
                None => println!("  {mark} {}", check.label),
            }
        }
    }

    if apps.iter().any(|app| !app.is_opencode()) {
        println!("\n{}", "Next:".bold());
    }
    for app in apps {
        if app.is_opencode() {
            continue;
        }
        if *app == App::Codex {
            println!(
                "  Codex — find Railway: {} in {}",
                prepared.agent_name,
                app.where_it_appears()
            );
            continue;
        }
        println!(
            "  {} — restart it, then find the agent in {}",
            app.name(),
            app.where_it_appears()
        );
    }
    if !apps.iter().any(|app| app.is_opencode()) {
        println!(
            "\n{} The agent must be awake when the app connects — the relay refuses a\nsleeping one, and a desktop app can't wake it. {} does.",
            "!".yellow().bold(),
            format!("railway ca wake {}", prepared.agent_name).cyan()
        );
    }
    println!(
        "{} stops the compute bill when you're done.",
        format!("railway ca sleep {}", prepared.agent_name).cyan()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn args_for(argv: &[&str]) -> Args {
        Args::parse_from(std::iter::once("desktop").chain(argv.iter().copied()))
    }

    #[test]
    fn codex_alias_maps_to_the_canonical_desktop_only_launch() {
        for extra in [
            vec![],
            vec!["--new"],
            vec![
                "--agent",
                "box",
                "--dir",
                "/app/a project",
                "-p",
                "project",
                "-e",
                "env",
            ],
        ] {
            let legacy = args_for(&[vec!["--codex"], extra.clone()].concat());
            let mut canonical = vec!["code", "--codex", "desktop-only"];
            if !extra.contains(&"--dir") {
                canonical.extend(["--dir", "/app"]);
            }
            canonical.extend(extra);
            assert_eq!(
                legacy.codex_launch_args(),
                LaunchArgs::try_parse_from(canonical).unwrap()
            );
        }
        let args = args_for(&[
            "--codex",
            "--alias",
            "my-desktop",
            "--ssh-config",
            "/custom/ssh config",
            "--no-verify",
        ]);
        let options = args.codex_options();
        assert_eq!(options.alias.as_deref(), Some("my-desktop"));
        assert_eq!(
            options.ssh_config.as_deref(),
            Some(Path::new("/custom/ssh config"))
        );
        assert!(options.no_verify);
        assert!(options.validate().is_ok());
    }

    #[test]
    fn shared_ssh_registration_preserves_desktop_alias_and_unrelated_hosts() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".ssh/config");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let personal = "Host personal\n    HostName personal.example.com\n";
        fs::write(&path, personal).unwrap();
        let old_key = home.path().join("old key");
        let new_key = home.path().join("new key");
        let alias = register_ssh(
            &path,
            "box",
            "env-id",
            Some(&old_key),
            Some("my-codex"),
            true,
        )
        .unwrap();
        assert_eq!(alias, "my-codex");

        // Terminal setup uses the same registration without requesting an alias.
        let alias = register_ssh(&path, "box", "env-id", Some(&new_key), None, true).unwrap();
        assert_eq!(alias, "my-codex");
        assert_eq!(
            registered_ssh_alias(&path, "box", "env-id", None, true).unwrap(),
            alias
        );
        let updated = fs::read_to_string(&path).unwrap();
        assert!(updated.starts_with(personal));
        assert_eq!(
            updated.matches("# BEGIN railway:agent:env-id:box").count(),
            1
        );
        assert!(updated.contains("User agent:env-id:box"));
        assert!(updated.contains("new key"));
        assert!(!updated.contains("old key"));
        register_ssh(&path, "box", "env-id", Some(&new_key), None, true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), updated);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn shared_ssh_registration_creates_a_default_and_leaves_ambiguous_blocks_intact() {
        for (codex, name, expected) in [
            (false, "box", "railway-agent-box"),
            (true, "codex-railg-3ed", "railway-codex-railg-3ed"),
        ] {
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join(".ssh/config");
            assert_eq!(
                register_ssh(&path, name, "env-id", None, None, codex).unwrap(),
                expected
            );
            let existing = fs::read_to_string(&path).unwrap();
            let ambiguous = existing.replace(&format!("Host {expected}\n"), "Host box other\n");
            fs::write(&path, &ambiguous).unwrap();
            assert!(register_ssh(&path, name, "env-id", None, None, codex).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), ambiguous);
            assert!(
                register_ssh(
                    &path,
                    name,
                    "env-id",
                    None,
                    Some("bad\nHost injected"),
                    codex
                )
                .is_err()
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), ambiguous);
        }
    }

    #[test]
    fn codex_registration_migrates_the_legacy_default_in_place() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".ssh/config");
        let name = "codex-railg-3ed";
        register_ssh(&path, name, "env-id", None, None, false).unwrap();
        let legacy = fs::read_to_string(&path).unwrap();
        assert_eq!(
            registered_ssh_alias(&path, name, "env-id", None, true).unwrap(),
            "railway-codex-railg-3ed"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), legacy);
        for _ in 0..2 {
            assert_eq!(
                register_ssh(&path, name, "env-id", None, None, true).unwrap(),
                "railway-codex-railg-3ed"
            );
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                legacy.replace(
                    "Host railway-agent-codex-railg-3ed\n",
                    "Host railway-codex-railg-3ed\n"
                )
            );
            // Other apps sharing this registration use the migrated host too.
            assert_eq!(
                registered_ssh_alias(&path, name, "env-id", None, false).unwrap(),
                "railway-codex-railg-3ed"
            );
        }
    }

    #[test]
    fn an_app_must_be_named() {
        assert!(selected_apps(&args_for(&[])).is_err());
        assert_eq!(selected_apps(&args_for(&["--claude"])).unwrap().len(), 1);
        assert_eq!(
            selected_apps(&args_for(&["--claude", "--codex"]))
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn opencode_editions_are_explicit_and_mutually_exclusive() {
        assert_eq!(
            selected_apps(&args_for(&["--opencode2"])).unwrap(),
            vec![App::OpenCode2]
        );
        assert!(Args::try_parse_from(["desktop", "--opencode", "--opencode2"]).is_err());
        assert!(Args::try_parse_from(["desktop", "--opencode2", "--new"]).is_ok());
    }

    #[test]
    fn opencode_can_be_configured_alone_or_with_other_apps() {
        assert_eq!(
            selected_apps(&args_for(&["--opencode"])).unwrap(),
            vec![App::OpenCode]
        );
        assert_eq!(
            selected_apps(&args_for(&["--claude", "--codex", "--opencode"])).unwrap(),
            vec![App::Claude, App::Codex, App::OpenCode]
        );
        for flag in ["--dry-run", "--remove", "--no-verify"] {
            assert!(Args::try_parse_from(["desktop", "--opencode", flag]).is_ok());
        }
        for flag in ["--port", "--remote-port"] {
            assert!(Args::try_parse_from(["desktop", "--opencode", flag, "4096"]).is_err());
        }
    }

    #[test]
    fn new_creates_once_and_later_apps_reuse_the_created_agent() {
        let args = args_for(&[
            "--claude",
            "--codex",
            "--opencode",
            "--new",
            "-e",
            "production",
        ]);
        let first = args.launch_args(App::Claude, None, None);
        assert!(first.new);
        assert!(first.code_endpoint);
        assert!(first.agent_id.is_none());
        assert_eq!(first.environment.as_deref(), Some("production"));
        for app in [App::Codex, App::OpenCode] {
            let next = args.launch_args(
                app,
                Some("created-agent".into()),
                Some(("created-project", "created-env")),
            );
            assert!(!next.new);
            assert_eq!(next.agent_id.as_deref(), Some("created-agent"));
            assert_eq!(next.environment.as_deref(), Some("created-env"));
            assert_eq!(next.project.as_deref(), Some("created-project"));
        }
    }

    #[test]
    fn desktop_reuses_by_default_and_new_conflicts_with_existing_agent_actions() {
        let args = args_for(&["--opencode"]);
        assert!(!args.launch_args(App::OpenCode, None, None).new);
        let pinned = args.launch_args(
            App::OpenCode,
            Some("existing-agent".into()),
            Some(("other-project", "other-env")),
        );
        assert!(!pinned.new);
        assert_eq!(pinned.agent_id.as_deref(), Some("existing-agent"));
        assert_eq!(pinned.environment.as_deref(), Some("other-env"));
        assert_eq!(pinned.project.as_deref(), Some("other-project"));
        assert!(
            Args::try_parse_from(["desktop", "--opencode", "--new", "--agent", "existing"])
                .is_err()
        );
        assert!(Args::try_parse_from(["desktop", "--opencode", "--new", "--remove"]).is_err());
        assert!(Args::try_parse_from(["desktop", "--opencode", "--new", "--dry-run"]).is_ok());
    }

    #[test]
    fn ssh_only_desktop_setup_does_not_request_a_code_endpoint() {
        let args = args_for(&["--claude", "--codex", "--new"]);
        assert!(!args.launch_args(App::Claude, None, None).code_endpoint);
    }

    #[test]
    fn render_block_refuses_an_unsafe_agent_name() {
        // A newline in the server-returned name would otherwise become its own
        // ssh directive; render_block must fail closed rather than emit it.
        let err = render_block(
            "bbp\n    ProxyCommand /bin/sh -c 'id'",
            "env-1",
            "railway-agent-bbp",
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not a safe single token"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn the_claude_entry_omits_what_the_ssh_block_owns() {
        let entry = claude_ssh_entry("ca_1", "my-box", "railway-agent-my-box", "/app");
        assert_eq!(entry["sshHost"], "railway-agent-my-box");
        assert_eq!(entry["id"], "railway-agent-ca_1");
        assert_eq!(entry["startDirectory"], "/app");
        // Port and key live in ~/.ssh/config, so there is one place to be right.
        assert!(entry.get("sshPort").is_none());
        assert!(entry.get("sshIdentityFile").is_none());
    }

    /// Running twice must leave one entry, in its original position — a
    /// duplicate would show as two identical rows in the app's dropdown.
    #[test]
    fn the_settings_merge_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let path = claude_settings_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"sshConfigs":[{"id":"mine","name":"Mine","sshHost":"box"}],"theme":"dark"}"#,
        )
        .unwrap();

        let entry = claude_ssh_entry("ca_1", "my-box", "railway-agent-my-box", "/app");
        upsert_claude_ssh_config(home.path(), entry.clone()).unwrap();
        upsert_claude_ssh_config(home.path(), entry).unwrap();

        let root: JsonValue =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let list = root["sshConfigs"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        // The user's own entry keeps its place and its contents.
        assert_eq!(list[0]["id"], "mine");
        assert_eq!(list[1]["id"], "railway-agent-ca_1");
        // Unrelated settings survive the merge.
        assert_eq!(root["theme"], "dark");
    }

    #[test]
    fn removal_takes_only_our_entry() {
        let home = tempfile::tempdir().unwrap();
        let path = claude_settings_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"sshConfigs":[{"id":"mine"}]}"#).unwrap();

        upsert_claude_ssh_config(
            home.path(),
            claude_ssh_entry("ca_1", "my-box", "railway-agent-my-box", "/app"),
        )
        .unwrap();
        assert!(remove_claude_ssh_config(home.path(), "ca_1").unwrap());
        // Gone once, and reporting `false` the second time rather than erroring.
        assert!(!remove_claude_ssh_config(home.path(), "ca_1").unwrap());

        let root: JsonValue =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let list = root["sshConfigs"].as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["id"], "mine");
    }

    /// A settings file with no `sshConfigs` yet is the common first run.
    #[test]
    fn a_missing_settings_file_is_a_first_run_not_an_error() {
        let home = tempfile::tempdir().unwrap();
        upsert_claude_ssh_config(
            home.path(),
            claude_ssh_entry("ca_1", "my-box", "railway-agent-my-box", "/app"),
        )
        .unwrap();
        let root: JsonValue = serde_json::from_str(
            &std::fs::read_to_string(claude_settings_path(home.path())).unwrap(),
        )
        .unwrap();
        assert_eq!(root["sshConfigs"][0]["id"], "railway-agent-ca_1");
        // An id that was never written reports `false`, not an error.
        assert!(!remove_claude_ssh_config(home.path(), "nope").unwrap());
    }

    #[test]
    fn corrupt_settings_are_reported_not_overwritten() {
        let home = tempfile::tempdir().unwrap();
        let path = claude_settings_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert!(
            upsert_claude_ssh_config(
                home.path(),
                claude_ssh_entry("ca_1", "b", "railway-agent-b", "/app")
            )
            .is_err()
        );
        // Untouched, so the user can fix it.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
    }
}
