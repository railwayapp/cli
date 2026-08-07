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
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "SSH command failed (exit {}): {}",
            output.status,
            stderr.trim()
        );
    }

    Ok(String::from_utf8(output.stdout)?)
}
