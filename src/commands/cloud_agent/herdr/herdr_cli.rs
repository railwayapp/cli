//! The one place this module runs the `herdr` binary.
//!
//! herdr injects `HERDR_BIN_PATH` into plugin commands so they reach the binary
//! that started them regardless of PATH; tests point it at `tests/fakes/herdr`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Machine {
    pub id: String,
    pub label: String,
    pub target: String,
    #[serde(default)]
    pub session: String,
    pub enabled: bool,
    #[serde(default)]
    pub selected: bool,
}

pub struct Herdr {
    bin: PathBuf,
    env: Vec<(String, String)>,
}

impl Herdr {
    pub fn from_env() -> Self {
        let bin = std::env::var_os("HERDR_BIN_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("herdr"));
        Self {
            bin,
            env: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn at(bin: impl Into<PathBuf>) -> Self {
        Self {
            bin: bin.into(),
            env: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn with_env(mut self, key: &str, value: impl Into<String>) -> Self {
        self.env.push((key.to_string(), value.into()));
        self
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args).envs(self.env.iter().map(|(k, v)| (k, v)));
        cmd
    }

    pub fn machines(&self) -> Result<Vec<Machine>> {
        let out = self.output(&["machine", "list", "--json"])?;
        serde_json::from_str(&out).context("Unexpected `herdr machine list --json` output")
    }

    /// Interactive: herdr asks about host keys and installing its server on the
    /// VM, so stdio is inherited and the caller must be on a terminal.
    pub fn machine_add(&self, target: &str, label: &str) -> Result<()> {
        self.run_inherited(&["machine", "add", target, "--label", label])
    }

    pub fn machine_enable(&self, id: &str) -> Result<()> {
        self.output(&["machine", "enable", id]).map(drop)
    }

    pub fn machine_disable(&self, id: &str) -> Result<()> {
        self.output(&["machine", "disable", id]).map(drop)
    }

    pub fn machine_remove(&self, id: &str) -> Result<()> {
        self.output(&["machine", "remove", id]).map(drop)
    }

    pub fn plugin_link(&self, dir: &Path) -> Result<()> {
        let dir = dir.to_string_lossy();
        self.output(&["plugin", "link", &dir]).map(drop)
    }

    pub fn plugin_unlink(&self, plugin_id: &str) -> Result<()> {
        self.output(&["plugin", "unlink", plugin_id]).map(drop)
    }

    /// Best effort: a toast is never worth failing the action for.
    pub fn notify(&self, title: &str, body: &str) {
        let _ = self.output(&[
            "notification",
            "show",
            title,
            "--body",
            body,
            "--sound",
            "none",
        ]);
    }

    pub fn server_reload_config(&self) -> Result<()> {
        self.output(&["server", "reload-config"]).map(drop)
    }

    pub fn plugin_pane_open(&self, plugin_id: &str, entrypoint: &str) -> Result<()> {
        self.output(&[
            "plugin",
            "pane",
            "open",
            "--plugin",
            plugin_id,
            "--entrypoint",
            entrypoint,
        ])
        .map(drop)
    }

    fn output(&self, args: &[&str]) -> Result<String> {
        let out = self
            .command(args)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("Failed to run {} {}", self.bin.display(), args.join(" ")))?;
        if !out.status.success() {
            bail!(
                "`herdr {}` failed ({}): {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn run_inherited(&self, args: &[&str]) -> Result<()> {
        let status = self
            .command(args)
            .status()
            .with_context(|| format!("Failed to run {} {}", self.bin.display(), args.join(" ")))?;
        if !status.success() {
            bail!("`herdr {}` failed ({status})", args.join(" "));
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! `tests/fakes/herdr` answers `machine list --json` from
    //! `$FAKE_HERDR_MACHINES` and appends every argv line to `$FAKE_HERDR_LOG`.
    //! Both ride the child's env, so parallel tests never share state.

    use std::path::PathBuf;

    use super::Herdr;

    pub struct FakeHerdr {
        dir: tempfile::TempDir,
    }

    impl FakeHerdr {
        pub fn with_machines(json: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("machines.json"), json).unwrap();
            Self { dir }
        }

        pub fn herdr(&self) -> Herdr {
            let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/herdr");
            Herdr::at(bin)
                .with_env(
                    "FAKE_HERDR_MACHINES",
                    self.dir.path().join("machines.json").to_string_lossy(),
                )
                .with_env(
                    "FAKE_HERDR_LOG",
                    self.dir.path().join("log").to_string_lossy(),
                )
        }

        pub fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("log"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_list_parses_and_calls_are_logged() {
        let fake = fake::FakeHerdr::with_machines(
            r#"[{"id":"0123456789abcdef0123456789abcdef","label":"p/a","target":"agent:e:i@ssh.railway.com","session":"default","enabled":true,"selected":false}]"#,
        );
        let herdr = fake.herdr();
        let machines = herdr.machines().unwrap();
        assert_eq!(machines.len(), 1);
        assert_eq!(machines[0].target, "agent:e:i@ssh.railway.com");
        herdr.machine_disable(&machines[0].id).unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                "machine list --json".to_string(),
                "machine disable 0123456789abcdef0123456789abcdef".to_string(),
            ]
        );
    }
}
