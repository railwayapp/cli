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

/// Transport-level failure classifier for the retrying probe wrapper: true
/// only when a second attempt can plausibly answer differently (the SSH
/// relay dropped the connection, a handshake timed out, the network
/// blipped). Deterministic failures — auth, missing binaries, host key
/// refusals — retry into the same wall and only add latency.
fn is_transient_exec_error(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    const DETERMINISTIC: &[&str] = &[
        "permission denied",
        "publickey",
        "host key verification failed",
        "command not found",
        "no such file",
        "too many authentication failures",
    ];
    if DETERMINISTIC.iter().any(|m| lower.contains(m)) {
        return false;
    }
    const TRANSIENT: &[&str] = &[
        "connection reset",
        "connection refused",
        "connection closed",
        "connection timed out",
        "timed out",
        "timeout",
        "broken pipe",
        "network is unreachable",
        "temporarily unavailable",
        "kex_exchange",
        "banner exchange",
        "unexpected eof",
        "connection to",
    ];
    TRANSIENT.iter().any(|m| lower.contains(m))
}

/// How many attempts a read probe gets, and the pauses between them. Small
/// on purpose: probes back live `status`-class commands, and three attempts
/// with short pauses distinguishes a blip from an outage without turning a
/// dead member into a half-minute hang.
const PROBE_ATTEMPTS: u32 = 3;
const PROBE_BACKOFF: [std::time::Duration; 2] = [
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(3),
];

/// [`exec_in_container`] for READ probes: bounded retry on transient
/// transport failures, with the per-attempt timeout applied INSIDE the loop
/// (an outer timeout would kill the whole retry chain on the first slow
/// attempt). An elapsed attempt counts as transient. NEVER route a mutation
/// through this — a timeout after the remote side already acted would
/// replay it (Patroni's `POST /switchover` stays on `exec_in_container`).
pub(crate) async fn exec_probe_in_container(
    instance_id: &str,
    command: &str,
    per_attempt_timeout: std::time::Duration,
) -> Result<String> {
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 1..=PROBE_ATTEMPTS {
        match tokio::time::timeout(per_attempt_timeout, exec_in_container(instance_id, command))
            .await
        {
            Ok(Ok(output)) => return Ok(output),
            Ok(Err(err)) => {
                let transient = is_transient_exec_error(&format!("{err:#}"));
                last_error = Some(err);
                if !transient {
                    break;
                }
            }
            Err(_elapsed) => {
                last_error = Some(anyhow::anyhow!(
                    "probe timed out after {per_attempt_timeout:?}"
                ));
            }
        }
        if attempt < PROBE_ATTEMPTS {
            tokio::time::sleep(PROBE_BACKOFF[(attempt - 1) as usize]).await;
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("probe failed with no attempts made")))
}

#[cfg(test)]
mod tests {
    use super::is_transient_exec_error;

    #[test]
    fn transient_and_deterministic_exec_errors_are_told_apart() {
        // Worth another attempt: the transport blinked.
        assert!(is_transient_exec_error(
            "SSH command failed (exit code 255): Connection reset by peer"
        ));
        assert!(is_transient_exec_error(
            "SSH command failed (exit code 255): kex_exchange_identification: read: Connection reset"
        ));
        assert!(is_transient_exec_error("probe timed out after 5s"));
        assert!(is_transient_exec_error(
            "SSH command failed (exit code 255): Connection to ssh.railway.com closed by remote host"
        ));
        // Not worth another attempt: the answer will not change.
        assert!(!is_transient_exec_error(
            "SSH command failed (exit code 255): paulo@ssh.railway.com: Permission denied (publickey)"
        ));
        assert!(!is_transient_exec_error(
            "SSH command failed (exit code 127): sh: curl: command not found"
        ));
        assert!(!is_transient_exec_error(
            "SSH command failed (exit code 255): Host key verification failed"
        ));
    }
}
