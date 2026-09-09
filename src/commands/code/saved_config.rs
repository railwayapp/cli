//! Last successful connection, independent of the current linked project.
//! Keep credentials private from the first write and replace the record atomically.
use std::{fmt::Write as _, fs, io::Write as _, path::Path};

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;
use serde::{Deserialize, Serialize};

use crate::commands::{cloud_agent::opencode, ssh::config as ssh_config};
use crate::config::{Configs, secure_config_dir};
use crate::util::shell::shell_join;

const FILE: &str = "last-code-config.json";
const VERSION: u32 = 1;

/// Replay locally saved connection details without creating or waking an agent
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n  railway code get-config\n  railway code get-config --json\n\nShows the last successful setup or OpenCode reconnect from any directory.\nThe local snapshot includes credentials and is removed by railway logout."
)]
pub(super) struct Args {
    /// Output the saved configuration as JSON, including connection credentials
    #[clap(long)]
    json: bool,
}

// No Debug: the OpenCode connection contains a password.
#[derive(Serialize, Deserialize)]
pub(super) struct SavedConfig {
    version: u32,
    saved_at: chrono::DateTime<chrono::Utc>,
    agent_id: String,
    agent_name: String,
    environment_id: String,
    harness: String,
    ssh_command: String,
    ssh_config: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    opencode: Option<OpenCodeConfig>,
}

#[derive(Serialize, Deserialize)]
struct OpenCodeConfig {
    connection: opencode::Connection,
    beta: bool,
    desktop_configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    desktop_error: Option<String>,
}

impl SavedConfig {
    pub(super) fn from_prepared(prepared: &super::Prepared) -> Result<Self> {
        Self::new(
            &prepared.agent_id,
            &prepared.agent_name,
            &prepared.environment_id,
            prepared.harness,
            prepared.identity.as_deref(),
        )
    }

    pub(super) fn new(
        agent_id: &str,
        agent_name: &str,
        environment_id: &str,
        harness: &str,
        identity: Option<&Path>,
    ) -> Result<Self> {
        // IDs are used in the raw SSH target so duplicate names cannot change
        // which agent the saved details reach.
        if !ssh_config::is_valid_agent_name(agent_id)
            || !ssh_config::is_valid_agent_name(environment_id)
        {
            bail!("Invalid cloud agent or environment ID in connection details");
        }
        let home = dirs::home_dir().context("Unable to locate the home directory")?;
        let known_hosts = home.join(".railway/known_hosts_relay");
        let (host, port) = Configs::get_ssh_relay();
        let target = format!("agent:{environment_id}:{agent_id}");
        let mut config = format!(
            "Host {}\n    HostName {host}\n    User {target}\n",
            ssh_config::agent_alias(agent_name)
        );
        let mut command = vec!["ssh".into()];
        if let Some(port) = port {
            writeln!(config, "    Port {port}")?;
            command.extend(["-p".into(), port.to_string()]);
        }
        if let Some(identity) = identity {
            writeln!(config, "    IdentityFile {}", quote_path(identity)?)?;
            command.extend(["-i".into(), identity.to_string_lossy().into_owned()]);
        }
        writeln!(
            config,
            "    UserKnownHostsFile {}",
            quote_path(&known_hosts)?
        )?;
        config.push_str("    StrictHostKeyChecking accept-new\n    ServerAliveInterval 30\n    ServerAliveCountMax 3\n");
        command.extend([
            "-o".into(),
            format!("UserKnownHostsFile={}", quote_path(&known_hosts)?),
            "-o".into(),
            "StrictHostKeyChecking=accept-new".into(),
            "-o".into(),
            "ServerAliveInterval=30".into(),
            "-o".into(),
            "ServerAliveCountMax=3".into(),
            "--".into(),
            format!("{target}@{host}"),
        ]);
        Ok(Self {
            version: VERSION,
            saved_at: chrono::Utc::now(),
            agent_id: agent_id.into(),
            agent_name: agent_name.into(),
            environment_id: environment_id.into(),
            harness: harness.into(),
            ssh_command: shell_join(&command),
            ssh_config: config,
            opencode: None,
        })
    }

    pub(super) fn with_opencode(
        mut self,
        connection: &opencode::Connection,
        beta: bool,
        desktop: &Result<bool>,
    ) -> Self {
        self.opencode = Some(OpenCodeConfig {
            connection: connection.clone(),
            beta,
            desktop_configured: matches!(desktop, Ok(true)),
            desktop_error: desktop.as_ref().err().map(|error| format!("{error:#}")),
        });
        self
    }

    pub(super) fn save(&self) -> Result<()> {
        let home = dirs::home_dir().context("Unable to locate the home directory")?;
        self.save_in(&home)
    }

    fn save_in(&self, home: &Path) -> Result<()> {
        let contents = serde_json::to_vec_pretty(self)?;
        let directory = home.join(".railway");
        fs::create_dir_all(&directory)?;
        secure_config_dir(&directory)?;
        // NamedTempFile starts at 0600 on Unix; no credentials are ever
        // written to a world-readable staging file, even with a permissive umask.
        let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
        temporary.write_all(&contents)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(directory.join(FILE))
            .map_err(|e| e.error)?;
        Ok(())
    }

    fn load_in(home: &Path) -> Result<Self> {
        let path = home.join(".railway").join(FILE);
        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => bail!(
                "No saved connection details yet. Run railway code to set up an agent, or railway code --opencode connect <agent> (use --opencode2 for Beta) to save an existing OpenCode connection."
            ),
            Err(error) => return Err(error).context("Reading saved Railway code configuration"),
        };
        let saved: Self = serde_json::from_slice(&contents)
            .context("Invalid saved Railway code configuration")?;
        if saved.version != VERSION {
            bail!("Unsupported saved Railway code configuration version; upgrade the CLI");
        }
        Ok(saved)
    }

    fn show(&self) -> Result<()> {
        println!(
            "Saved connection for {} ({})",
            self.agent_name,
            self.saved_at.to_rfc3339()
        );
        if let Some(opencode) = &self.opencode {
            opencode::show_connection(
                &opencode.connection,
                opencode.beta,
                &self.agent_name,
                opencode.desktop_configured,
            )?;
            if let Some(error) = &opencode.desktop_error {
                println!("Desktop configuration was not saved: {error}");
            } else if !opencode.desktop_configured {
                let edition = if opencode.beta {
                    "OpenCode2 [Beta]"
                } else {
                    "OpenCode"
                };
                println!("{edition} Desktop was not detected; desktop configuration was skipped.");
            }
        }
        let divider = "─".repeat(64).cyan();
        println!("\n{divider}");
        println!("{}", "Railway Cloud Agent SSH Configuration:".bold());
        println!("  Agent:       {}", self.agent_name);
        println!("  Agent ID:    {}", self.agent_id);
        println!("  Environment: {}", self.environment_id);
        println!("  Harness:     {}", self.harness);
        println!("\n{}\n  {}", "Connect with SSH:".bold(), self.ssh_command);
        println!("\n{}", "SSH config block (for ~/.ssh/config):".bold());
        print!("{}", self.ssh_config);
        println!("\n{}", "Connect with the Railway CLI:".bold());
        println!(
            "  {}",
            shell_join(&[
                "railway".into(),
                "ca".into(),
                "ssh".into(),
                self.agent_id.clone()
            ])
        );
        println!("{divider}\n");
        Ok(())
    }
}

fn quote_path(path: &Path) -> Result<String> {
    let value = path.to_string_lossy();
    if value.contains(['\n', '\r']) {
        bail!("SSH configuration paths must not contain line breaks");
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

pub(super) fn command(args: Args) -> Result<()> {
    let home = dirs::home_dir().context("Unable to locate the home directory")?;
    let saved = SavedConfig::load_in(&home)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&saved)?);
        Ok(())
    } else {
        saved.show()
    }
}

pub(super) fn clear_in(home: &Path) {
    let _ = fs::remove_file(home.join(".railway").join(FILE));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(password: &str) -> opencode::Connection {
        opencode::Connection {
            url: "https://example.up.railway.app".into(),
            username: "opencode".into(),
            password: password.into(),
            directory: "/app/my project".into(),
            reused: true,
        }
    }

    fn saved(harness: &str) -> SavedConfig {
        SavedConfig::new("agent-123", "my-box", "env-123", harness, None).unwrap()
    }

    #[test]
    fn replaces_connection_and_credentials_privately() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".railway").join(FILE);
        saved("opencode")
            .with_opencode(&connection("old-password"), false, &Ok(false))
            .save_in(home.path())
            .unwrap();
        saved("opencode2")
            .with_opencode(&connection("new-password"), true, &Ok(true))
            .save_in(home.path())
            .unwrap();

        let loaded = SavedConfig::load_in(home.path()).unwrap();
        assert_eq!(loaded.agent_id, "agent-123");
        assert_eq!(loaded.harness, "opencode2");
        let opencode = loaded.opencode.unwrap();
        assert!(opencode.beta && opencode.desktop_configured);
        assert_eq!(opencode.connection.password, "new-password");
        assert_eq!(opencode.connection.directory, "/app/my project");
        assert!(!fs::read_to_string(&path).unwrap().contains("old-password"));
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }

        // A later SSH-only launch must not retain an unrelated server password.
        saved("claude").save_in(home.path()).unwrap();
        assert!(
            SavedConfig::load_in(home.path())
                .unwrap()
                .opencode
                .is_none()
        );
        assert!(!fs::read_to_string(&path).unwrap().contains("password"));
        clear_in(home.path());
        assert!(!path.exists());
        clear_in(home.path());
    }

    #[test]
    fn desktop_failure_preserves_usable_connection() {
        let home = tempfile::tempdir().unwrap();
        saved("opencode")
            .with_opencode(
                &connection("secret"),
                false,
                &Err(anyhow::anyhow!("settings are locked")),
            )
            .save_in(home.path())
            .unwrap();
        let loaded = SavedConfig::load_in(home.path()).unwrap().opencode.unwrap();
        assert_eq!(loaded.connection.password, "secret");
        assert!(!loaded.desktop_configured);
        assert_eq!(loaded.desktop_error.as_deref(), Some("settings are locked"));
    }

    #[test]
    fn ssh_targets_exact_agent_and_quotes_identity() {
        let snapshot = SavedConfig::new(
            "agent-123",
            "My Box",
            "env-123",
            "codex",
            Some(Path::new("/home/my user/key\"file")),
        )
        .unwrap();
        assert!(
            snapshot
                .ssh_config
                .starts_with("Host railway-agent-my-box\n")
        );
        assert!(
            snapshot
                .ssh_config
                .contains("User agent:env-123:agent-123\n")
        );
        assert!(
            snapshot
                .ssh_config
                .contains("IdentityFile \"/home/my user/key\\\"file\"\n")
        );
        assert!(snapshot.ssh_command.contains("agent:env-123:agent-123@"));
        let agent_key = saved("codex");
        assert!(!agent_key.ssh_config.contains("IdentityFile"));
        assert!(quote_path(Path::new("/home/user\nProxyCommand bad")).is_err());
        assert!(SavedConfig::new("bad\nID", "box", "env", "codex", None).is_err());
    }
}
