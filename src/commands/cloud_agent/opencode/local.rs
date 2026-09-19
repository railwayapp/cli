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

use super::{Connection, Protocol};
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

fn runtime_root(home: &Path, version: &str) -> PathBuf {
    home.join(".railway/runtimes/opencode-client").join(version)
}

fn client_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        home.join(".opencode/bin")
            .join(format!("opencode{}", std::env::consts::EXE_SUFFIX)),
        home.join(".opencode/bin")
            .join(format!("opencode2{}", std::env::consts::EXE_SUFFIX)),
        home.join(".railway/runtimes/opencode2-client")
            .join(format!("opencode2{}", std::env::consts::EXE_SUFFIX)),
    ];
    #[cfg(target_os = "macos")]
    for root in [PathBuf::from("/Applications"), home.join("Applications")] {
        for app in ["OpenCode.app", "OpenCode Beta.app"] {
            paths.push(root.join(app).join("Contents/Resources/opencode-cli"));
        }
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        for app in ["OpenCode", "OpenCode Beta"] {
            paths.push(
                PathBuf::from(&local)
                    .join("Programs")
                    .join(app)
                    .join("resources/opencode-cli.exe"),
            );
        }
    }
    paths
}

#[cfg(test)]
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
        && parts[0] == "2"
}

fn client_version(output: &str) -> Option<&str> {
    let version = output
        .trim()
        .strip_prefix("opencode v")
        .unwrap_or(output.trim());
    if let Some(build) = version.strip_prefix("0.0.0-beta-") {
        return (!build.is_empty() && build.bytes().all(|b| b.is_ascii_digit())).then_some(version);
    }
    let parts = version.split('.').collect::<Vec<_>>();
    (parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && Protocol::from_version(version).is_some())
    .then_some(version)
}

pub(crate) async fn compatible(binary: &Path, protocol: Protocol, version: Option<&str>) -> bool {
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(binary)
            .arg("--version")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    matches!(result, Ok(Ok(output)) if output.status.success()
        && client_version(&String::from_utf8_lossy(&output.stdout)).is_some_and(|actual|
            Protocol::from_version(actual) == Some(protocol) && version.is_none_or(|v| v == actual)))
}

async fn find_client(connection: &Connection) -> Result<Option<PathBuf>> {
    if let Some(version) = &connection.version
        && (client_version(version) != Some(version.as_str())
            || Protocol::from_version(version) != Some(connection.protocol))
    {
        bail!(
            "Unsupported remote OpenCode release; use railway code --opencode upgrade <agent> before installing a matching client"
        );
    }
    let home = dirs::home_dir().context("Unable to get home directory")?;
    let mut candidates = Vec::new();
    if let Some(version) = &connection.version {
        candidates.push(
            runtime_root(&home, version).join(format!("opencode{}", std::env::consts::EXE_SUFFIX)),
        );
    }
    candidates.extend(which::which("opencode").ok());
    candidates.extend(which::which("opencode2").ok());
    candidates.extend(client_paths(&home));
    for binary in candidates {
        if compatible(&binary, connection.protocol, connection.version.as_deref()).await {
            return Ok(Some(binary));
        }
    }
    Ok(None)
}

pub(crate) async fn ensure_client(connection: &Connection) -> Result<Option<PathBuf>> {
    let found = find_client(connection).await?;
    let name = connection.protocol.label();
    ensure_client_with(
        found,
        || {
            confirm(&format!(
                "A matching {name} client is not installed locally. Install a Railway-managed copy?"
            ))
        },
        || async {
            let installed = install_client(connection).await?;
            println!("Installed {name}: {}", installed.display());
            Ok(installed)
        },
    )
    .await
}

/// Installation while the CA frame owns the terminal must not print or prompt.
pub(crate) async fn ensure_client_quiet(connection: &Connection) -> Result<PathBuf> {
    let found = find_client(connection).await?;
    if let Some(binary) = found {
        return Ok(binary);
    }
    install_client(connection).await
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
fn client_command(binary: &Path, connection: &Connection) -> Command {
    let mut command = Command::new(binary);
    command
        .args(super::attach_args(connection))
        .env("OPENCODE_SERVER_USERNAME", &connection.username)
        .env("OPENCODE_SERVER_PASSWORD", &connection.password)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

async fn install_client(connection: &Connection) -> Result<PathBuf> {
    let version = connection.version.as_deref().context("The remote server did not report its version; reconnect before installing a matching client")?;
    if client_version(version) != Some(version)
        || Protocol::from_version(version) != Some(connection.protocol)
        || version.starts_with("0.0.0-beta-")
    {
        bail!(
            "The remote OpenCode release is unsupported; explicitly upgrade the VM before installing a client"
        );
    }
    let home = dirs::home_dir().context("Unable to get home directory")?;
    install_release(&runtime_root(&home, version), connection.protocol, version).await
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

async fn install_release(root: &Path, protocol: Protocol, version: &str) -> Result<PathBuf> {
    fs::create_dir_all(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    }
    let client = reqwest::Client::builder()
        .user_agent("railway-opencode-client")
        .timeout(Duration::from_secs(600))
        .build()?;
    let name = beta_asset_name(std::env::consts::OS, std::env::consts::ARCH)?;
    let package_name = if protocol.is_v2() {
        format!("@opencode/{name}")
    } else {
        name.replacen("cli-", "opencode-", 1)
    };
    let package: Value = client
        .get(format!(
            "https://registry.npmjs.org/{package_name}/{version}"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let asset = if protocol.is_v2() {
        beta_asset(&package, &name, version)?
    } else {
        let url =
            format!("https://registry.npmjs.org/{package_name}/-/{package_name}-{version}.tgz");
        if package["name"] != package_name
            || package["version"] != version
            || package["dist"]["tarball"] != url
        {
            bail!("OpenCode V1 returned an unexpected package identity");
        }
        let digest = package["dist"]["integrity"]
            .as_str()
            .and_then(|v| v.strip_prefix("sha512-"))
            .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok())
            .filter(|v| v.len() == 64)
            .context("OpenCode V1 package has no SHA-512 integrity digest")?;
        Asset { url, digest }
    };
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
        || client_version(&String::from_utf8_lossy(&checked.stdout)) != Some(version)
    {
        bail!("The downloaded OpenCode V2 client could not run or returned an unexpected version");
    }
    let binary = root.join(format!("opencode{}", std::env::consts::EXE_SUFFIX));
    fs::rename(staged, &binary)?;
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "downloads official V1 and V2 packages and runs each CLI in a temporary directory"]
    async fn official_release_install_smoke() {
        for (protocol, version) in [(Protocol::V1, "1.18.29"), (Protocol::V2, "2.0.8")] {
            let root = tempfile::tempdir().unwrap();
            let binary = install_release(root.path(), protocol, version)
                .await
                .unwrap();
            assert!(binary.starts_with(root.path()));
            assert!(compatible(&binary, protocol, Some(version)).await);
        }
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
    fn discovery_includes_canonical_and_alias_binaries_and_official_packages() {
        let home = Path::new("/test");
        assert!(
            client_paths(home)
                .iter()
                .any(|p| p.ends_with(format!("bin/opencode{}", std::env::consts::EXE_SUFFIX)))
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

    #[cfg(unix)]
    #[tokio::test]
    async fn executable_name_never_substitutes_for_protocol_and_release() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("opencode");
        for (output, protocol) in [("1.18.29", Protocol::V1), ("opencode v2.0.8", Protocol::V2)] {
            fs::write(&binary, format!("#!/bin/sh\necho '{output}'\n")).unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(compatible(&binary, protocol, None).await);
            let other = if protocol.is_v2() {
                Protocol::V1
            } else {
                Protocol::V2
            };
            assert!(!compatible(&binary, other, None).await);
            assert!(!compatible(&binary, protocol, Some("2.0.7")).await);
        }
        assert!(compatible(&binary, Protocol::V2, Some("2.0.8")).await);
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
        let mut c = Connection {
            url: "https://agent.up.railway.app".into(),
            username: "opencode".into(),
            password: "' $(touch INJECTED)".into(),
            directory: "/remote/path with spaces".into(),
            reused: true,
            protocol: Protocol::V1,
            version: None,
        };
        for protocol in [Protocol::V1, Protocol::V2] {
            c.protocol = protocol;
            let mut command = client_command(&binary, &c);
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
            assert_eq!(lines[2..], super::super::attach_args(&c));
            assert!(!root.path().join("INJECTED").exists());
        }
    }
}
