//! OpenCode Desktop and local terminal clients connect directly to the agent's code HTTPS domain.
//! SSH is used only to start a detached, password-protected server on port
//! configured at creation (default 4096; 8080 on legacy agents). The VM keeps
//! the port, credential and PID so reconnects are idempotent.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use colored::Colorize;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::util::shell::shell_join;

pub(crate) mod auth;
pub(crate) mod bridge;
pub(crate) mod local;
mod protocol;
pub(crate) use protocol::Protocol;

const BOOTSTRAP: &str = include_str!("opencode.py");
const IMPORT_AUTH: &str = include_str!("opencode/import_auth.py");
const RESULT_PREFIX: &str = "RAILWAY_OPENCODE_CONNECTION=";

/// The official V2 installer, which places `opencode` at `~/.opencode/bin`,
/// the same location the cloud-agent image installs it to.
pub(crate) const V2_INSTALLER: &str = "https://opencode.ai/v2/install";

/// Runtime seed for terminal launches: the image's own `opencode` is the V2
/// runtime, so nothing is downloaded on a current image. On an older image
/// whose `opencode` is 1.x the official installer upgrades it in place (same
/// path the image used), because a saved V1 session cannot be resumed by the
/// CLI any more and a dead end helps nobody. Then the shim earlier releases
/// seeded as `~/.local/bin/opencode2` is retired and staged provider
/// credentials are imported into V2's store.
///
/// The installer script is fetched into a variable before it runs so a failed
/// download is reported as such rather than executed half-way, and it never
/// reads this script's stdin, which may still carry the skills payload.
pub(crate) fn seed_script() -> String {
    format!(
        r#"mkdir -p ~/.railway/runtimes/opencode
if [ -e ~/.local/bin/opencode2 ] && grep -q 'railway/runtimes/opencode2' ~/.local/bin/opencode2 2>/dev/null; then rm -f ~/.local/bin/opencode2; fi
rm -rf ~/.railway/runtimes/opencode2
railway_opencode_v2() {{ case "$(opencode --version 2>/dev/null </dev/null)" in 2.*|'opencode v2.'*) return 0;; *) return 1;; esac; }}
if ! railway_opencode_v2; then
  printf '%s\n' 'This agent image ships an older OpenCode; installing OpenCode V2 from opencode.ai...' >&2
  if ! opencode_installer=$(curl -fsSL {V2_INSTALLER}) || [ -z "$opencode_installer" ]; then
    printf '%s\n' 'Could not download the OpenCode V2 installer on this agent. Create a fresh agent with railway code --opencode --new, or run on the agent: curl -fsSL {V2_INSTALLER} | bash' >&2
    exit 1
  fi
  if ! bash -c "$opencode_installer" install --no-modify-path </dev/null >&2; then
    printf '%s\n' 'Could not install OpenCode V2 on this agent. Create a fresh agent with railway code --opencode --new, or run on the agent: curl -fsSL {V2_INSTALLER} | bash' >&2
    exit 1
  fi
  hash -r 2>/dev/null || true
  if ! railway_opencode_v2; then
    printf '%s\n' 'OpenCode V2 was installed, but an older opencode is still first on PATH. Create a fresh agent with railway code --opencode --new.' >&2
    exit 1
  fi
fi
printf '%s' {importer} > ~/.railway/runtimes/opencode/import_auth.py
chmod 700 ~/.railway/runtimes/opencode/import_auth.py
python3 ~/.railway/runtimes/opencode/import_auth.py || exit 1"#,
        importer = shell_join(&[IMPORT_AUTH.to_string()])
    )
}

// Deliberately no Debug: this value contains the server password.
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Connection {
    pub url: String,
    pub username: String,
    pub password: String,
    pub directory: String,
    pub reused: bool,
    #[serde(default)]
    pub protocol: Protocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Shared connection details for Desktop setup and local-client commands.
/// Keep commands free of border prefixes so they can be copied directly.
pub(crate) fn show_connection(
    connection: &Connection,
    name: &str,
    desktop_configured: bool,
) -> Result<()> {
    let edition = connection.protocol.label();
    let divider = "─".repeat(64).cyan();
    println!("\n{divider}");
    println!("{}", format!("{edition} server on {name}").cyan().bold());
    show_server_config(connection, name);
    println!("\n{}", "Connect with the Railway CLI:".bold());
    println!("  {}", railway_connect_command(name));
    if desktop_configured {
        println!("\nOpenCode Desktop configuration updated (you may need to restart)");
    }
    println!("{divider}\n");
    Ok(())
}

/// The server fields, without a surrounding panel or connection commands.
pub(crate) fn show_server_config(connection: &Connection, name: &str) {
    let edition = connection.protocol.label();
    println!(
        "\n{}",
        format!("Railway {edition} Server Configuration:").bold()
    );
    println!("  {}      {name}", "Name:".bold());
    println!("  {}    {}", "Server:".bold(), connection.url);
    println!("  {}  {}", "Username:".bold(), connection.username);
    println!("  {}  {}", "Password:".bold(), connection.password);
    println!("  {} {}", "Directory:".bold(), connection.directory);
}

pub(crate) fn railway_connect_command(name: &str) -> String {
    shell_join(&[
        "railway".into(),
        "code".into(),
        "--opencode".into(),
        "connect".into(),
        name.into(),
    ])
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
        Duration::from_secs(if request["action"] == "inspect" {
            15
        } else if request["protocol"] == "v2"
            || matches!(request["action"].as_str(), Some("connect" | "upgrade"))
        {
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
) -> Result<Connection> {
    start_with(
        ssh_command(
            alias,
            &["-F".into(), ssh_config.to_string_lossy().into_owned()],
        ),
        directory,
        password,
    )
    .await
}

/// Use the launcher's verified relay and identity without modifying local SSH
/// or Desktop settings. The credentials still travel only over SSH stdin.
pub(crate) async fn start_prepared(
    prepared: &crate::commands::code::Prepared,
    directory: &str,
    password: &str,
) -> Result<Connection> {
    let info = crate::commands::code::ConnectInfo {
        ssh_target: prepared.ssh_target.clone(),
        identity: prepared.identity.clone(),
        relay_opts: prepared.relay_opts.clone(),
    };
    let connection = start_with(relay_command(&info), directory, password).await?;
    verify_client_directory(&connection).await?;
    Ok(connection)
}

async fn verify_client_directory(connection: &Connection) -> Result<()> {
    if connection.protocol.is_v2() {
        // V2's positional directory performs a local chdir, even with
        // --server. Its remote client uses the server's default location.
        let location: serde_json::Value = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()?
            .get(format!("{}/api/location", connection.url))
            .basic_auth(&connection.username, Some(&connection.password))
            .send()
            .await
            .context("Checking OpenCode's remote project directory")?
            .error_for_status()?
            .json()
            .await?;
        let actual = location["directory"]
            .as_str()
            .context("OpenCode returned no remote project directory")?;
        if actual != connection.directory {
            bail!(
                "OpenCode is already serving {actual}. Reconnect with --dir {actual}, or use --new to serve {} on a fresh agent.",
                connection.directory
            );
        }
    }
    Ok(())
}

async fn start_with(
    command: tokio::process::Command,
    directory: &str,
    password: &str,
) -> Result<Connection> {
    let request = json!({ "directory": directory, "password": password, "protocol": Protocol::V2 });
    let response = bootstrap(command, request).await?;
    let connection: Connection =
        serde_json::from_str(&response).context("Invalid OpenCode connection result")?;
    // A fresh client carries no Railway API credentials. Never follow a
    // redirect with OpenCode's password or accept a plaintext public URL.
    validate_url(&connection.url)?;
    verify_connection(&connection).await?;
    Ok(connection)
}

pub(crate) async fn stop(alias: &str, ssh_config: &Path) -> Result<()> {
    bootstrap(
        ssh_command(
            alias,
            &["-F".into(), ssh_config.to_string_lossy().into_owned()],
        ),
        json!({ "action": "stop" }),
    )
    .await?;
    Ok(())
}

/// Arguments passed to the local client when Railway launches it.
pub(crate) fn attach_args(connection: &Connection) -> Vec<String> {
    if connection.protocol.is_v2() {
        vec!["--server".into(), connection.url.clone(), "--auto".into()]
    } else {
        vec![
            "attach".into(),
            connection.url.clone(),
            "--dir".into(),
            connection.directory.clone(),
        ]
    }
}

#[derive(Deserialize)]
pub(crate) struct ServerInfo {
    pub directory: String,
    pub protocol: Protocol,
}

fn relay_command(info: &crate::commands::code::ConnectInfo) -> tokio::process::Command {
    let mut options = info.relay_opts.clone();
    if let Some(identity) = &info.identity {
        options.extend(["-i".into(), identity.to_string_lossy().into_owned()]);
    }
    options.extend(crate::commands::ssh::native::relay_port_args());
    ssh_command(
        &crate::commands::ssh::native::relay_destination(&info.ssh_target),
        &options,
    )
}

pub(crate) async fn inspect(
    info: &crate::commands::code::ConnectInfo,
) -> Result<Option<ServerInfo>> {
    let response = bootstrap(relay_command(info), json!({"action": "inspect"})).await?;
    serde_json::from_str(&response).context("Invalid OpenCode discovery result")
}

pub(crate) async fn reconnect(info: &crate::commands::code::ConnectInfo) -> Result<Connection> {
    reconnect_with(info, "connect").await
}

pub(crate) async fn upgrade(info: &crate::commands::code::ConnectInfo) -> Result<Connection> {
    reconnect_with(info, "upgrade").await
}

async fn reconnect_with(
    info: &crate::commands::code::ConnectInfo,
    action: &str,
) -> Result<Connection> {
    // The VM owns the protocol. Reconnect must not change it to suit whichever
    // client happens to be installed on this machine; only `upgrade` migrates.
    let request = json!({"action": action});
    let response = bootstrap(relay_command(info), request).await?;
    let connection: Connection =
        serde_json::from_str(&response).context("Invalid OpenCode connection result")?;
    validate_url(&connection.url)?;
    verify_connection(&connection).await?;
    verify_client_directory(&connection).await?;
    Ok(connection)
}

pub(super) fn validate_url(value: &str) -> Result<url::Url> {
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
    let origin = validate_url(&connection.url)?;
    let paths: &[&str] = if connection.protocol.is_v2() {
        &["api/info", "api/status", "api/health"]
    } else {
        &["global/health"]
    };
    let mut endpoint = 0;
    let mut health = origin.join(paths[endpoint])?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let response = client
            .get(health.clone())
            .basic_auth(&connection.username, Some(&connection.password))
            .send()
            .await;
        if let Ok(response) = response {
            if response.status() == reqwest::StatusCode::NOT_FOUND && endpoint + 1 < paths.len() {
                endpoint += 1;
                health = origin.join(paths[endpoint])?;
                continue;
            }
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
                        if connection.protocol.is_v2() {
                            body["version"].is_string()
                                && ((body["pid"].is_u64() && body["urls"].is_array())
                                    || body["healthy"] == true)
                        } else {
                            body["healthy"] == true
                        }
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
        // A not-yet-published route can transiently return 404 too. Start
        // the next probe at the current endpoint instead of pinning retries
        // to a legacy path that this server may never expose.
        endpoint = 0;
        health = origin.join(paths[endpoint])?;
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
            protocol: Protocol::V1,
            version: None,
        }
    }

    #[test]
    fn attach_arguments_use_the_matching_client_protocol() {
        let mut connection = connection();
        assert_eq!(
            attach_args(&connection),
            ["attach", "https://app-box.up.railway.app", "--dir", "/app"]
        );
        connection.protocol = Protocol::V2;
        assert_eq!(
            attach_args(&connection),
            ["--server", "https://app-box.up.railway.app", "--auto"]
        );
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
    fn discovery_uses_the_relay_host_even_without_a_prepared_ssh_master() {
        let info = crate::commands::code::ConnectInfo {
            ssh_target: "agent:env:box".into(),
            identity: None,
            relay_opts: vec![],
        };
        let command = relay_command(&info);
        let args = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let destination = crate::commands::ssh::native::relay_destination(&info.ssh_target);
        assert!(args.contains(&destination));
        assert!(!args.contains(&info.ssh_target));
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
    fn runtime_seed_upgrades_only_old_images_and_imports_staged_credentials() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let script = seed_script();
        // The installer is fetched only behind the version check, never
        // unconditionally, and the credential importer always runs.
        assert!(script.find("opencode --version").unwrap() < script.find(V2_INSTALLER).unwrap());
        assert!(
            script.contains("bash -c \"$opencode_installer\" install --no-modify-path </dev/null")
        );
        assert!(script.contains("railway/runtimes/opencode2"));
        assert!(script.ends_with("python3 ~/.railway/runtimes/opencode/import_auth.py || exit 1"));
        let mut child = Command::new("sh")
            .arg("-n")
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // The embedded importer is the shipped file, byte for byte.
        let root = tempfile::tempdir().unwrap();
        // The quoted importer spans many lines: take from its printf up to
        // the chmod that follows it.
        let start = script.find("printf '%s' ").unwrap();
        let end = script
            .find("\nchmod 700 ~/.railway/runtimes/opencode/import_auth.py")
            .unwrap();
        // Only the redirect target moves; the importer's own text mentions
        // `~/` too and must arrive untouched.
        let write = script[start..end].replace(
            "> ~/.railway/runtimes/opencode/import_auth.py",
            &format!(
                "> {}/.railway/runtimes/opencode/import_auth.py",
                root.path().display()
            ),
        );
        std::fs::create_dir_all(root.path().join(".railway/runtimes/opencode")).unwrap();
        assert!(
            Command::new("sh")
                .args(["-c", &write])
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(
            std::fs::read_to_string(
                root.path()
                    .join(".railway/runtimes/opencode/import_auth.py")
            )
            .unwrap(),
            IMPORT_AUTH
        );
    }

    #[cfg(unix)]
    #[test]
    fn staged_credentials_merge_without_overwriting_remote_signins() {
        let output = std::process::Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/opencode_import_auth.py"
            ))
            .output()
            .expect("python3 is required for OpenCode credential tests");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
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
