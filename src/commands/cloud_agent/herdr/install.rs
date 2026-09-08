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

// prefix+shift+a / prefix+shift+c are unbound in herdr's defaults; the
// descriptions are what `prefix+?` lists under "custom".
const KEYBINDING: &str = r#"[[keys.command]]
key = "prefix+shift+a"
type = "plugin_action"
command = "railway.ca.agents"
description = "railway agents"

[[keys.command]]
key = "prefix+shift+c"
type = "plugin_action"
command = "railway.ca.new"
description = "railway new agent"
"#;

#[derive(Parser)]
pub struct Args {
    /// Unlink the plugin and delete its manifest
    #[clap(long)]
    remove: bool,

    /// Print the manifest and exit without writing or linking
    #[clap(long, conflicts_with = "remove")]
    print: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let dir = super::plugin_dir()?;
    let herdr = Herdr::from_env();
    if args.remove {
        remove_from(&herdr, &dir)?;
        println!("✓ Unlinked herdr plugin {}", PLUGIN_ID.cyan());
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
    println!(
        "\nTo bind the pickers, add to your herdr config.toml:\n\n{}",
        KEYBINDING.dimmed()
    );
    Ok(())
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

fn remove_from(herdr: &Herdr, dir: &Path) -> Result<()> {
    herdr.plugin_unlink(PLUGIN_ID)?;
    match std::fs::remove_file(dir.join(MANIFEST_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
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
struct Manifest {
    id: String,
    name: String,
    version: String,
    min_herdr_version: String,
    description: String,
    platforms: Vec<String>,
    startup: Vec<Startup>,
    actions: Vec<Action>,
    panes: Vec<Pane>,
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
                command: cmd(&["sync"]),
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
            ],
            panes: vec![
                pane("agents", "Railway agents"),
                pane("new", "New Railway agent"),
            ],
        }
    }

    fn render(&self) -> Result<String> {
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
            ["sync", "agents", "new"]
        );
        assert_eq!(
            parsed
                .panes
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["agents", "new"]
        );
        assert_eq!(
            parsed.startup[0].command,
            ["/opt/homebrew/bin/railway", "ca", "herdr", "sync"]
        );
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
}
