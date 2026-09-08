//! OpenCode Desktop connects directly to the agent's existing HTTPS domain.
//! SSH is used only to start a detached, password-protected server on port
//! 8080. The VM keeps the credential and PID so reconnects are idempotent.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rand::RngCore;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::util::shell::shell_join;

const BOOTSTRAP: &str = include_str!("opencode.py");
const RESULT_PREFIX: &str = "RAILWAY_OPENCODE_CONNECTION=";

// Deliberately no Debug: this value contains the server password.
#[derive(Deserialize)]
pub(super) struct Connection {
    pub url: String,
    pub username: String,
    pub password: String,
    pub directory: String,
    pub reused: bool,
}

pub(super) fn generate_password() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn ssh_command(alias: &str, ssh_config: &Path) -> tokio::process::Command {
    let python = shell_join(&["python3".into(), "-c".into(), BOOTSTRAP.into()]);
    let remote = shell_join(&["bash".into(), "-lc".into(), python]);
    let mut command = tokio::process::Command::new("ssh");
    command
        .arg("-F")
        .arg(ssh_config)
        .args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=20"])
        .arg("--")
        .arg(alias)
        .arg(remote)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn bootstrap(alias: &str, ssh_config: &Path, request: serde_json::Value) -> Result<String> {
    let payload = serde_json::to_vec(&request)?;
    let mut child = ssh_command(alias, ssh_config)
        .spawn()
        .context("Failed to start OpenCode setup over SSH")?;
    let mut stdin = child.stdin.take().context("SSH stdin unavailable")?;
    stdin.write_all(&payload).await?;
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(90), child.wait_with_output())
        .await
        .context("OpenCode setup timed out. Rerun the command to check or finish setup.")??;
    if !output.status.success() {
        // Never echo stdout: it may contain the connection password if SSH
        // disconnects after the bootstrap has printed its result.
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!(
            "OpenCode setup failed ({}): {}",
            output.status,
            detail.trim()
        );
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix(RESULT_PREFIX))
        .map(str::to_owned)
        .context("SSH returned no OpenCode setup result")
}

pub(super) async fn start(
    alias: &str,
    directory: &str,
    ssh_config: &Path,
    password: &str,
) -> Result<Connection> {
    let response = bootstrap(
        alias,
        ssh_config,
        json!({ "directory": directory, "password": password }),
    )
    .await?;
    let connection: Connection =
        serde_json::from_str(&response).context("Invalid OpenCode connection result")?;
    // A fresh client carries no Railway API credentials. Never follow a
    // redirect with OpenCode's password or accept a plaintext public URL.
    validate_url(&connection.url)?;
    verify_connection(&connection).await?;
    Ok(connection)
}

pub(super) async fn stop(alias: &str, ssh_config: &Path) -> Result<()> {
    bootstrap(alias, ssh_config, json!({ "action": "stop" })).await?;
    Ok(())
}

fn validate_url(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).context("Invalid OpenCode public URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("OpenCode's public address must be an HTTPS origin");
    }
    Ok(url)
}

async fn verify_connection(connection: &Connection) -> Result<()> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?;
    let health = validate_url(&connection.url)?.join("global/health")?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let response = client
            .get(health.clone())
            .basic_auth(&connection.username, Some(&connection.password))
            .send()
            .await;
        if let Ok(response) = response {
            if response.status().is_redirection() {
                bail!(
                    "OpenCode's HTTPS address redirected unexpectedly; credentials were not forwarded"
                );
            }
            if response.status().is_success()
                && response
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .is_some_and(|body| body["healthy"] == true)
            {
                let unauthenticated = client.get(health.clone()).send().await?;
                if unauthenticated.status() != reqwest::StatusCode::UNAUTHORIZED {
                    bail!("OpenCode's public endpoint did not reject an unauthenticated request");
                }
                return Ok(());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "OpenCode started on the agent, but its HTTPS health check failed. Rerun setup to retry; check ~/.railway/desktop/opencode/server.log on the agent."
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_urls_require_https_and_no_embedded_credentials_or_paths() {
        for invalid in [
            "http://agent.up.railway.app",
            "https://user:password@agent.up.railway.app",
            "https://agent.up.railway.app/other",
            "https://agent.up.railway.app?token=secret",
        ] {
            assert!(validate_url(invalid).is_err());
        }
        assert!(validate_url("https://app-agent.up.railway.app").is_ok());
    }

    #[test]
    fn password_is_random_and_safe_as_a_boot_variable() {
        let first = generate_password();
        assert_eq!(first.len(), 43);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(first, generate_password());
    }

    #[test]
    fn ssh_bootstrap_passes_no_credentials_in_argv_and_does_not_allocate_a_tunnel() {
        let command = ssh_command("railway-agent-box", Path::new("/tmp/ssh config"));
        let args = command
            .as_std()
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(&args[..3], ["-F", "/tmp/ssh config", "-T"]);
        assert!(!args.iter().any(|arg| arg == "-L" || arg == "-tt"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_bootstrap_lifecycle() {
        // Exercise the shipped Python bootstrap against a fake OpenCode HTTP
        // server: detach, credential reuse, conflicts, auth, and PID ownership.
        let output = std::process::Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/opencode_desktop.py"
            ))
            .output()
            .expect("python3 is required for OpenCode bootstrap integration tests");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
