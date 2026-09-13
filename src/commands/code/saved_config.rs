//! Offline connection replay. The legacy latest snapshot remains readable by
//! older CLIs; an atomic, locked archive retains the latest snapshot per agent.
use std::{fmt::Write as _, fs, io::Write as _, path::Path};

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::commands::{
    cloud_agent::{codex, desktop::CodexDesktop, opencode},
    ssh::config as ssh_config,
};
use crate::config::{Configs, secure_config_dir};
use crate::util::shell::shell_join;

const FILE: &str = "last-code-config.json";
const ARCHIVE: &str = "code-configs.json";
const LOCK: &str = ".code-config.lock";
const VERSION: u32 = 1;

/// Known instances can be probed without opening SSH or changing remote state.
pub(crate) fn client_connection(
    agent_id: &str,
    environment_id: &str,
) -> Option<crate::commands::cloud_agent::client_sessions::Connection> {
    let saved = SavedConfig::load_in(&dirs::home_dir()?, Some(agent_id)).ok()?;
    if saved.agent_id != agent_id || saved.environment_id != environment_id {
        return None;
    }
    use crate::commands::cloud_agent::client_sessions::Connection;
    saved
        .codex
        .map(|c| Connection::Codex(c.connection))
        .or_else(|| {
            saved
                .opencode
                .map(|c| Connection::OpenCode(c.connection, c.beta))
        })
}

/// Show locally saved connection details without creating or waking an agent
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n  railway code get-config\n  railway code get-config my-box\n  railway code get-config my-box --json\n\nReads local snapshots from any directory, without login or network access.\nOmit the agent to show the latest snapshot; use an ID for ambiguous names.\nSnapshots include credentials and are removed by railway logout."
)]
pub(super) struct Args {
    /// Agent name or ID (defaults to the most recently saved connection)
    #[clap(value_name = "AGENT")]
    agent: Option<String>,
    /// Output the saved configuration as JSON, including connection credentials
    #[clap(long)]
    json: bool,
}

// No Debug: snapshots include bearer tokens and passwords.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SavedConfig {
    version: u32,
    saved_at: chrono::DateTime<chrono::Utc>,
    pub(super) agent_id: String,
    pub(super) agent_name: String,
    pub(super) environment_id: String,
    harness: String,
    ssh_command: String,
    ssh_config: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    opencode: Option<OpenCodeConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    codex: Option<CodexConfig>,
}

#[derive(Clone, Serialize, Deserialize)]
struct OpenCodeConfig {
    connection: opencode::Connection,
    beta: bool,
    desktop_configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    desktop_error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CodexConfig {
    connection: codex::Connection,
    desktop_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    desktop: Option<CodexDesktop>,
    #[serde(skip_serializing_if = "Option::is_none")]
    desktop_error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Archive {
    version: u32,
    connections: Vec<SavedConfig>,
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
            codex: None,
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
            desktop_error: desktop.as_ref().err().map(|e| format!("{e:#}")),
        });
        self
    }

    pub(super) fn with_codex(
        mut self,
        connection: &codex::Connection,
        desktop_only: bool,
        desktop: &Result<CodexDesktop>,
    ) -> Self {
        if let Ok(desktop) = desktop {
            // Reflect the custom alias used by Desktop, while retaining the
            // exact-ID SSH destination in the portable configuration block.
            self.ssh_config = self.ssh_config.replacen(
                &format!("Host {}\n", ssh_config::agent_alias(&self.agent_name)),
                &format!("Host {}\n", desktop.ssh_alias),
                1,
            );
        }
        self.codex = Some(CodexConfig {
            connection: connection.clone(),
            desktop_only,
            desktop: desktop.as_ref().ok().cloned(),
            desktop_error: desktop.as_ref().err().map(|e| format!("{e:#}")),
        });
        self
    }

    pub(super) fn save(&self) -> Result<()> {
        self.save_in(&dirs::home_dir().context("Unable to locate the home directory")?)
    }

    pub(super) fn require_desktop(&self) -> Result<()> {
        if let Some(error) = self.codex.as_ref().and_then(|c| c.desktop_error.as_ref()) {
            bail!("Codex backend is ready, but Desktop configuration failed: {error}");
        }
        Ok(())
    }

    fn save_in(&self, home: &Path) -> Result<()> {
        let directory = home.join(".railway");
        fs::create_dir_all(&directory)?;
        secure_config_dir(&directory)?;
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(directory.join(LOCK))?;
        lock.lock_exclusive()
            .context("Locking saved code configurations")?;
        let mut archive = load_archive(home)?;
        // Archive an existing v1 snapshot before replacing it, including writes
        // made by an older CLI since the last archive update.
        upsert(&mut archive.connections, self.clone());
        write_private(&directory.join(ARCHIVE), &archive)?;
        write_private(&directory.join(FILE), self)
    }

    fn load_in(home: &Path, selector: Option<&str>) -> Result<Self> {
        let archive = load_archive(home)?;
        let mut connections = archive.connections;
        if let Some(selector) = selector {
            // Exact IDs take priority over a coincidentally matching name.
            let by_id = connections.iter().any(|s| s.agent_id == selector);
            connections.retain(|s| {
                if by_id {
                    s.agent_id == selector
                } else {
                    s.agent_name == selector
                }
            });
            if connections.len() > 1 {
                connections.sort_by(|a, b| {
                    a.environment_id
                        .cmp(&b.environment_id)
                        .then(a.agent_id.cmp(&b.agent_id))
                });
                bail!(
                    "Multiple saved agents match {selector:?}. Use an agent ID:\n{}",
                    connections
                        .iter()
                        .map(|s| format!(
                            "  {} [id: {}, environment: {}]",
                            s.agent_name, s.agent_id, s.environment_id
                        ))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
        }
        connections.into_iter().max_by_key(|s| s.saved_at).with_context(|| match selector {
            Some(selector) => format!("No saved connection details for {selector:?}. Run setup or connect for that agent first (for example, railway code --codex connect <agent> or railway code --opencode connect <agent>)."),
            None => "No saved connection details yet. Run railway code to set up an agent, or railway code --codex connect <agent> / railway code --opencode connect <agent> to save an existing connection.".into(),
        })
    }

    /// One panel is shared by setup, reconnect, client exit/cancel and replay.
    pub(super) fn show(&self) -> Result<()> {
        print!("{}", self.render()?);
        Ok(())
    }

    fn render(&self) -> Result<String> {
        let divider = "─".repeat(64).cyan();
        let mut out = format!("\n{divider}\n");
        writeln!(
            out,
            "{} connection details for {}",
            self.harness, self.agent_name
        )?;
        if let Some(codex) = &self.codex {
            let c = &codex.connection;
            writeln!(
                out,
                "\n{}\n  Name:      {}\n  Server:    {}\n  Token:     {}\n  Directory: {}\n  Version:   {}",
                "Codex App Server Configuration:".bold(),
                self.agent_name,
                c.url,
                c.token,
                c.directory,
                c.version
            )?;
            if let Some(d) = &codex.desktop {
                writeln!(
                    out,
                    "\nCodex Desktop configured: {}\nSSH configuration written to {}",
                    d.project_label,
                    d.ssh_config_path.display()
                )?;
                if let Some(error) = &d.apply_error {
                    writeln!(
                        out,
                        "Automatic import could not be opened: {error}. Open Codex Desktop to apply the saved configuration."
                    )?;
                }
            } else if let Some(error) = &codex.desktop_error {
                writeln!(
                    out,
                    "\nCodex Desktop configuration failed: {error}\nThe backend connection above is available; rerun Desktop setup to retry."
                )?;
            }
        }
        if let Some(o) = &self.opencode {
            let c = &o.connection;
            let edition = if o.beta {
                "OpenCode2 [Beta]"
            } else {
                "OpenCode"
            };
            writeln!(
                out,
                "\n{}\n  Name:      {}\n  Server:    {}\n  Username:  {}\n  Password:  {}\n  Directory: {}",
                format!("Railway {edition} Server Configuration:").bold(),
                self.agent_name,
                c.url,
                c.username,
                c.password,
                c.directory
            )?;
            if o.desktop_configured {
                writeln!(
                    out,
                    "\n{edition} Desktop configuration updated (you may need to restart)."
                )?;
            } else if let Some(error) = &o.desktop_error {
                writeln!(out, "\nDesktop configuration was not saved: {error}")?;
            } else {
                writeln!(
                    out,
                    "\n{edition} Desktop was not detected; desktop configuration was skipped."
                )?;
            }
        }
        // Only SSH-based sessions need the SSH panel, in both creation and replay.
        let show_ssh = self.codex.is_none() && self.opencode.is_none();
        if show_ssh {
            writeln!(
                out,
                "\n{}\n  Agent:       {}\n  Agent ID:    {}\n  Environment: {}\n  Harness:     {}",
                "Railway Cloud Agent SSH Configuration:".bold(),
                self.agent_name,
                self.agent_id,
                self.environment_id,
                self.harness
            )?;
            writeln!(
                out,
                "\n{}\n  {}\n\n{}\n{}",
                "Connect with SSH:".bold(),
                self.ssh_command,
                "SSH config block (for ~/.ssh/config):".bold(),
                self.ssh_config
            )?;
        }
        writeln!(out, "\n{}", "Connect with the Railway CLI:".bold())?;
        if self.codex.is_some() || self.opencode.is_some() {
            writeln!(
                out,
                "  {}",
                shell_join(&[
                    "railway".into(),
                    "code".into(),
                    format!("--{}", self.harness),
                    "connect".into(),
                    self.agent_name.clone()
                ])
            )?;
        }
        if show_ssh {
            writeln!(
                out,
                "  {}",
                shell_join(&[
                    "railway".into(),
                    "ca".into(),
                    "ssh".into(),
                    self.agent_id.clone()
                ])
            )?;
        }
        writeln!(
            out,
            "\nShow these details again:\n  {}",
            shell_join(&[
                "railway".into(),
                "code".into(),
                "get-config".into(),
                self.agent_name.clone()
            ])
        )?;
        writeln!(
            out,
            "\nYou can close this terminal; the backend stays on Railway.\n{} stops compute when you're done.",
            shell_join(&[
                "railway".into(),
                "ca".into(),
                "sleep".into(),
                self.agent_name.clone()
            ])
        )?;
        writeln!(out, "{divider}\n")?;
        Ok(out)
    }
}

fn read_optional<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).with_context(|| {
            format!(
                "Invalid saved Railway code configuration in {}",
                path.display()
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("Reading saved Railway code configuration"),
    }
}

fn load_archive(home: &Path) -> Result<Archive> {
    let directory = home.join(".railway");
    let mut archive: Archive = read_optional(&directory.join(ARCHIVE))?.unwrap_or(Archive {
        version: VERSION,
        connections: vec![],
    });
    if archive.version != VERSION || archive.connections.iter().any(|s| s.version != VERSION) {
        bail!("Unsupported saved Railway code configuration version; upgrade the CLI");
    }
    if let Some(saved) = read_optional::<SavedConfig>(&directory.join(FILE))? {
        if saved.version != VERSION {
            bail!("Unsupported saved Railway code configuration version; upgrade the CLI");
        }
        upsert(&mut archive.connections, saved);
    }
    Ok(archive)
}

fn upsert(connections: &mut Vec<SavedConfig>, saved: SavedConfig) {
    if let Some(existing) = connections
        .iter_mut()
        .find(|s| s.agent_id == saved.agent_id && s.environment_id == saved.environment_id)
    {
        if saved.saved_at >= existing.saved_at {
            *existing = saved;
        }
    } else {
        connections.push(saved);
    }
}

fn write_private(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().context("Missing config directory")?)?;
    temporary.write_all(&serde_json::to_vec_pretty(value)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    Ok(())
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
    let saved = SavedConfig::load_in(&home, args.agent.as_deref())?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&saved)?);
        Ok(())
    } else {
        saved.show()
    }
}

pub(super) fn clear_in(home: &Path) {
    let directory = home.join(".railway");
    // Coordinate with in-flight writers without creating anything on logout.
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(directory.join(LOCK))
        .ok();
    if let Some(lock) = &lock {
        let _ = lock.lock_exclusive();
    }
    let _ = fs::remove_file(directory.join(FILE));
    let _ = fs::remove_file(directory.join(ARCHIVE));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(id: &str, name: &str) -> SavedConfig {
        SavedConfig::new(id, name, "env", "codex", None).unwrap()
    }

    fn codex(token: &str) -> codex::Connection {
        codex::Connection {
            url: "wss://example.up.railway.app:443".into(),
            token: token.into(),
            directory: "/app/my project".into(),
            version: "0.153.4".into(),
            reused: true,
        }
    }

    fn desktop() -> Result<CodexDesktop> {
        Ok(CodexDesktop {
            ssh_alias: "custom-alias".into(),
            ssh_config_path: "/home/user/custom ssh".into(),
            config_path: "/home/user/.codex/codex-app/config.json".into(),
            project_label: "My custom label".into(),
            remote_path: "/app/my project".into(),
            apply_url: "codex://codex-app/apply-config".into(),
            apply_sent: true,
            apply_error: None,
        })
    }

    #[test]
    fn archives_legacy_snapshots_and_refreshes_one_agent_without_losing_others() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(".railway");
        fs::create_dir(&directory).unwrap();
        let old = saved("old-id", "old-box");
        fs::write(directory.join(FILE), serde_json::to_vec(&old).unwrap()).unwrap();
        saved("new-id", "new-box")
            .with_codex(&codex("old-token"), true, &desktop())
            .save_in(home.path())
            .unwrap();
        saved("new-id", "renamed-box")
            .with_codex(&codex("new-token"), false, &desktop())
            .save_in(home.path())
            .unwrap();
        let loaded = SavedConfig::load_in(home.path(), None).unwrap();
        assert_eq!(loaded.agent_name, "renamed-box");
        assert_eq!(loaded.codex.unwrap().connection.token, "new-token");
        for selector in ["old-id", "old-box"] {
            assert_eq!(
                SavedConfig::load_in(home.path(), Some(selector))
                    .unwrap()
                    .agent_id,
                "old-id"
            );
        }
        assert!(SavedConfig::load_in(home.path(), Some("new-box")).is_err());
        assert_eq!(load_archive(home.path()).unwrap().connections.len(), 2);
        for file in [FILE, ARCHIVE] {
            let bytes = fs::read_to_string(directory.join(file)).unwrap();
            assert!(bytes.contains("new-token"));
            assert!(!bytes.contains("old-token"));
        }
        // A legacy CLI can still write the latest record after migration.
        let legacy_update = saved("legacy-id", "legacy-box");
        fs::write(
            directory.join(FILE),
            serde_json::to_vec(&legacy_update).unwrap(),
        )
        .unwrap();
        assert_eq!(
            SavedConfig::load_in(home.path(), None).unwrap().agent_id,
            "legacy-id"
        );
        saved("third-id", "third").save_in(home.path()).unwrap();
        assert_eq!(load_archive(home.path()).unwrap().connections.len(), 4);
    }

    #[test]
    fn duplicate_names_require_exact_id_and_selectors_are_never_paths() {
        let home = tempfile::tempdir().unwrap();
        saved("first-id", "box").save_in(home.path()).unwrap();
        let mut second = saved("second-id", "box");
        second.environment_id = "other-env".into();
        second.save_in(home.path()).unwrap();
        let error = SavedConfig::load_in(home.path(), Some("box"))
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("Multiple saved agents")
                && error.contains("first-id")
                && error.contains("second-id")
        );
        assert_eq!(
            SavedConfig::load_in(home.path(), Some("first-id"))
                .unwrap()
                .environment_id,
            "env"
        );
        saved("third-id", "first-id").save_in(home.path()).unwrap();
        assert_eq!(
            SavedConfig::load_in(home.path(), Some("first-id"))
                .unwrap()
                .agent_id,
            "first-id"
        );
        for selector in ["missing", "../../last-code-config.json", "/etc/passwd", ""] {
            assert!(SavedConfig::load_in(home.path(), Some(selector)).is_err());
        }
    }

    #[test]
    fn concurrent_saves_retain_all_agents_and_logout_removes_all_credentials() {
        let home = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for i in 0..8 {
                let home = home.path();
                scope.spawn(move || {
                    saved(&format!("id-{i}"), &format!("box-{i}"))
                        .with_codex(&codex("token"), false, &desktop())
                        .save_in(home)
                        .unwrap()
                });
            }
        });
        assert_eq!(load_archive(home.path()).unwrap().connections.len(), 8);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let directory = home.path().join(".railway");
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            for file in [FILE, ARCHIVE, LOCK] {
                assert_eq!(
                    fs::metadata(directory.join(file))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
        clear_in(home.path());
        clear_in(home.path());
        assert!(!home.path().join(".railway").join(FILE).exists());
        assert!(!home.path().join(".railway").join(ARCHIVE).exists());
        assert!(SavedConfig::load_in(home.path(), None).is_err());
    }

    #[test]
    fn corrupt_and_future_archives_are_not_overwritten() {
        for file in [FILE, ARCHIVE] {
            for contents in ["{", "null", r#"{"version":99,"connections":[]}"#] {
                let home = tempfile::tempdir().unwrap();
                let dir = home.path().join(".railway");
                fs::create_dir(&dir).unwrap();
                fs::write(dir.join(file), contents).unwrap();
                assert!(saved("id", "box").save_in(home.path()).is_err());
                assert_eq!(fs::read_to_string(dir.join(file)).unwrap(), contents);
            }
        }
    }

    #[test]
    fn codex_panel_replays_custom_desktop_settings_and_failed_import_with_usable_backend() {
        let snapshot = saved("id", "box").with_codex(&codex("token"), true, &desktop());
        let panel = snapshot.render().unwrap();
        assert_eq!(panel.matches(&"─".repeat(64)).count(), 2);
        assert_eq!(panel.matches("Codex App Server Configuration:").count(), 1);
        assert_eq!(panel.matches("Codex Desktop configured:").count(), 1);
        for expected in [
            "My custom label",
            "SSH configuration written to /home/user/custom ssh",
            "railway code --codex connect box",
            "railway code get-config box",
        ] {
            assert!(panel.contains(expected), "missing {expected}");
        }
        assert!(!panel.contains("Connect from your computer:"));
        let home = tempfile::tempdir().unwrap();
        let failed = saved("id", "box").with_codex(
            &codex("still-usable"),
            true,
            &Err(anyhow::anyhow!("SSH check failed")),
        );
        failed.save_in(home.path()).unwrap();
        let loaded = SavedConfig::load_in(home.path(), None).unwrap();
        assert!(loaded.require_desktop().is_err());
        let panel = loaded.render().unwrap();
        assert!(panel.contains("still-usable") && panel.contains("SSH check failed"));
        assert!(!panel.contains("Codex Desktop configured:"));
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
        assert!(quote_path(Path::new("/home/user\nProxyCommand bad")).is_err());
        assert!(SavedConfig::new("bad\nID", "box", "env", "codex", None).is_err());
    }
}
