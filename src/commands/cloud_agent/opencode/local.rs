//! Find/install the selected local client, then hand it the user's terminal.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::Connection;
use crate::config::Configs;

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

pub(crate) async fn ensure_client(beta: bool) -> Result<Option<PathBuf>> {
    let name = if beta { "OpenCode2 [Beta]" } else { "OpenCode" };
    let detail = if beta && cfg!(windows) {
        " (includes the Beta Desktop package)"
    } else {
        ""
    };
    ensure_client_with(find_client(beta),
        || confirm(&format!("{name} is not installed locally. Install it from OpenCode's official release{detail}?")),
        || async {
            let home = dirs::home_dir().context("Unable to get home directory")?;
            let installed = if beta { install_beta(&runtime_root(&home)).await? } else { install_standard(&home).await? };
            println!("Installed {name}: {}", installed.display());
            Ok(installed)
        }).await
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

pub(crate) fn run_client(binary: &Path, connection: &Connection, beta: bool) -> Result<ExitStatus> {
    client_command(binary, connection, beta)
        .status()
        .with_context(|| format!("Could not launch local client {}", binary.display()))
}

async fn install_standard(home: &Path) -> Result<PathBuf> {
    if cfg!(windows) {
        let npm = which::which("npm")
            .context("Install Node.js, then rerun connect to install OpenCode")?;
        let status = Command::new(npm)
            .args(["install", "--global", "opencode-ai"])
            .status()?;
        if !status.success() {
            bail!("OpenCode installation failed ({status}); the remote server is still running.");
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
        let status = Command::new("bash")
            .arg(script.path())
            .arg("--no-modify-path")
            .status()?;
        if !status.success() {
            bail!("OpenCode installation failed ({status}); the remote server is still running.");
        }
    }
    find_client(false).or_else(|| client_paths(home, false).into_iter().find_map(|path| which::which(path).ok()))
        .context("OpenCode installation finished, but its client could not be found. The remote server is still running.")
}

fn beta_asset_name(os: &str, arch: &str) -> Result<String> {
    let arch = match arch {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => bail!("No OpenCode2 Beta package for {os}/{other}"),
    };
    Ok(match os {
        "macos" => format!("opencode-desktop-mac-{arch}.app.tar.gz"),
        "linux" => format!(
            "opencode-desktop-linux-{}.deb",
            if arch == "x64" { "amd64" } else { arch }
        ),
        "windows" => format!("opencode-desktop-win-{arch}.exe"),
        _ => bail!("No OpenCode2 Beta package for {os}/{arch}"),
    })
}

struct Asset {
    url: String,
    digest: String,
}

fn beta_asset(releases: &[Value], name: &str) -> Result<Asset> {
    let release = releases
        .iter()
        .find(|r| {
            r["draft"] == false
                && r["tag_name"]
                    .as_str()
                    .is_some_and(|tag| tag.starts_with("v0.0.0-beta-"))
        })
        .context("No published OpenCode2 Beta release was found")?;
    let tag = release["tag_name"]
        .as_str()
        .context("Missing Beta release tag")?;
    let asset = release["assets"]
        .as_array()
        .context("Missing Beta assets")?
        .iter()
        .find(|a| a["name"] == name)
        .with_context(|| format!("The latest Beta has no {name} package"))?;
    let url = asset["browser_download_url"]
        .as_str()
        .context("Missing Beta package URL")?;
    if url != format!("https://github.com/anomalyco/opencode-beta/releases/download/{tag}/{name}") {
        bail!("OpenCode2 Beta returned an unexpected download address");
    }
    let digest = asset["digest"]
        .as_str()
        .and_then(|s| s.strip_prefix("sha256:"))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .context("Beta package has no valid SHA-256 digest")?;
    Ok(Asset {
        url: url.into(),
        digest: digest.into(),
    })
}

async fn download(client: &reqwest::Client, asset: &Asset, package: &Path) -> Result<()> {
    let mut response = client.get(&asset.url).send().await?.error_for_status()?;
    let mut file = fs::File::create(package)?;
    let mut digest = Sha256::new();
    while let Some(chunk) = response.chunk().await? {
        digest.update(&chunk);
        file.write_all(&chunk)?;
    }
    file.sync_all()?;
    if format!("{:x}", digest.finalize()) != asset.digest {
        bail!("OpenCode2 Beta checksum did not match; nothing was installed");
    }
    Ok(())
}

fn extract_mac(package: &Path, output: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(fs::File::open(package)?);
    for entry in tar::Archive::new(decoder).entries()? {
        let mut entry = entry?;
        if entry.header().entry_type().is_file()
            && entry.path()?.ends_with("Contents/Resources/opencode-cli")
        {
            let mut target = fs::File::create(output)?;
            std::io::copy(&mut entry, &mut target)?;
            target.sync_all()?;
            return Ok(());
        }
    }
    bail!("The Beta package contains no standalone CLI executable")
}

fn extract_linux(package: &Path, output: &Path, temporary: &Path) -> Result<()> {
    // ar/tar handle Debian's compression without adding a platform codec to
    // Railway. Extract just one file to stdout; archive paths never write to
    // the user's filesystem.
    let listing = Command::new("ar")
        .arg("t")
        .arg(package)
        .output()
        .context("Beta installation requires ar (binutils) and tar")?;
    if !listing.status.success() {
        bail!("Could not read the Beta Debian package");
    }
    let listing = String::from_utf8(listing.stdout)?;
    let member = listing
        .lines()
        .find(|s| {
            matches!(
                *s,
                "data.tar.xz" | "data.tar.gz" | "data.tar.zst" | "data.tar"
            )
        })
        .context("Beta Debian package contains no data archive")?;
    let archive = temporary.join(member);
    let status = Command::new("ar")
        .arg("p")
        .arg(package)
        .arg(member)
        .stdout(fs::File::create(&archive)?)
        .status()?;
    if !status.success() {
        bail!("Could not extract the Beta data archive");
    }
    let listing = Command::new("tar").arg("-tf").arg(&archive).output()?;
    if !listing.status.success() {
        bail!("Could not read the Beta data archive; install tar with xz support");
    }
    let listing = String::from_utf8(listing.stdout)?;
    let member = listing
        .lines()
        .find(|s| s.ends_with("/resources/opencode-cli"))
        .context("Beta package contains no standalone CLI")?;
    let status = Command::new("tar")
        .arg("-xOf")
        .arg(&archive)
        .arg("--")
        .arg(member)
        .stdout(fs::File::create(output)?)
        .status()?;
    if !status.success() {
        bail!("Could not extract the Beta CLI");
    }
    Ok(())
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
    let releases: Vec<Value> = client
        .get("https://api.github.com/repos/anomalyco/opencode-beta/releases?per_page=5")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let name = beta_asset_name(std::env::consts::OS, std::env::consts::ARCH)?;
    let asset = beta_asset(&releases, &name)?;
    println!("Downloading the latest official OpenCode2 Beta client…");
    let temporary = tempfile::tempdir_in(root)?;
    let package = temporary.path().join(&name);
    download(&client, &asset, &package).await?;
    if cfg!(windows) {
        let desktop = root.join("desktop");
        let status = Command::new(&package)
            .arg("/S")
            .arg(format!("/D={}", desktop.display()))
            .status()?;
        if !status.success() {
            bail!("The Beta installer exited with {status}");
        }
        let binary = desktop.join("resources/opencode-cli.exe");
        if !binary.is_file() {
            bail!(
                "The Beta installer did not install its CLI in {}",
                desktop.display()
            );
        }
        return Ok(binary);
    }
    let staged = temporary.path().join("opencode2");
    match std::env::consts::OS {
        "macos" => extract_mac(&package, &staged)?,
        "linux" => extract_linux(&package, &staged, temporary.path())?,
        _ => bail!("Unsupported platform for OpenCode2 Beta"),
    }
    if fs::metadata(&staged)?.len() == 0 {
        bail!("The Beta CLI package was empty");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o700))?;
    }
    let version = Command::new(&staged)
        .arg("--version")
        .output()
        .context("Checking the installed Beta client")?;
    if !version.status.success() || !String::from_utf8_lossy(&version.stdout).contains("beta") {
        bail!("The downloaded Beta client could not run on this computer");
    }
    let binary = root.join("opencode2");
    fs::rename(staged, &binary)?;
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "downloads the official Beta package and runs its CLI in a temporary directory"]
    async fn official_beta_install_smoke() {
        let root = tempfile::tempdir().unwrap();
        let binary = install_beta(root.path()).await.unwrap();
        assert!(binary.starts_with(root.path()));
        let output = Command::new(binary).arg("--version").output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("beta"));
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
            "opencode-desktop-mac-arm64.app.tar.gz"
        );
        assert_eq!(
            beta_asset_name("linux", "x86_64").unwrap(),
            "opencode-desktop-linux-amd64.deb"
        );
        assert_eq!(
            beta_asset_name("windows", "aarch64").unwrap(),
            "opencode-desktop-win-arm64.exe"
        );
        assert!(beta_asset_name("linux", "riscv64").is_err());
    }

    #[test]
    fn installer_requires_official_release_and_checksum() {
        let name = "opencode-desktop-mac-arm64.app.tar.gz";
        let release = json!({"draft":false,"tag_name":"v0.0.0-beta-test", "assets":[{"name":name,"browser_download_url":format!("https://github.com/anomalyco/opencode-beta/releases/download/v0.0.0-beta-test/{name}"),"digest":format!("sha256:{}", "a".repeat(64))}]});
        assert!(beta_asset(std::slice::from_ref(&release), name).is_ok());
        let mut hostile = release.clone();
        hostile["assets"][0]["browser_download_url"] = json!("https://example.com/client");
        assert!(beta_asset(&[hostile], name).is_err());
        let mut missing = release;
        missing["assets"][0]["digest"] = Value::Null;
        assert!(beta_asset(&[missing], name).is_err());
    }

    #[test]
    fn mac_extraction_copies_only_the_regular_cli_file() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("beta.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            fs::File::create(&package).unwrap(),
            flate2::Compression::fast(),
        );
        let mut archive = tar::Builder::new(encoder);
        for (name, data) in [
            (
                "OpenCode Beta.app/Contents/Resources/opencode-cli",
                "binary",
            ),
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
        extract_mac(&package, &output).unwrap();
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
