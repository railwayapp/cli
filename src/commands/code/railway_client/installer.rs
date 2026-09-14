//! Resolve the latest stable TUI on every launch; verify downloads before reuse.
use crate::commands::code::Progress;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Duration};

pub(crate) struct InstalledClient {
    pub binary: PathBuf,
    pub version: String,
}

fn validate_version(version: &str) -> Result<()> {
    if !regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+$")?.is_match(version) {
        bail!("Unsupported Railway agent version");
    }
    Ok(())
}

fn platform() -> Result<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        _ => bail!("Railway TUI releases support macOS and Linux; use WSL or --railway remote"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => bail!("Railway TUI releases require amd64 or arm64"),
    };
    Ok(format!("{os}-{arch}"))
}

async fn compatible(binary: &std::path::Path, version: &str) -> bool {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.arg("--version").kill_on_drop(true);
    let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(5), cmd.output()).await else {
        return false;
    };
    output.status.success()
        && matches_release(String::from_utf8_lossy(&output.stdout).trim(), version)
}

fn matches_release(output: &str, version: &str) -> bool {
    let Some(installed) = output.strip_prefix("railway-agent-tui ") else {
        return false;
    };
    // Local fixes may identify themselves with SemVer build metadata. Reuse them only for
    // the same release: the next published version must still trigger the normal update.
    let (base, metadata) = installed.split_once('+').unwrap_or((installed, ""));
    base == version
        && (installed == base
            || (!metadata.is_empty()
                && metadata.split('.').all(|part| {
                    !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })))
}

pub(crate) async fn ensure_client(progress: &dyn Progress) -> Result<InstalledClient> {
    let client = reqwest::Client::builder()
        .user_agent("railway-cli")
        .timeout(Duration::from_secs(120))
        .build()?;
    let root = dirs::home_dir()
        .context("Unable to get home directory")?
        .join(".railway/runtimes/railway-tui");
    ensure_client_from(
        &client,
        "https://api.github.com/repos/railwayapp/agent-releases/releases/latest",
        "https://github.com/railwayapp/agent-releases/releases/download",
        &root,
        &platform()?,
        progress,
    )
    .await
}

async fn ensure_client_from(
    client: &reqwest::Client,
    latest_url: &str,
    download_base: &str,
    root: &std::path::Path,
    platform: &str,
    progress: &dyn Progress,
) -> Result<InstalledClient> {
    progress.step("Checking for Railway client updates");
    let release: serde_json::Value = client
        .get(latest_url)
        .timeout(Duration::from_secs(15))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let version = release_version(&release)?;
    let binary = install_release(
        client,
        &release,
        &version,
        &root.join(&version),
        platform,
        download_base,
        progress,
    )
    .await?;
    Ok(InstalledClient { binary, version })
}

fn release_version(release: &serde_json::Value) -> Result<String> {
    let tag = release["tag_name"]
        .as_str()
        .context("Railway release has no version")?;
    let version = tag
        .strip_prefix('v')
        .context("Invalid Railway release tag")?;
    validate_version(version)?;
    if release["draft"] == true
        || release["prerelease"] == true
        || crate::util::compare_semver::compare_semver(version, "0.1.15")
            == std::cmp::Ordering::Less
    {
        bail!(
            "Railway release does not support the local client; version 0.1.15 or newer is required"
        );
    }
    Ok(version.into())
}

async fn install_release(
    client: &reqwest::Client,
    release: &serde_json::Value,
    version: &str,
    root: &std::path::Path,
    platform: &str,
    download_base: &str,
    progress: &dyn Progress,
) -> Result<PathBuf> {
    let binary = root.join("railway-agent-tui");
    if compatible(&binary, version).await {
        return Ok(binary);
    }
    if let Ok(candidate) = which::which("railway-agent-tui") {
        if compatible(&candidate, version).await {
            return Ok(candidate);
        }
    }
    progress.step(&format!("Installing Railway client v{version}"));
    let tag = format!("v{version}");
    let name = format!("railway-agent-tui-{tag}-{platform}.tar.gz");
    let asset = release["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|a| a["name"] == name))
        .with_context(|| {
            format!("Railway {tag} has no TUI release for {platform}; use --railway remote")
        })?;
    let digest = asset["digest"]
        .as_str()
        .context("Release asset is missing its checksum")?;
    let data = client
        .get(format!("{download_base}/{tag}/{name}"))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    if format!("sha256:{:x}", Sha256::digest(&data)) != digest {
        bail!("Railway TUI download checksum mismatch");
    }
    std::fs::create_dir_all(root)?;
    // Extract only the two regular executable files into a private temporary directory.
    let temporary = tempfile::tempdir_in(root)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(data.as_ref()));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.into_owned();
        if ["railway-agent-tui", "railway-agent"]
            .iter()
            .any(|expected| name == std::path::Path::new(expected))
        {
            if !entry.header().entry_type().is_file() {
                bail!("Invalid Railway release archive");
            }
            entry.unpack(temporary.path().join(name))?;
        }
    }
    if !compatible(&temporary.path().join("railway-agent-tui"), version).await
        || !temporary.path().join("railway-agent").is_file()
    {
        bail!("Downloaded Railway client does not match release v{version}");
    }
    for name in ["railway-agent", "railway-agent-tui"] {
        std::fs::rename(temporary.path().join(name), root.join(name))?;
    }
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_build_metadata_does_not_prevent_the_next_release_update() {
        assert!(matches_release("railway-agent-tui 0.1.15", "0.1.15"));
        assert!(matches_release(
            "railway-agent-tui 0.1.15+ca-render-fix",
            "0.1.15"
        ));
        for output in [
            "railway-agent-tui 0.1.15+ca-render-fix",
            "railway-agent-tui 0.1.16-dev",
            "railway-agent-tui 0.1.160",
            "railway-agent-tui 0.1.16+",
            "railway-agent-tui 0.1.16+invalid..metadata",
            "other-client 0.1.16",
        ] {
            assert!(!matches_release(output, "0.1.16"), "{output}");
        }
    }

    #[derive(Default)]
    struct ProgressLog(std::sync::Mutex<Vec<String>>);
    impl Progress for ProgressLog {
        fn step(&self, text: &str) {
            self.0.lock().unwrap().push(text.into());
        }
        fn note(&self, text: &str) {
            self.step(text);
        }
        fn finish(&self) {}
    }

    #[test]
    fn latest_release_must_be_stable_and_attach_capable() {
        for tag in [
            "v0.1.14",
            "v../0.1.15",
            "latest",
            "v0.1.16-beta.1",
            "v0.1.16/other",
        ] {
            assert!(release_version(&json!({"tag_name": tag})).is_err());
        }
        for flag in ["draft", "prerelease"] {
            let mut release = json!({"tag_name":"v0.1.16"});
            release[flag] = true.into();
            assert!(release_version(&release).is_err());
        }
        assert_eq!(
            release_version(&json!({"tag_name":"v0.1.16"})).unwrap(),
            "0.1.16"
        );
    }

    #[cfg(unix)]
    fn archive(version: &str) -> Vec<u8> {
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gzip);
        for name in ["railway-agent-tui", "railway-agent"] {
            let body = format!("#!/bin/sh\nprintf '%s\\n' '{name} {version}'\n");
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            header.set_size(body.len() as u64);
            header.set_cksum();
            tar.append_data(&mut header, name, body.as_bytes()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn every_launch_checks_latest_updates_and_reuses_only_the_current_version() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let root = tempfile::tempdir().unwrap();
        let platform = "test-arm64";
        let mut replies = Vec::new();
        for (version, download, corrupt) in [
            ("7.0.1", true, false),
            ("7.0.2", true, false),
            ("7.0.2", false, false),
            ("7.0.3", true, true),
        ] {
            let data = archive(version);
            let digest = if corrupt {
                "sha256:invalid".into()
            } else {
                format!("sha256:{:x}", Sha256::digest(&data))
            };
            let name = format!("railway-agent-tui-v{version}-{platform}.tar.gz");
            let release =
                json!({"tag_name":format!("v{version}"), "assets":[{"name":name,"digest":digest}]});
            replies.push(("/latest".to_string(), release.to_string().into_bytes()));
            if download {
                replies.push((format!("/download/v{version}/{name}"), data));
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for (path, body) in replies {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                assert!(
                    String::from_utf8_lossy(&request).starts_with(&format!("GET {path} HTTP/1.1"))
                );
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                stream.write_all(&body).await.unwrap();
            }
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let progress = ProgressLog::default();
        for expected in ["7.0.1", "7.0.2", "7.0.2"] {
            let installed = ensure_client_from(
                &client,
                &format!("{base}/latest"),
                &format!("{base}/download"),
                root.path(),
                platform,
                &progress,
            )
            .await
            .unwrap();
            assert_eq!(installed.version, expected);
            assert!(compatible(&installed.binary, expected).await);
        }
        let error = ensure_client_from(
            &client,
            &format!("{base}/latest"),
            &format!("{base}/download"),
            root.path(),
            platform,
            &progress,
        )
        .await
        .err()
        .unwrap();
        assert!(error.to_string().contains("checksum mismatch"));
        assert!(compatible(&root.path().join("7.0.2/railway-agent-tui"), "7.0.2").await);
        assert!(!root.path().join("7.0.3/railway-agent-tui").exists());
        assert_eq!(
            progress
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.as_str() == "Checking for Railway client updates")
                .count(),
            4
        );
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "downloads the latest official Railway TUI release"]
    async fn official_client_installs_and_is_reused() {
        let first = ensure_client(&ProgressLog::default()).await.unwrap();
        assert!(compatible(&first.binary, &first.version).await);
        let second = ensure_client(&ProgressLog::default()).await.unwrap();
        if first.version == second.version {
            assert_eq!(first.binary, second.binary);
        }
    }
}
