//! The plugin manifest herdr links. Every command in it is argv with an
//! absolute path to this binary: herdr starts plugin commands with the
//! server's env, not the user's shell PATH.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use colored::Colorize;
use serde::{Deserialize, Serialize};

use super::PLUGIN_ID;
use super::herdr_cli::Herdr;

pub const MANIFEST_FILE: &str = "herdr-plugin.toml";

// Two keys, unbound in herdr's defaults: the picker (which also offers new
// agent and sync now) and wake. The descriptions are what `prefix+?` lists.
const KEYBINDING: &str = r#"[[keys.command]]
key = "prefix+shift+a"
type = "plugin_action"
command = "railway.ca.agents"
description = "railway agents"

[[keys.command]]
key = "prefix+shift+s"
type = "plugin_action"
command = "railway.ca.wake"
description = "railway wake agent"
"#;

/// On a VM the same keys mean: the picker in remote mode, and sleep THIS agent.
/// Same plugin id on both servers, so a binding reads the same wherever the
/// client is pointed.
pub(super) const REMOTE_KEYBINDING: &str = r#"[[keys.command]]
key = "prefix+shift+a"
type = "plugin_action"
command = "railway.ca.agents"
description = "railway agents"

[[keys.command]]
key = "prefix+shift+s"
type = "plugin_action"
command = "railway.ca.sleep-self"
description = "railway sleep this agent"
"#;

#[derive(Parser)]
pub struct Args {
    /// Unlink the plugin and delete its manifest
    #[clap(long)]
    remove: bool,

    /// Print the manifest and exit without writing or linking
    #[clap(long, conflicts_with = "remove")]
    print: bool,

    /// Leave herdr's config.toml alone (no keybindings added or removed)
    #[clap(long)]
    no_keys: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let dir = super::plugin_dir()?;
    let herdr = Herdr::from_env();
    if args.remove {
        if let Some(pid) = super::watch::stop() {
            println!("✓ Stopped the watcher (pid {pid})");
        }
        remove_from(&herdr, &dir)?;
        println!("✓ Unlinked herdr plugin {}", PLUGIN_ID.cyan());
        if !args.no_keys && remove_keybindings(&herdr_config_path()?)? {
            println!("✓ Removed the Railway keybindings from herdr's config.toml");
            let _ = herdr.server_reload_config();
        }
        return Ok(());
    }
    let manifest = Manifest::new(railway_binary()?);
    if args.print {
        print!("{}", manifest.render()?);
        return Ok(());
    }
    install_into(&herdr, &dir, &manifest)?;
    println!(
        "✓ Linked herdr plugin {} from {}",
        PLUGIN_ID.cyan(),
        dir.join(MANIFEST_FILE).display()
    );
    if manifest.binary().contains("/target/") {
        println!(
            "{}",
            format!(
                "The manifest points at a build directory ({}); rerun install after moving or cleaning it.",
                manifest.binary()
            )
            .yellow()
        );
    }
    if let Some(warning) = super::known_hosts::ssh_config_warning() {
        println!("{} {warning}", "!".yellow());
    }
    match super::known_hosts::ensure_relay_known_host()? {
        super::known_hosts::Seeded::Added => {
            println!("✓ Added the Railway ssh relay to ~/.ssh/known_hosts")
        }
        super::known_hosts::Seeded::Present => {}
        super::known_hosts::Seeded::NoSource => println!(
            "{}",
            "The relay's host key is not cached yet; connect once with `railway ca ssh` and rerun install."
                .yellow()
        ),
    }
    if args.no_keys {
        println!(
            "\nTo bind the pickers, add to your herdr config.toml:\n\n{}",
            KEYBINDING.dimmed()
        );
        return Ok(());
    }
    super::watch::spawn_detached();
    std::thread::sleep(std::time::Duration::from_millis(300));
    match super::watch::running() {
        Some(pid) => println!(
            "✓ Watching cloud agent state for this herdr session (pid {pid}, log in {})",
            dir.join("watch*.log").display()
        ),
        None if std::env::var_os("HERDR_SOCKET_PATH").is_none() => println!(
            "{}",
            "Not inside herdr: the watcher starts with herdr's next launch.".dimmed()
        ),
        None => println!(
            "{}",
            "The watcher did not start; see the watch log.".yellow()
        ),
    }
    let config = herdr_config_path()?;
    if ensure_keybindings(&config)? {
        println!(
            "✓ Bound {} agents (new agent and sync live in the picker), {} wake in {}",
            "prefix+shift+a".cyan(),
            "prefix+shift+s".cyan(),
            config.display()
        );
        if herdr.server_reload_config().is_err() {
            println!(
                "{}",
                "herdr is not running; the bindings apply when it starts.".dimmed()
            );
        }
    }
    Ok(())
}

pub(super) const KEYS_MARKER: &str = "# railway ca herdr keys";

/// herdr's own rule: `$XDG_CONFIG_HOME/herdr`, else `~/.config/herdr`.
fn herdr_config_path() -> Result<PathBuf> {
    let dir = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(xdg) if !xdg.is_empty() => PathBuf::from(xdg),
        _ => dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Unable to get home directory"))?
            .join(".config"),
    };
    Ok(dir.join("herdr").join("config.toml"))
}

/// Our marker block is rewritten when the bindings changed; a config that
/// binds the actions on its own, without the marker, is left alone.
fn ensure_keybindings(path: &Path) -> Result<bool> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let current = existing.contains(KEYS_MARKER);
    if !current && existing.contains("railway.ca.agents") {
        return Ok(false);
    }
    let base = if current {
        strip_keybindings(&existing)
    } else {
        existing.clone()
    };
    let wanted = with_keybindings(&base);
    if wanted == existing {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, wanted).with_context(|| format!("Writing {}", path.display()))?;
    Ok(true)
}

fn with_keybindings(existing: &str) -> String {
    let mut out = existing.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(KEYS_MARKER);
    out.push('\n');
    out.push_str(KEYBINDING);
    out
}

/// Drops the marker line and every `[[keys.command]]` table bound to a
/// `railway.ca.*` action. Anything else in the file is kept byte for byte.
fn remove_keybindings(path: &Path) -> Result<bool> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    if !text.contains(KEYS_MARKER) && !text.contains("\"railway.ca.") {
        return Ok(false);
    }
    let stripped = strip_keybindings(&text);
    if stripped == text {
        return Ok(false);
    }
    std::fs::write(path, stripped).with_context(|| format!("Writing {}", path.display()))?;
    Ok(true)
}

fn strip_keybindings(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim() == KEYS_MARKER {
            i += 1;
            continue;
        }
        if line.trim() == "[[keys.command]]" {
            let mut end = i + 1;
            while end < lines.len() && !lines[end].trim_start().starts_with('[') {
                end += 1;
            }
            let ours = lines[i..end]
                .iter()
                .any(|l| l.trim_start().starts_with("command") && l.contains("\"railway.ca."));
            if ours {
                while end > i + 1 && lines[end - 1].trim().is_empty() {
                    end -= 1;
                }
                i = end;
                continue;
            }
        }
        out.push(line);
        i += 1;
    }
    let mut joined = out.join("\n");
    joined.truncate(joined.trim_end_matches('\n').len());
    if !joined.is_empty() && text.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

fn install_into(herdr: &Herdr, dir: &Path, manifest: &Manifest) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("Creating {}", dir.display()))?;
    let path = dir.join(MANIFEST_FILE);
    let text = manifest.render()?;
    let unchanged = std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str());
    std::fs::write(&path, &text).with_context(|| format!("Writing {}", path.display()))?;
    match herdr.plugin_link(dir) {
        Err(e) if is_already_linked(&e) && !unchanged => {
            herdr.plugin_unlink(PLUGIN_ID)?;
            herdr.plugin_link(dir)
        }
        Err(e) if is_already_linked(&e) => Ok(()),
        other => other,
    }
}

fn is_already_linked(e: &anyhow::Error) -> bool {
    e.to_string().to_lowercase().contains("already")
}

/// The manifest goes regardless: `plugin unlink` needs a running herdr, and a
/// stopped one must not leave the files behind.
fn remove_from(herdr: &Herdr, dir: &Path) -> Result<()> {
    let unlinked = herdr.plugin_unlink(PLUGIN_ID);
    match std::fs::remove_file(dir.join(MANIFEST_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    if let Err(e) = unlinked {
        println!(
            "{} herdr did not unlink the plugin ({e:#}); run `herdr plugin unlink {PLUGIN_ID}` once it is up.",
            "!".yellow()
        );
    }
    Ok(())
}

/// The PATH entry when it is this same binary (a stable symlink such as
/// `/opt/homebrew/bin/railway`), otherwise the executable itself (a dev build).
fn railway_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe()?
        .canonicalize()
        .context("Resolving the railway binary path")?;
    Ok(binary_path(exe, std::env::var_os("PATH")))
}

fn binary_path(exe: PathBuf, path: Option<OsString>) -> PathBuf {
    find_in_path("railway", path)
        .filter(|found| found.canonicalize().ok().as_deref() == Some(exe.as_path()))
        .unwrap_or(exe)
}

fn find_in_path(name: &str, path: Option<OsString>) -> Option<PathBuf> {
    std::env::split_paths(&path?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .and_then(|found| std::path::absolute(found).ok())
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Manifest {
    id: String,
    name: String,
    version: String,
    min_herdr_version: String,
    description: String,
    platforms: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    startup: Vec<Startup>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    events: Vec<Event>,
    actions: Vec<Action>,
    panes: Vec<Pane>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Event {
    on: String,
    command: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Startup {
    command: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Action {
    id: String,
    title: String,
    command: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Pane {
    id: String,
    title: String,
    placement: String,
    width: String,
    height: u32,
    command: Vec<String>,
}

impl Manifest {
    fn new(railway: PathBuf) -> Self {
        let railway = railway.to_string_lossy().into_owned();
        let cmd = |args: &[&str]| -> Vec<String> {
            std::iter::once(railway.clone())
                .chain(["ca", "herdr"].into_iter().map(str::to_owned))
                .chain(args.iter().map(|s| s.to_string()))
                .collect()
        };
        let pane = |id: &str, title: &str| Pane {
            id: id.into(),
            title: title.into(),
            placement: "popup".into(),
            width: "80%".into(),
            height: 24,
            command: cmd(&[id]),
        };
        Self {
            id: PLUGIN_ID.into(),
            name: "Railway cloud agents".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            min_herdr_version: "0.9.0".into(),
            description: "Railway cloud agents as herdr machines: create, connect, sleep, wake"
                .into(),
            platforms: vec!["linux".into(), "macos".into()],
            startup: vec![Startup {
                command: cmd(&["sync", "--spawn-watch"]),
            }],
            // Local agent activity is the one server-side event that fires
            // often (workspace focus is client-local in herdr 0.9 and never
            // reaches hooks); 30 s keeps a busy agent cheap.
            events: vec![Event {
                on: "pane.agent_status_changed".into(),
                command: cmd(&["sync", "--debounce", "30"]),
            }],
            actions: vec![
                Action {
                    id: "sync".into(),
                    title: "Railway: sync agents".into(),
                    command: cmd(&["sync"]),
                },
                Action {
                    id: "agents".into(),
                    title: "Railway: agents".into(),
                    command: cmd(&["agents", "--open"]),
                },
                Action {
                    id: "new".into(),
                    title: "Railway: new agent".into(),
                    command: cmd(&["new", "--open"]),
                },
                Action {
                    id: "wake".into(),
                    title: "Railway: wake agent".into(),
                    command: cmd(&["agents", "--wake", "--open"]),
                },
            ],
            panes: vec![
                pane("agents", "Railway agents"),
                pane("new", "New Railway agent"),
                Pane {
                    id: "wake".into(),
                    title: "Wake Railway agent".into(),
                    placement: "popup".into(),
                    width: "80%".into(),
                    height: 24,
                    command: cmd(&["agents", "--wake"]),
                },
            ],
        }
    }

    /// The manifest bootstrap writes on the VM. Commands are the two scripts
    /// bootstrap drops next to it, so it works before the VM's railway binary
    /// knows `ca herdr`.
    pub(super) fn remote() -> Self {
        let sh = |script: &str, extra: &[&str]| -> Vec<String> {
            ["sh", script]
                .into_iter()
                .chain(extra.iter().copied())
                .map(str::to_owned)
                .collect()
        };
        Self {
            id: PLUGIN_ID.into(),
            name: "Railway cloud agents (this VM)".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            min_herdr_version: "0.9.0".into(),
            description: "Railway agents picker and sleep for this VM".into(),
            platforms: vec!["linux".into()],
            startup: Vec::new(),
            events: Vec::new(),
            actions: vec![
                Action {
                    id: "agents".into(),
                    title: "Railway: agents".into(),
                    command: sh("agents.sh", &["--open"]),
                },
                Action {
                    id: "sleep-self".into(),
                    title: "Railway: sleep this agent".into(),
                    command: sh("sleep-self.sh", &[]),
                },
            ],
            panes: vec![Pane {
                id: "agents".into(),
                title: "Railway agents".into(),
                placement: "popup".into(),
                width: "80%".into(),
                height: 24,
                command: sh("agents.sh", &[]),
            }],
        }
    }

    fn binary(&self) -> &str {
        self.actions
            .first()
            .and_then(|a| a.command.first())
            .map(String::as_str)
            .unwrap_or_default()
    }

    pub(super) fn render(&self) -> Result<String> {
        toml::to_string(self).context("Rendering the herdr plugin manifest")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips_with_one_binary_path() {
        let manifest = Manifest::new(PathBuf::from("/opt/homebrew/bin/railway"));
        let text = manifest.render().unwrap();
        let parsed: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(parsed, manifest);
        assert_eq!(parsed.id, "railway.ca");
        assert_eq!(
            parsed
                .actions
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            ["sync", "agents", "new", "wake"]
        );
        assert_eq!(
            parsed
                .panes
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["agents", "new", "wake"]
        );
        assert_eq!(
            parsed.startup[0].command,
            [
                "/opt/homebrew/bin/railway",
                "ca",
                "herdr",
                "sync",
                "--spawn-watch"
            ]
        );
        assert_eq!(parsed.events[0].on, "pane.agent_status_changed");
        assert_eq!(
            parsed.actions[1].command,
            [
                "/opt/homebrew/bin/railway",
                "ca",
                "herdr",
                "agents",
                "--open"
            ]
        );
        assert_eq!(
            parsed.panes[1].command,
            ["/opt/homebrew/bin/railway", "ca", "herdr", "new"]
        );
        let commands = parsed
            .startup
            .iter()
            .map(|s| &s.command)
            .chain(parsed.actions.iter().map(|a| &a.command))
            .chain(parsed.panes.iter().map(|p| &p.command));
        for command in commands {
            assert_eq!(command[0], "/opt/homebrew/bin/railway", "{command:?}");
        }
        assert!(text.contains("[[startup]]"), "{text}");
        assert!(text.contains("[[actions]]"), "{text}");
        assert!(text.contains("[[panes]]"), "{text}");
        assert!(text.contains("placement = \"popup\""), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn path_symlink_to_the_running_exe_wins_over_the_exe() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("target").join("railway");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "").unwrap();
        let exe = exe.canonicalize().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&exe, bin.join("railway")).unwrap();

        let path = Some(OsString::from(bin.to_string_lossy().into_owned()));
        assert_eq!(binary_path(exe.clone(), path), bin.join("railway"));

        let other = tmp.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("railway"), "").unwrap();
        let path = Some(OsString::from(other.to_string_lossy().into_owned()));
        assert_eq!(binary_path(exe.clone(), path), exe);
        assert_eq!(binary_path(exe.clone(), None), exe);
    }

    // The fake herdr is a shebang script: unix only.

    #[cfg(unix)]
    #[test]
    fn install_writes_the_manifest_and_links_the_dir() {
        let fake = super::super::herdr_cli::fake::FakeHerdr::with_machines("[]");
        let herdr = fake.herdr();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("herdr-plugin");
        let manifest = Manifest::new(PathBuf::from("/usr/local/bin/railway"));

        install_into(&herdr, &dir, &manifest).unwrap();
        install_into(&herdr, &dir, &manifest).unwrap();

        let link = format!("plugin link {}", dir.display());
        assert_eq!(fake.calls(), vec![link.clone(), link]);
        let written = std::fs::read_to_string(dir.join(MANIFEST_FILE)).unwrap();
        assert_eq!(toml::from_str::<Manifest>(&written).unwrap(), manifest);

        remove_from(&herdr, &dir).unwrap();
        assert!(!dir.join(MANIFEST_FILE).exists());
        assert_eq!(fake.calls().last().unwrap(), "plugin unlink railway.ca");
    }

    #[test]
    fn keybindings_are_added_once_and_removed_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("herdr").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let before = "[ui]\naccent = \"cyan\"\n\n[[keys.command]]\nkey = \"prefix+alt+g\"\ntype = \"popup\"\ncommand = \"lazygit\"\n";
        std::fs::write(&path, before).unwrap();
        assert!(ensure_keybindings(&path).unwrap());
        assert!(!ensure_keybindings(&path).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("railway.ca.agents").count(), 1, "{text}");
        assert!(text.contains("railway.ca.wake"), "{text}");
        assert!(text.contains("lazygit"), "{text}");
        assert!(remove_keybindings(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert!(!remove_keybindings(&path).unwrap());
    }

    #[test]
    fn remote_manifest_uses_the_dropped_scripts_and_the_same_plugin_id() {
        let text = Manifest::remote().render().unwrap();
        let parsed: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(parsed.id, PLUGIN_ID);
        assert!(parsed.startup.is_empty() && parsed.events.is_empty());
        let ids: Vec<&str> = parsed.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["agents", "sleep-self"]);
        for command in parsed
            .actions
            .iter()
            .map(|a| &a.command)
            .chain(parsed.panes.iter().map(|p| &p.command))
        {
            assert_eq!(command[0], "sh", "{command:?}");
            assert!(command[1].ends_with(".sh"), "{command:?}");
        }
    }

    #[test]
    fn an_outdated_block_is_replaced_and_a_hand_binding_is_respected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let old = format!(
            "x = 1\n\n{KEYS_MARKER}\n[[keys.command]]\nkey = \"prefix+shift+a\"\ntype = \"plugin_action\"\ncommand = \"railway.ca.agents\"\n"
        );
        std::fs::write(&path, &old).unwrap();
        assert!(ensure_keybindings(&path).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("x = 1\n\n"), "{text}");
        assert_eq!(text.matches("[[keys.command]]").count(), 2, "{text}");
        assert!(text.contains("railway.ca.wake"), "{text}");
        assert!(!ensure_keybindings(&path).unwrap());

        let hand = "[[keys.command]]\nkey = \"prefix+m\"\ntype = \"plugin_action\"\ncommand = \"railway.ca.agents\"\n";
        std::fs::write(&path, hand).unwrap();
        assert!(!ensure_keybindings(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), hand);
    }

    #[test]
    fn missing_config_is_created_with_only_our_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("herdr").join("config.toml");
        assert!(ensure_keybindings(&path).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(KEYS_MARKER), "{text}");
        assert!(remove_keybindings(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }
}
