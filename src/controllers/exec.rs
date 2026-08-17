//! Shared primitive for running a command inside a service's container over
//! native SSH (`ssh <instanceId>@ssh.railway.com`). Originally private and
//! postgres-stats-specific (`db_stats::exec_command_in_container`); promoted
//! here so `railway postgres {pitr,ha,pgbouncer}`'s live-probe commands
//! (pgBackRest info, Patroni's REST API, PgBouncer's `SHOW POOLS`) can reuse
//! it in addition to `railway metrics`'s database stats collection.

use std::process::Stdio;

use anyhow::{Result, bail};
use tokio::io::AsyncWriteExt;

const SSH_HOST: &str = "ssh.railway.com";

/// Execute a shell command inside a service container via native SSH and
/// capture stdout. Callers are expected to have already run
/// `crate::controllers::ssh::keys::find_local_ssh_keys`/`ensure_ssh_key` so a
/// registered key is available; this function does no preflighting itself.
pub(crate) async fn exec_in_container(instance_id: &str, command: &str) -> Result<String> {
    let target = format!("{instance_id}@{SSH_HOST}");

    let mut child = tokio::process::Command::new("ssh")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg(&target)
        .arg("sh")
        .arg("-s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(command.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
    } else {
        bail!("Failed to open stdin for SSH command");
    }

    let output = child.wait_with_output().await?;

    if !output.status.success() {
        // `ExitStatus`'s Display already reads "exit code: N" / "signal: N",
        // so no "exit" prefix of our own (it used to render doubled, as
        // "SSH command failed (exit exit code: 31)").
        let status = match output.status.code() {
            Some(code) => format!("exit code {code}"),
            None => output.status.to_string(),
        };
        // Some in-container tools log their errors to STDOUT, not stderr
        // (pgbackrest's console log, for one) — fall back to a stdout tail
        // rather than reporting an empty message.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            // Prefer the actual error line over whatever printed last —
            // pgbackrest follows its ERROR line with HINT lines.
            stdout
                .lines()
                .rev()
                .find(|line| line.contains("ERROR") || line.contains("FATAL"))
                .or_else(|| stdout.lines().rev().find(|line| !line.trim().is_empty()))
                .unwrap_or("")
                .trim()
                .to_string()
        } else {
            stderr.trim().to_string()
        };
        bail!("SSH command failed ({status}): {detail}");
    }

    Ok(String::from_utf8(output.stdout)?)
}
