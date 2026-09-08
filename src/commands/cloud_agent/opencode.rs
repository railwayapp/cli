//! OpenCode Desktop and local terminal clients connect directly to the agent's existing HTTPS domain.
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
pub(crate) struct Connection {
    pub url: String,
    pub username: String,
    pub password: String,
    pub directory: String,
    pub reused: bool,
}

pub(crate) fn generate_password() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn ssh_command(target: &str, options: &[String]) -> tokio::process::Command {
    let python = shell_join(&["python3".into(), "-c".into(), BOOTSTRAP.into()]);
    let remote = shell_join(&["bash".into(), "-lc".into(), python]);
    let mut command = tokio::process::Command::new("ssh");
    command
        .args(options)
        .args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=20"])
        .arg("--")
        .arg(target)
        .arg(remote)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn bootstrap(
    mut command: tokio::process::Command,
    request: serde_json::Value,
) -> Result<String> {
    let payload = serde_json::to_vec(&request)?;
    let mut child = command
        .spawn()
        .context("Failed to start OpenCode setup over SSH")?;
    let mut stdin = child.stdin.take().context("SSH stdin unavailable")?;
    stdin.write_all(&payload).await?;
    drop(stdin);
    let output = tokio::time::timeout(
        Duration::from_secs(if request["harness"] == "opencode2" {
            660
        } else {
            90
        }),
        child.wait_with_output(),
    )
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

pub(crate) async fn start(
    alias: &str,
    directory: &str,
    ssh_config: &Path,
    password: &str,
    beta: bool,
) -> Result<Connection> {
    start_with(
        ssh_command(
            alias,
            &["-F".into(), ssh_config.to_string_lossy().into_owned()],
        ),
        directory,
        password,
        beta,
    )
    .await
}

/// Use the launcher's verified relay and identity without modifying local SSH
/// or Desktop settings. The credentials still travel only over SSH stdin.
pub(crate) async fn start_prepared(
    prepared: &crate::commands::code::Prepared,
    directory: &str,
    password: &str,
    beta: bool,
) -> Result<Connection> {
    let mut options = prepared.relay_opts.clone();
    if let Some(identity) = &prepared.identity {
        options.push("-i".into());
        options.push(identity.to_string_lossy().into_owned());
    }
    let connection = start_with(
        ssh_command(&prepared.ssh_target, &options),
        directory,
        password,
        beta,
    )
    .await?;
    if beta {
        // Beta's positional directory performs a local chdir, even with
        // --server. Its remote client uses the server's default location.
        let location: serde_json::Value = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()?
            .get(format!("{}/api/location", connection.url))
            .basic_auth(&connection.username, Some(&connection.password))
            .send()
            .await
            .context("Checking OpenCode2's remote project directory")?
            .error_for_status()?
            .json()
            .await?;
        let actual = location["directory"]
            .as_str()
            .context("OpenCode2 returned no remote project directory")?;
        if actual != connection.directory {
            bail!(
                "OpenCode2 is already serving {actual}. Reconnect with --dir {actual}, or use --new to serve {} on a fresh agent.",
                connection.directory
            );
        }
    }
    Ok(connection)
}

async fn start_with(
    command: tokio::process::Command,
    directory: &str,
    password: &str,
    beta: bool,
) -> Result<Connection> {
    let response = bootstrap(
        command,
        json!({ "directory": directory, "password": password, "harness": if beta { "opencode2" } else { "opencode" } }),
    )
    .await?;
    let connection: Connection =
        serde_json::from_str(&response).context("Invalid OpenCode connection result")?;
    // A fresh client carries no Railway API credentials. Never follow a
    // redirect with OpenCode's password or accept a plaintext public URL.
    validate_url(&connection.url)?;
    verify_connection(&connection, beta).await?;
    Ok(connection)
}

pub(crate) async fn stop(alias: &str, ssh_config: &Path, beta: bool) -> Result<()> {
    bootstrap(
        ssh_command(
            alias,
            &["-F".into(), ssh_config.to_string_lossy().into_owned()],
        ),
        json!({ "action": "stop", "harness": if beta { "opencode2" } else { "opencode" } }),
    )
    .await?;
    Ok(())
}

/// Print credentials as environment variables, keeping them out of the URL.
/// Beta's packaged Desktop CLI is usable even without an opencode2 PATH entry.
pub(crate) fn attach_command(connection: &Connection, beta: bool) -> Result<String> {
    validate_url(&connection.url)?;
    let client = local_client(beta);
    Ok(format_attach_command(
        connection,
        beta,
        &client,
        cfg!(windows),
    ))
}

fn local_client(beta: bool) -> String {
    if !beta {
        return "opencode".into();
    }
    #[cfg(target_os = "macos")]
    if which::which("opencode2").is_err() {
        let mut applications = vec![std::path::PathBuf::from("/Applications")];
        if let Some(home) = dirs::home_dir() {
            applications.push(home.join("Applications"));
        }
        for root in applications {
            let binary = root.join("OpenCode Beta.app/Contents/Resources/opencode-cli");
            if binary.is_file() {
                return binary.to_string_lossy().into_owned();
            }
        }
    }
    "opencode2".into()
}

fn format_attach_command(
    connection: &Connection,
    beta: bool,
    client: &str,
    powershell: bool,
) -> String {
    let args = if beta {
        vec![client.into(), "--server".into(), connection.url.clone()]
    } else {
        vec![
            client.into(),
            "attach".into(),
            connection.url.clone(),
            "--dir".into(),
            connection.directory.clone(),
        ]
    };
    if powershell {
        let quote = |value: &str| format!("'{}'", value.replace('\'', "''"));
        return format!(
            "$env:OPENCODE_SERVER_USERNAME = {}; $env:OPENCODE_SERVER_PASSWORD = {}; & {}",
            quote(&connection.username),
            quote(&connection.password),
            args.iter()
                .map(|arg| quote(arg))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    format!(
        "OPENCODE_SERVER_USERNAME={} OPENCODE_SERVER_PASSWORD={} {}",
        shell_join(std::slice::from_ref(&connection.username)),
        shell_join(std::slice::from_ref(&connection.password)),
        shell_join(&args)
    )
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

async fn verify_connection(connection: &Connection, beta: bool) -> Result<()> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?;
    let health =
        validate_url(&connection.url)?.join(if beta { "api/health" } else { "global/health" })?;
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
                    .is_some_and(|body| {
                        body["healthy"] == true && (!beta || body["version"].is_string())
                    })
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

    fn connection() -> Connection {
        Connection {
            url: "https://app-box.up.railway.app".into(),
            username: "opencode".into(),
            password: "test-password".into(),
            directory: "/app".into(),
            reused: false,
        }
    }

    #[test]
    fn attach_commands_use_the_matching_client_protocol_and_auth() {
        let connection = connection();
        let standard = format_attach_command(&connection, false, "opencode", false);
        assert_eq!(
            standard,
            "OPENCODE_SERVER_USERNAME=opencode OPENCODE_SERVER_PASSWORD=test-password opencode attach https://app-box.up.railway.app --dir /app"
        );
        let beta = format_attach_command(&connection, true, "opencode2", false);
        assert_eq!(
            beta,
            "OPENCODE_SERVER_USERNAME=opencode OPENCODE_SERVER_PASSWORD=test-password opencode2 --server https://app-box.up.railway.app"
        );
        assert!(!beta.contains("test-password@"));
    }

    #[test]
    fn powershell_attach_quotes_passwords_and_client_paths() {
        let mut connection = connection();
        connection.password = "pass'word$".into();
        let command =
            format_attach_command(&connection, true, "C:/OpenCode Beta/opencode2.exe", true);
        assert!(command.contains("$env:OPENCODE_SERVER_PASSWORD = 'pass''word$'"));
        assert!(command.contains("& 'C:/OpenCode Beta/opencode2.exe' '--server'"));
    }

    #[cfg(unix)]
    #[test]
    fn attach_command_preserves_quoted_credentials_and_remote_directory() {
        let root = tempfile::tempdir().unwrap();
        let client = root.path().join("local client");
        std::fs::write(&client, "#!/bin/sh\nprintf '%s\\n' \"$OPENCODE_SERVER_USERNAME\" \"$OPENCODE_SERVER_PASSWORD\" \"$@\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut connection = connection();
        connection.password = "pass'word $(touch INJECTED)".into();
        connection.directory = "/app/a project' $(touch INJECTED)".into();
        for beta in [false, true] {
            let command = format_attach_command(&connection, beta, client.to_str().unwrap(), false);
            let result = std::process::Command::new("sh")
                .args(["-c", &command])
                .current_dir(root.path())
                .output()
                .unwrap();
            assert!(result.status.success());
            let output = String::from_utf8(result.stdout).unwrap();
            let lines = output.lines().collect::<Vec<_>>();
            assert_eq!(lines[0], connection.username);
            assert_eq!(lines[1], connection.password);
            if beta {
                assert_eq!(&lines[2..], ["--server", connection.url.as_str()]);
            } else {
                assert_eq!(lines.last().unwrap(), &connection.directory);
            }
            assert!(!root.path().join("INJECTED").exists());
        }
    }

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
        let command = ssh_command(
            "railway-agent-box",
            &["-F".into(), "/tmp/ssh config".into()],
        );
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
