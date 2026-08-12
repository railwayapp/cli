//! `railway ca desktop` — hand a cloud agent to a desktop coding app.
//!
//! The Claude Code and Codex desktop apps both drive a remote machine over
//! ordinary SSH, and the relay already is one: `agent:<env>:<name>@ssh.railway.com`
//! is a complete destination, resolved by name so it survives a recreated VM.
//! So this command writes config rather than building a transport — an OpenSSH
//! block for both apps, plus a `sshConfigs` entry for Claude, which keeps its
//! connections in its own settings file instead of reading `~/.ssh/config`.
//!
//! What it does beyond writing files is provision: the app expects to arrive at
//! a machine where its harness is already signed in, so this runs the same
//! credential and skills pipeline as `railway code` and then stops, without
//! opening a session. That pass also marks the agent `app_mode`, which is what
//! keeps the `~/.profile` autostart from putting a harness where the app expects
//! a login shell.
//!
//! Waking is deliberately not handled here: a sleeping agent refuses the
//! connection at the relay and a GUI has no hook to wake it, so the summary says
//! so and names the command. Auto-wake is its own change.

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

/// Set up a desktop coding app to work on a cloud agent over SSH
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway ca desktop --claude              # Claude Code Desktop\n  railway ca desktop --codex               # the Codex app\n  railway ca desktop --claude --codex      # both, on one agent\n\n  railway ca desktop --claude --agent my-box   # an agent you already have\n  railway ca desktop --claude --dir /app/api   # where sessions open\n  railway ca desktop --claude --dry-run        # print the changes, write nothing\n  railway ca desktop --claude --remove         # undo them\n\nWrites an OpenSSH block for the agent (both apps), and for Claude an entry in\n~/.claude/settings.json pointing at that block. Restart the app afterwards.\n\nThe agent must be awake when the app connects — the relay refuses a sleeping\none and a desktop app has no way to wake it. `railway ca wake <name>` does."
)]
pub struct Args {
    /// Configure Claude Code Desktop
    #[clap(long)]
    claude: bool,

    /// Configure the Codex app
    #[clap(long)]
    codex: bool,

    /// Agent to point the app at, by name or id (defaults to this
    /// environment's, creating one if there is none)
    #[clap(long, value_name = "NAME_OR_ID")]
    agent: Option<String>,

    /// Directory sessions open in on the agent
    #[clap(long, default_value = "/app", value_name = "PATH")]
    dir: String,

    /// Host alias to write (defaults to railway-agent-<name>)
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

    /// Skip the connection check after writing
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum App {
    Claude,
    Codex,
}

impl App {
    /// The harness slug `railway code` uses, so provisioning seeds the
    /// credential this app's remote half will look for.
    fn harness(self) -> &'static str {
        match self {
            App::Claude => "claude",
            App::Codex => "codex",
        }
    }

    fn name(self) -> &'static str {
        match self {
            App::Claude => "Claude Code Desktop",
            App::Codex => "Codex",
        }
    }

    /// The binary the app expects to find on the agent's `PATH`.
    fn remote_bin(self) -> &'static str {
        match self {
            App::Claude => "claude",
            App::Codex => "codex",
        }
    }

    /// Where the app finds the connection once this command has written it.
    fn where_it_appears(self) -> &'static str {
        match self {
            App::Claude => "the environment dropdown, under the name below",
            App::Codex => "the SSH host list — Codex reads ~/.ssh/config itself",
        }
    }
}

pub async fn command(args: Args) -> Result<()> {
    let apps = selected_apps(&args)?;
    let home = dirs::home_dir().context("Unable to get home directory")?;
    let ssh_config_path = ssh_config::expand_tilde(&args.ssh_config)?;

    if args.remove {
        return remove(&args, &apps, &home, &ssh_config_path).await;
    }
    if args.dry_run {
        return dry_run(&args, &apps, &home, &ssh_config_path).await;
    }

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
    let mut prepared: Option<code::Prepared> = None;
    for app in &apps {
        let progress = code::CliProgress::default();
        let mut launch = LaunchArgs::for_app_mode(
            app.harness(),
            args.project.clone(),
            // A named agent carries its own environment. Without this the launch
            // pipeline resolves the *linked* one, fails to find the agent there,
            // and creates a second VM instead of using the one that was named.
            pinned
                .as_ref()
                .map(|a| a.environment_id.clone())
                .or_else(|| args.environment.clone()),
        );
        launch.agent_id = prepared
            .as_ref()
            .map(|p| p.agent_id.clone())
            .or_else(|| pinned.as_ref().map(|a| a.id.clone()));
        let result = code::prepare(&launch, &progress).await;
        progress.finish();
        prepared = Some(result.with_context(|| format!("Preparing the agent for {}", app.name()))?);
    }
    let prepared = prepared.expect("selected_apps guarantees at least one app");

    let alias = args
        .alias
        .clone()
        .unwrap_or_else(|| ssh_config::agent_alias(&prepared.agent_name));
    let block = render_block(
        &prepared.agent_name,
        &prepared.environment_id,
        &alias,
        prepared.identity.as_deref(),
    )?;
    let claude_entry = apps
        .contains(&App::Claude)
        .then(|| claude_ssh_entry(&prepared.agent_id, &prepared.agent_name, &alias, &args.dir));

    let marker = ssh_config::agent_marker(&prepared.environment_id, &prepared.agent_name);
    ssh_config::upsert_marked_block(&ssh_config_path, &marker, &block)
        .with_context(|| format!("Failed to update {}", ssh_config_path.display()))?;
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
        verify(&alias, &apps, &ssh_config_path).await
    };
    if custom_config {
        println!(
            "\n{} {} is not where the apps look. Make sure {} pulls it in:\n    {}",
            "!".yellow().bold(),
            ssh_config_path.display(),
            default_ssh_config_path()?.display(),
            format!("Include {}", ssh_config_path.display()).cyan()
        );
    }

    summarize(
        &prepared,
        &apps,
        &alias,
        &args.dir,
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
    Ok(())
}

impl Args {
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

fn render_block(
    agent_name: &str,
    environment_id: &str,
    alias: &str,
    identity_file: Option<&Path>,
) -> Result<String> {
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
/// quietly bills for a machine is worse than no flag. So this reads an agent
/// that already exists — it will not create one — and reads the local key
/// without registering it. The trade is that the identity shown is a best guess
/// at what a real run would register, which is why it is labelled.
async fn dry_run(args: &Args, apps: &[App], home: &Path, ssh_config_path: &Path) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (agent, _) = ca::resolve(&configs, &client, args.agent.as_deref(), None).await?;

    let identity = preferred_local_key().await;
    let alias = args
        .alias
        .clone()
        .unwrap_or_else(|| ssh_config::agent_alias(&agent.name));
    let block = render_block(
        &agent.name,
        &agent.environment_id,
        &alias,
        identity.as_deref(),
    )?;

    println!("\n{}", ssh_config_path.display().to_string().cyan());
    print!("{block}");
    if apps.contains(&App::Claude) {
        let entry = claude_ssh_entry(&agent.id, &agent.name, &alias, &args.dir);
        println!(
            "\n{}",
            claude_settings_path(home).display().to_string().cyan()
        );
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "sshConfigs": [entry] }))?
        );
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
        "Nothing written, and no agent created or woken (--dry-run).".dimmed()
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
    if apps.is_empty() {
        bail!("Name an app: --claude, --codex, or both.");
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

    let marker = ssh_config::agent_marker(&agent.environment_id, &agent.name);
    let removed_block = ssh_config::remove_marked_block(ssh_config_path, &marker)?;
    let removed_entry = if apps.contains(&App::Claude) {
        remove_claude_ssh_config(home, &agent.id)?
    } else {
        false
    };

    match (removed_block, removed_entry) {
        (false, false) => println!(
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
        .arg(alias)
        .args(command)
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
    dir: &str,
    ssh_config_path: &Path,
    home: &Path,
    checks: &[Check],
) {
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

    println!("\n{}", "Next:".bold());
    for app in apps {
        println!(
            "  {} — restart it, then find the agent in {}",
            app.name(),
            app.where_it_appears()
        );
    }
    // The one thing that will bite a user who does everything right: an agent
    // asleep by the time they open the app.
    println!(
        "\n{} The agent must be awake when the app connects — the relay refuses a\nsleeping one, and a desktop app can't wake it. {} does.",
        "!".yellow().bold(),
        format!("railway ca wake {}", prepared.agent_name).cyan()
    );
    println!(
        "{} stops the compute bill when you're done.",
        format!("railway ca sleep {}", prepared.agent_name).cyan()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_for(argv: &[&str]) -> Args {
        Args::parse_from(std::iter::once("desktop").chain(argv.iter().copied()))
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
