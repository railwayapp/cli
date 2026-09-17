//! Find/install the selected local client, then hand it the user's terminal.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha512};

#[cfg(test)]
use super::Connection;
use crate::config::Configs;
#[cfg(test)]
use std::process::Command;

pub(crate) fn confirm(message: &str) -> Result<bool> {
    match inquire::Confirm::new(message)
        .with_default(true)
        .with_help_message("Enter to continue; Esc or Ctrl+C to leave the server running")
        .with_render_config(Configs::get_render_config())
        .prompt()
    {
        Ok(answer) => Ok(answer),
        Err(
            inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted,
        ) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn runtime_root(home: &Path) -> PathBuf {
    home.join(".railway/runtimes/opencode2-client")
}

fn client_paths(home: &Path, beta: bool) -> Vec<PathBuf> {
    let name = if beta { "opencode2" } else { "opencode" };
    let mut paths = vec![
        home.join(".opencode/bin")
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
    ];
    if beta {
        paths.push(runtime_root(home).join(format!("opencode2{}", std::env::consts::EXE_SUFFIX)));
        paths.push(runtime_root(home).join("desktop/resources/opencode-cli.exe"));
        #[cfg(target_os = "macos")]
        for root in [PathBuf::from("/Applications"), home.join("Applications")] {
            paths.push(root.join("OpenCode Beta.app/Contents/Resources/opencode-cli"));
        }
        #[cfg(windows)]
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            paths.push(
                PathBuf::from(local).join("Programs/OpenCode Beta/resources/opencode-cli.exe"),
            );
        }
    }
    paths
}

pub(crate) fn find_client(beta: bool) -> Option<PathBuf> {
    which::which(if beta { "opencode2" } else { "opencode" })
        .ok()
        .or_else(|| {
            client_paths(&dirs::home_dir()?, beta)
                .into_iter()
                .find_map(|path| which::which(path).ok())
        })
}

fn v2_version(output: &str) -> Option<&str> {
    let version = output.trim().strip_prefix("opencode v")?;
    valid_v2_version(version).then_some(version)
}

fn valid_v2_version(version: &str) -> bool {
    let parts = version.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        && parts[0].parse::<u64>().is_ok_and(|major| major >= 2)
}

async fn find_v2_client() -> Result<Option<(PathBuf, String)>> {
    let home = dirs::home_dir().context("Unable to get home directory")?;
    let mut candidates = Vec::new();
    candidates.extend(which::which("opencode2").ok());
    candidates.extend(client_paths(&home, true));
    candidates.extend(which::which("opencode").ok());
    for binary in candidates {
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new(&binary)
                .arg("--version")
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await;
        if let Ok(Ok(output)) = result
            && output.status.success()
            && let Some(version) = v2_version(&String::from_utf8_lossy(&output.stdout))
        {
            return Ok(Some((binary, version.into())));
        }
    }
    Ok(None)
}

/// The local V2 client chooses the remote release. Never replace a newer local
/// installation with the old, independently managed beta server's version.
pub(crate) async fn server_version() -> Result<Option<String>> {
    Ok(find_v2_client().await?.map(|(_, version)| version))
}

pub(crate) async fn ensure_client(beta: bool) -> Result<Option<PathBuf>> {
    let found = if beta {
        find_v2_client().await?.map(|(binary, _)| binary)
    } else {
        find_client(false)
    };
    let name = if beta { "OpenCode2 [Beta]" } else { "OpenCode" };
    ensure_client_with(
        found,
        || {
            confirm(&format!(
                "{name} is not installed locally. Install it from OpenCode's official release?"
            ))
        },
        || async {
            let home = dirs::home_dir().context("Unable to get home directory")?;
            let installed = if beta {
                install_beta(&runtime_root(&home)).await?
            } else {
                install_standard(&home).await?
            };
            println!("Installed {name}: {}", installed.display());
            Ok(installed)
        },
    )
    .await
}

/// Installation while the CA frame owns the terminal must not print or prompt.
pub(crate) async fn ensure_client_quiet(beta: bool) -> Result<PathBuf> {
    let found = if beta {
        find_v2_client().await?.map(|(binary, _)| binary)
    } else {
        find_client(false)
    };
    if let Some(binary) = found {
        return Ok(binary);
    }
    let home = dirs::home_dir().context("Unable to get home directory")?;
    if beta {
        install_beta(&runtime_root(&home)).await
    } else {
        install_standard(&home).await
    }
}

async fn ensure_client_with<F, I, Fut>(
    found: Option<PathBuf>,
    consent: F,
    install: I,
) -> Result<Option<PathBuf>>
where
    F: FnOnce() -> Result<bool>,
    I: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<PathBuf>>,
{
    if let Some(binary) = found {
        return Ok(Some(binary));
    }
    if !consent()? {
        return Ok(None);
    }
    Ok(Some(install().await?))
}

#[cfg(test)]
fn client_command(binary: &Path, connection: &Connection, beta: bool) -> Command {
    let mut command = Command::new(binary);
    command
        .args(super::attach_args(connection, beta))
        .env("OPENCODE_SERVER_USERNAME", &connection.username)
        .env("OPENCODE_SERVER_PASSWORD", &connection.password)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

async fn install_standard(home: &Path) -> Result<PathBuf> {
    if cfg!(windows) {
        let npm = which::which("npm")
            .context("Install Node.js, then rerun connect to install OpenCode")?;
        let output = tokio::time::timeout(
            Duration::from_secs(300),
            tokio::process::Command::new(npm)
                .args(["install", "--global", "opencode-ai"])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("OpenCode installation timed out")??;
        if !output.status.success() {
            bail!(
                "OpenCode installation failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    } else {
        // Fetch the documented installer as a file; no shell pipeline or profile
        // edits. Resolve its known install location even before PATH is updated.
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?
            .get("https://opencode.ai/install")
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let mut script = tempfile::NamedTempFile::new()?;
        script.write_all(&response)?;
        script.flush()?;
        let output = tokio::time::timeout(
            Duration::from_secs(300),
            tokio::process::Command::new("bash")
                .arg(script.path())
                .arg("--no-modify-path")
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("OpenCode installation timed out")??;
        if !output.status.success() {
            bail!(
                "OpenCode installation failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    find_client(false).or_else(|| client_paths(home, false).into_iter().find_map(|path| which::which(path).ok()))
        .context("OpenCode installation finished, but its client could not be found. The remote server is still running.")
}

fn beta_asset_name(os: &str, arch: &str) -> Result<String> {
    let os = match os {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        _ => bail!("No OpenCode V2 package for {os}/{arch}"),
    };
    let arch = match arch {
        "aarch64" => "arm64",
        "x86_64" => "x64-baseline",
        _ => bail!("No OpenCode V2 package for {os}/{arch}"),
    };
    Ok(format!("cli-{os}-{arch}"))
}

struct Asset {
    url: String,
    digest: Vec<u8>,
}

fn beta_asset(package: &Value, name: &str, version: &str) -> Result<Asset> {
    if !valid_v2_version(version)
        || package["name"] != format!("@opencode/{name}")
        || package["version"] != version
    {
        bail!("OpenCode V2 returned an unexpected package identity");
    }
    let url = format!("https://registry.npmjs.org/@opencode/{name}/-/{name}-{version}.tgz");
    if package["dist"]["tarball"] != url {
        bail!("OpenCode V2 returned an unexpected download address");
    }
    let digest = package["dist"]["integrity"]
        .as_str()
        .and_then(|value| value.strip_prefix("sha512-"))
        .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok())
        .filter(|value| value.len() == 64)
        .context("OpenCode V2 package has no valid SHA-512 integrity digest")?;
    Ok(Asset { url, digest })
}

async fn download(client: &reqwest::Client, asset: &Asset, package: &Path) -> Result<()> {
    let mut response = client.get(&asset.url).send().await?.error_for_status()?;
    let mut file = fs::File::create(package)?;
    let mut digest = Sha512::new();
    while let Some(chunk) = response.chunk().await? {
        digest.update(&chunk);
        file.write_all(&chunk)?;
    }
    file.sync_all()?;
    if digest.finalize().as_slice() != asset.digest {
        bail!("OpenCode V2 checksum did not match; nothing was installed");
    }
    Ok(())
}

fn extract_beta(package: &Path, output: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(fs::File::open(package)?);
    for entry in tar::Archive::new(decoder).entries()? {
        let mut entry = entry?;
        if entry.header().entry_type().is_file()
            && entry.path()?
                == Path::new(if cfg!(windows) {
                    "package/bin/opencode.exe"
                } else {
                    "package/bin/opencode"
                })
        {
            let mut target = fs::File::create(output)?;
            std::io::copy(&mut entry, &mut target)?;
            target.sync_all()?;
            return Ok(());
        }
    }
    bail!("The OpenCode V2 package contains no standalone CLI executable")
}

async fn install_beta(root: &Path) -> Result<PathBuf> {
    fs::create_dir_all(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    }
    let client = reqwest::Client::builder()
        .user_agent("railway-opencode2-client")
        .timeout(Duration::from_secs(600))
        .build()?;
    let latest: Value = client
        .get("https://registry.npmjs.org/@opencode%2fcli/latest")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let version = latest["version"]
        .as_str()
        .filter(|version| valid_v2_version(version))
        .context("No published OpenCode V2 release was found")?;
    let name = beta_asset_name(std::env::consts::OS, std::env::consts::ARCH)?;
    let package: Value = client
        .get(format!(
            "https://registry.npmjs.org/@opencode%2f{name}/{version}"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let asset = beta_asset(&package, &name, version)?;
    let temporary = tempfile::tempdir_in(root)?;
    let package = temporary.path().join("package.tgz");
    download(&client, &asset, &package).await?;
    let staged = temporary
        .path()
        .join(format!("opencode2{}", std::env::consts::EXE_SUFFIX));
    extract_beta(&package, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o700))?;
    }
    let checked = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(&staged)
            .arg("--version")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Checking OpenCode V2 timed out")??;
    if !checked.status.success()
        || v2_version(&String::from_utf8_lossy(&checked.stdout)) != Some(version)
    {
        bail!("The downloaded OpenCode V2 client could not run or returned an unexpected version");
    }
    let binary = root.join(format!("opencode2{}", std::env::consts::EXE_SUFFIX));
    fs::rename(staged, &binary)?;
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "downloads the official V2 package and runs its CLI in a temporary directory"]
    async fn official_beta_install_smoke() {
        let root = tempfile::tempdir().unwrap();
        let binary = install_beta(root.path()).await.unwrap();
        assert!(binary.starts_with(root.path()));
        let output = Command::new(binary).arg("--version").output().unwrap();
        assert!(output.status.success());
        assert!(v2_version(&String::from_utf8_lossy(&output.stdout)).is_some());
    }

    #[tokio::test]
    async fn installation_requires_consent_and_is_skipped_for_an_existing_client() {
        let found = ensure_client_with(
            Some("installed".into()),
            || panic!("must not prompt"),
            || async { panic!("must not install") },
        )
        .await
        .unwrap();
        assert_eq!(found.unwrap(), PathBuf::from("installed"));
        let canceled = ensure_client_with(
            None,
            || Ok(false),
            || async { panic!("must not install after cancel") },
        )
        .await
        .unwrap();
        assert!(canceled.is_none());
        let installed = ensure_client_with(
            None,
            || Ok(true),
            || async { Ok(PathBuf::from("new client")) },
        )
        .await
        .unwrap();
        assert_eq!(installed.unwrap(), PathBuf::from("new client"));
        assert!(
            ensure_client_with(None, || Ok(true), || async { bail!("download failed") })
                .await
                .is_err()
        );
    }

    #[test]
    fn editions_have_distinct_discovery_paths_and_release_packages() {
        let home = Path::new("/test");
        assert!(
            client_paths(home, true)
                .iter()
                .all(|p| !p.ends_with("bin/opencode"))
        );
        assert_eq!(
            beta_asset_name("macos", "aarch64").unwrap(),
            "cli-darwin-arm64"
        );
        assert_eq!(
            beta_asset_name("linux", "x86_64").unwrap(),
            "cli-linux-x64-baseline"
        );
        assert_eq!(
            beta_asset_name("windows", "aarch64").unwrap(),
            "cli-windows-arm64"
        );
        assert!(beta_asset_name("linux", "riscv64").is_err());
    }

    #[test]
    fn installer_requires_official_release_and_checksum() {
        let name = "cli-darwin-arm64";
        let package = json!({"name":format!("@opencode/{name}"),"version":"2.0.5","dist":{
            "tarball":format!("https://registry.npmjs.org/@opencode/{name}/-/{name}-2.0.5.tgz"),
            "integrity":format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode([1u8;64]))}});
        assert!(beta_asset(&package, name, "2.0.5").is_ok());
        assert!(beta_asset(&package, name, "2.0.4").is_err());
        let mut hostile = package.clone();
        hostile["dist"]["tarball"] = json!("https://example.com/client");
        assert!(beta_asset(&hostile, name, "2.0.5").is_err());
        let mut missing = package;
        missing["dist"]["integrity"] = Value::Null;
        assert!(beta_asset(&missing, name, "2.0.5").is_err());
    }

    #[test]
    fn v2_client_version_drives_the_server_without_accepting_legacy_beta() {
        assert_eq!(v2_version("opencode v2.0.5\n"), Some("2.0.5"));
        for output in [
            "opencode2 v0.0.0-beta-19425",
            "1.18.31",
            "opencode v2.0.5/../../other",
            "opencode v2.0.5?next=1",
        ] {
            assert_eq!(v2_version(output), None);
        }
    }

    #[test]
    fn beta_extraction_copies_only_the_regular_cli_file() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("beta.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            fs::File::create(&package).unwrap(),
            flate2::Compression::fast(),
        );
        let mut archive = tar::Builder::new(encoder);
        let member = format!("package/bin/opencode{}", std::env::consts::EXE_SUFFIX);
        for (name, data) in [
            (member.as_str(), "binary"),
            ("OpenCode Beta.app/unrelated", "ignore"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o700);
            header.set_cksum();
            archive
                .append_data(&mut header, name, data.as_bytes())
                .unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap();
        let output = root.path().join("opencode2");
        extract_beta(&package, &output).unwrap();
        assert_eq!(fs::read_to_string(output).unwrap(), "binary");
        assert!(!root.path().join("OpenCode Beta.app").exists());
    }

    #[cfg(unix)]
    #[test]
    fn local_child_gets_literal_auth_and_remote_arguments_without_a_shell() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("local client");
        fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' \"$OPENCODE_SERVER_USERNAME\" \"$OPENCODE_SERVER_PASSWORD\" \"$@\"\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let c = Connection {
            url: "https://agent.up.railway.app".into(),
            username: "opencode".into(),
            password: "' $(touch INJECTED)".into(),
            directory: "/remote/path with spaces".into(),
            reused: true,
        };
        for beta in [false, true] {
            let mut command = client_command(&binary, &c, beta);
            assert!(!command.get_args().any(|arg| arg == c.password.as_str()));
            let output = command
                .current_dir(root.path())
                .stdout(Stdio::piped())
                .output()
                .unwrap();
            assert!(output.status.success());
            let text = String::from_utf8(output.stdout).unwrap();
            let lines: Vec<_> = text.lines().collect();
            assert_eq!(&lines[..2], ["opencode", c.password.as_str()]);
            assert_eq!(lines[2..], super::super::attach_args(&c, beta));
            assert!(!root.path().join("INJECTED").exists());
        }
    }
}
