//! Find a compatible native Codex TUI, or install a private version-matched copy.
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use sha2::{Digest, Sha256};

use super::Connection;

/// Keep local MCP servers, plugins, and hooks out of the remote TUI. Scope its
/// persistent UI state to the backend, not the rotating token or loopback bridge.
pub(crate) fn client_home(connection: &Connection) -> Result<PathBuf> {
    let home = dirs::home_dir().context("Unable to get home directory")?;
    let path = client_home_at(&home, connection);
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&path)
        .with_context(|| format!("Creating remote Codex client home {}", path.display()))?;
    Ok(path)
}

fn client_home_at(home: &Path, connection: &Connection) -> PathBuf {
    let id = format!("{:x}", Sha256::digest(connection.url.as_bytes()));
    home.join(".railway/codex-client").join(&id[..16])
}

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

pub(crate) async fn ensure_client(version: &str) -> Result<PathBuf> {
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
        return Ok(binary);
    }
    let npm =
        which::which("npm").context("Install Node.js, then rerun connect to install Codex")?;
    install_client(&npm, &root, version).await
}

async fn install_client(npm: &Path, root: &Path, version: &str) -> Result<PathBuf> {
    validate_version(version)?;
    std::fs::create_dir_all(root)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".install.lock"))?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(240);
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(error) => {
                return Err(error).context("Waiting for another Codex client installation");
            }
        }
    }
    let installed =
        root.join("node_modules/.bin")
            .join(if cfg!(windows) { "codex.cmd" } else { "codex" });
    if compatible(&installed, version) {
        return Ok(installed);
    }
    let mut command = tokio::process::Command::new(npm);
    command
        .args(["install", "--prefix"])
        .arg(root)
        .args([
            "--no-audit",
            "--no-fund",
            &format!("@openai/codex@{version}"),
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(180), command.output())
        .await
        .context("Installing the matching Codex client timed out")?
        .context("Installing the official Codex package")?;
    if !output.status.success() || !compatible(&installed, version) {
        bail!(
            "Could not install a compatible Codex {version} client. The remote server is still running.\n{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_client_state_is_isolated_and_stable_across_token_rotation() {
        let root = tempfile::tempdir().unwrap();
        let mut connection = Connection {
            url: "wss://agent.example.com".into(),
            token: "token".into(),
            directory: "/app".into(),
            version: "0.153.4".into(),
            reused: true,
        };
        let home = client_home_at(root.path(), &connection);
        assert_ne!(home, root.path().join(".codex"));
        connection.token = "rotated-token".into();
        assert_eq!(client_home_at(root.path(), &connection), home);
        connection.url = "wss://another-agent.example.com".into();
        assert_ne!(client_home_at(root.path(), &connection), home);
    }

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
    #[tokio::test]
    async fn installs_matching_clients_without_prompts_and_serializes_concurrent_installs() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let npm = root.path().join("fake npm");
        std::fs::write(
            &npm,
            r#"#!/usr/bin/env python3
import pathlib, sys
assert sys.stdin.read() == ''
assert sys.argv[1:3] == ['install', '--prefix']
assert sys.argv[4:6] == ['--no-audit', '--no-fund']
assert sys.argv[6].startswith('@openai/codex@')
version = sys.argv[6].split('@')[-1]
root = pathlib.Path(sys.argv[3])
with (root / 'installs').open('a') as log: log.write(version + '\n')
binary = root / 'node_modules/.bin/codex'
binary.parent.mkdir(parents=True, exist_ok=True)
binary.write_text('#!/bin/sh\nif [ "$1" = "--version" ]; then echo "codex-cli ' + version + '"; else echo "--remote <URL> --remote-auth-token-env"; fi\n')
binary.chmod(0o700)
print('npm output is captured')
"#,
        )
        .unwrap();
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o700)).unwrap();
        let first = root.path().join("0.153.4");
        let (a, b) = tokio::join!(
            install_client(&npm, &first, "0.153.4"),
            install_client(&npm, &first, "0.153.4")
        );
        let binary = a.unwrap();
        assert_eq!(binary, b.unwrap());
        assert!(compatible(&binary, "0.153.4"));
        assert_eq!(
            std::fs::read_to_string(first.join("installs")).unwrap(),
            "0.153.4\n"
        );
        let next = root.path().join("0.154.0");
        let updated = install_client(&npm, &next, "0.154.0").await.unwrap();
        assert!(compatible(&updated, "0.154.0"));
        assert!(!compatible(&binary, "0.154.0"));
        std::fs::write(&npm, "#!/bin/sh\necho 'registry unavailable' >&2\nexit 1\n").unwrap();
        let error = install_client(&npm, &root.path().join("0.155.0"), "0.155.0")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("registry unavailable"));
        assert!(compatible(&updated, "0.154.0"));
    }
}
