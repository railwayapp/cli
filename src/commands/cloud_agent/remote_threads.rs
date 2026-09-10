//! Persisted Claude/Grok conversations discovered on the VM over one bounded SSH call.
use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result, bail};
use tokio::io::AsyncWriteExt;

use super::client_sessions::{self, Thread};
use crate::commands::{code, ssh::native};

const HELPER: &str = include_str!("remote_threads.py");
const PREFIX: &str = "RAILWAY-THREADS:";

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct RemoteThread {
    pub harness: String,
    pub thread: Thread,
    pub config_dir: String,
    #[serde(default)]
    pub active: bool,
    pub pane_id: Option<String>,
    pub console_name: Option<String>,
    pub background_id: Option<String>,
}

impl RemoteThread {
    pub fn name(&self, agent_id: &str) -> String {
        client_sessions::name(&self.harness, agent_id, Some(&self.thread.id))
    }

    pub fn resume_command(&self, pane_id: &str) -> Result<String> {
        client_sessions::validate_id(&self.thread.id)?;
        let variable = match self.harness.as_str() {
            "claude" => "CLAUDE_CONFIG_DIR",
            "grok" => "GROK_HOME",
            _ => bail!("Unsupported remote conversation harness"),
        };
        if !self.thread.directory.starts_with('/') || !self.config_dir.starts_with('/') {
            bail!("Conversation has no absolute working/config directory");
        }
        let invocation = match self
            .background_id
            .as_deref()
            .filter(|_| self.harness == "claude")
        {
            Some(id) => vec!["claude".into(), "attach".into(), id.into()],
            None => vec![
                self.harness.clone(),
                "--resume".into(),
                self.thread.id.clone(),
            ],
        };
        Ok(format!(
            "{}export RAILWAY_CODE_AUTOSTARTED=1; export RAILWAY_THREAD_PANE_ID={}; export {variable}={}; cd -- {} && exec {}",
            code::harness_env_prefix(),
            quote(pane_id),
            quote(&self.config_dir),
            quote(&self.thread.directory),
            crate::util::shell::shell_join(&invocation),
        ))
    }
}

fn quote(value: &str) -> String {
    crate::util::shell::shell_join(&[value.into()])
}

#[derive(Default, serde::Deserialize)]
pub(crate) struct Discovery {
    pub threads: Vec<RemoteThread>,
    pub warnings: Vec<String>,
    pub failed: Vec<String>,
}

pub(crate) async fn discover(info: &code::ConnectInfo) -> Result<Discovery> {
    let mut command = tokio::process::Command::new("ssh");
    command
        .args(native::relay_port_args())
        .args(&info.relay_opts);
    if let Some(identity) = &info.identity {
        command.arg("-i").arg(identity);
    }
    // Read-only metadata runs never request a durable console or a PTY.
    let remote = format!(
        "{}export RAILWAY_CODE_AUTOSTARTED=1 RAILWAY_THREAD_DISCOVERY=1; python3 -",
        code::harness_env_prefix()
    );
    let mut child = command
        .args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "--"])
        .arg(native::relay_destination(&info.ssh_target))
        .arg(remote)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Starting conversation discovery over SSH")?;
    let mut stdin = child.stdin.take().context("SSH stdin unavailable")?;
    stdin.write_all(HELPER.as_bytes()).await?;
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output())
        .await
        .context("Conversation discovery timed out")??;
    if !output.status.success() {
        bail!(
            "Conversation discovery failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = stdout
        .lines()
        .find_map(|line| line.strip_prefix(PREFIX))
        .context("SSH returned no conversation inventory")?;
    let mut result: Discovery =
        serde_json::from_str(json).context("Invalid conversation inventory")?;
    result.threads.retain(|row| {
        matches!(row.harness.as_str(), "claude" | "grok")
            && client_sessions::validate_id(&row.thread.id).is_ok()
    });
    Ok(result)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn thread(harness: &str, id: &str) -> RemoteThread {
        RemoteThread {
            harness: harness.into(),
            thread: Thread {
                id: id.into(),
                title: "Saved conversation".into(),
                directory: "/app/it's a project".into(),
                created_at: None,
                updated_at: "2026-09-10T10:00:00Z".into(),
                state: "idle".into(),
            },
            config_dir: "/home/user/custom config".into(),
            active: false,
            pane_id: None,
            console_name: None,
            background_id: None,
        }
    }

    #[test]
    fn resumes_exact_thread_in_original_directory_and_config() {
        for harness in ["claude", "grok"] {
            let thread = thread(harness, "saved-id");
            let command = thread.resume_command("pane-id").unwrap();
            let words = shlex::split(&command).unwrap();
            let cd = words.iter().position(|w| w == "cd").unwrap();
            assert_eq!(
                &words[cd..],
                &[
                    "cd",
                    "--",
                    "/app/it's a project",
                    "&&",
                    "exec",
                    harness,
                    "--resume",
                    "saved-id"
                ]
            );
            assert!(
                words
                    .iter()
                    .any(|w| w.ends_with("=/home/user/custom config;"))
            );
        }
        let mut thread = thread("claude", "saved-id");
        thread.background_id = Some("job-id".into());
        assert!(
            thread
                .resume_command("pane-id")
                .unwrap()
                .ends_with("exec claude attach job-id")
        );
        thread.thread.directory.clear();
        assert!(thread.resume_command("pane-id").is_err());
    }

    #[test]
    fn vm_metadata_discovery_fixtures() {
        let output = std::process::Command::new("python3")
            .arg("src/commands/cloud_agent/remote_threads_test.py")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .output()
            .expect("python3 is required for VM metadata integration tests");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
