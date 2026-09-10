//! Find a compatible native Codex TUI, or install a private version-matched copy.
use std::{
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::{Connection, TOKEN_ENV, attach_args};
use crate::commands::cloud_agent::opencode::local::confirm;

pub(super) fn validate_version(version: &str) -> Result<()> {
    if !regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[a-zA-Z0-9.-]+)?$")?.is_match(version) {
        bail!("Unsupported Codex version from the server");
    }
    Ok(())
}

fn compatible(binary: &Path, version: &str) -> bool {
    let Ok(output) = Command::new(binary).arg("--version").output() else {
        return false;
    };
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout).trim() != format!("codex-cli {version}")
    {
        return false;
    }
    let Ok(output) = Command::new(binary).arg("--help").output() else {
        return false;
    };
    let help = String::from_utf8_lossy(&output.stdout);
    output.status.success()
        && help.contains("--remote <")
        && help.contains("--remote-auth-token-env")
}

pub(crate) async fn ensure_client(version: &str) -> Result<Option<PathBuf>> {
    validate_version(version)?;
    let home = dirs::home_dir().context("Unable to get home directory")?;
    let root = home.join(".railway/runtimes/codex-client").join(version);
    let installed =
        root.join("node_modules/.bin")
            .join(if cfg!(windows) { "codex.cmd" } else { "codex" });
    let mut candidates = vec![installed.clone()];
    if let Ok(binary) = which::which("codex") {
        candidates.push(binary);
    }
    candidates.push(home.join(".local/bin/codex"));
    #[cfg(target_os = "macos")]
    candidates.push(PathBuf::from(
        "/Applications/Codex.app/Contents/Resources/codex",
    ));
    if let Some(binary) = candidates
        .into_iter()
        .find(|path| compatible(path, version))
    {
        return Ok(Some(binary));
    }
    if !confirm(&format!(
        "Install Codex {version} locally to match this server? (Private Railway runtime)"
    ))? {
        return Ok(None);
    }
    let npm =
        which::which("npm").context("Install Node.js, then rerun connect to install Codex")?;
    let status = tokio::process::Command::new(npm)
        .args(["install", "--prefix"])
        .arg(&root)
        .args([
            "--no-audit",
            "--no-fund",
            &format!("@openai/codex@{version}"),
        ])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("Installing the official Codex package")?;
    if !status.success() || !compatible(&installed, version) {
        bail!(
            "Could not install a compatible Codex {version} client. The remote server is still running."
        );
    }
    Ok(Some(installed))
}

/// Do not mix the local harness's MCP servers, plugins, and hooks into the remote
/// TUI. Their startup state can keep Codex busy even after the remote tools are
/// ready, trapping Ctrl+C in interrupt handling. Retain the client's UI state
/// between attaches, scoped to the backend rather than its rotating token.
pub(super) fn client_home(connection: &Connection) -> Result<PathBuf> {
    let home = dirs::home_dir().context("Unable to get home directory")?;
    Ok(client_home_at(&home, connection))
}

fn client_home_at(home: &Path, connection: &Connection) -> PathBuf {
    let id = format!("{:x}", Sha256::digest(connection.url.as_bytes()));
    home.join(".railway/codex-client").join(&id[..16])
}

fn client_command(binary: &Path, connection: &Connection, home: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .args(attach_args(connection))
        .env(TOKEN_ENV, &connection.token)
        .env("CODEX_HOME", home)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

pub(crate) fn run_client(binary: &Path, connection: &Connection) -> Result<ExitStatus> {
    let home = client_home(connection)?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&home)
        .with_context(|| format!("Creating remote Codex client home {}", home.display()))?;
    client_command(binary, connection, &home)
        .status()
        .with_context(|| format!("Could not launch local Codex client {}", binary.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_cannot_inject_package_specs_or_cache_paths() {
        for bad in [
            "latest",
            "../0.1.0",
            "0.1.0/../../other",
            "0.1.0 --global",
            "https://example.com/pkg",
        ] {
            assert!(validate_version(bad).is_err());
        }
        for good in ["0.153.4", "0.154.0-alpha.1"] {
            assert!(validate_version(good).is_ok());
        }
    }

    #[cfg(unix)]
    #[test]
    fn local_client_receives_remote_directory_and_token_only_in_environment() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("fake codex");
        std::fs::write(
            &binary,
            "#!/bin/sh\nprintf '%s\\n' \"$RAILWAY_CODEX_SERVER_TOKEN\" \"$CODEX_HOME\" \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let connection = Connection {
            url: "wss://agent.example.com".into(),
            token: "secret ' $(echo injected)".into(),
            directory: "/app/a project".into(),
            version: "0.153.4".into(),
            reused: true,
        };
        let home = client_home_at(root.path(), &connection);
        let output = client_command(&binary, &connection, &home)
            .stdout(Stdio::piped())
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = String::from_utf8(output.stdout).unwrap();
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines[0], connection.token);
        assert_eq!(Path::new(lines[1]), home);
        assert_ne!(home, root.path().join(".codex"));
        assert_eq!(lines[2..], attach_args(&connection));
        assert!(!attach_args(&connection).contains(&connection.token));
        let mut reconnected = connection.clone();
        reconnected.token = "rotated-token".into();
        assert_eq!(client_home_at(root.path(), &reconnected), home);
        reconnected.url = "wss://another-agent.example.com".into();
        assert_ne!(client_home_at(root.path(), &reconnected), home);
    }
}
