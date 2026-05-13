use std::{
    borrow::Cow,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{StreamExt, TryStreamExt, stream};
use inquire::Password;
use is_terminal::IsTerminal;
use russh::{
    Preferred, client,
    keys::{PrivateKeyWithHashAlg, load_secret_key},
};
use russh_sftp::{
    client::{SftpSession, error::Error as SftpError},
    protocol::StatusCode,
};
use serde::Serialize;
use tokio::{fs::File, io::AsyncWriteExt};

use crate::controllers::ssh_keys::find_local_ssh_keys;

const SSH_HOST: &str = "ssh.railway.com";
const SSH_PORT: u16 = 22;
const DIRECTORY_FILE_CONCURRENCY: usize = 16;
const DIRECTORY_SUBDIR_CONCURRENCY: usize = 4;

pub struct VolumeFileClient {
    sftp: SftpSession,
    ssh: client::Handle<RailwaySshClient>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeFileEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: VolumeFileKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VolumeFileKind {
    File,
    Directory,
    Symlink,
    Other,
}

impl VolumeFileKind {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Directory => "[d]",
            Self::File => "[f]",
            Self::Symlink => "[l]",
            Self::Other => "[?]",
        }
    }

    pub fn is_dir(self) -> bool {
        self == Self::Directory
    }
}

impl VolumeFileClient {
    pub fn connect(service_instance_id: String, identity_file: Option<PathBuf>) -> Result<Self> {
        block_on(async {
            let key_paths = resolve_private_key_paths(identity_file)?;
            let mut last_error = None;

            for key_path in key_paths {
                match connect_with_key(&service_instance_id, &key_path).await {
                    Ok((ssh, sftp)) => {
                        return Ok(Self {
                            sftp,
                            ssh,
                        });
                    }
                    Err(error) => {
                        last_error = Some((key_path, error));
                    }
                }
            }

            if let Some((key_path, error)) = last_error {
                bail!(
                    "Failed to authenticate to {SSH_HOST} as {service_instance_id} using {}: {error}",
                    key_path.display()
                );
            }

            bail!(
                "No SSH private keys found. Generate one with:\n  ssh-keygen -t ed25519\n\nThen register it with Railway."
            );
        })
        .context("Failed to connect to Railway volume over SFTP")
    }

    pub fn list_dir(&self, path: &Path) -> Result<Vec<VolumeFileEntry>> {
        block_on(self.list_dir_async(path))
    }

    pub fn exists(&self, path: &Path) -> Result<bool> {
        block_on(self.exists_async(path))
    }

    pub fn stat_kind(&self, path: &Path) -> Result<VolumeFileKind> {
        block_on(self.stat_kind_async(path))
    }

    pub fn remove_path(&self, path: &Path) -> Result<()> {
        block_on(self.remove_path_async(path))
    }

    pub fn download(&self, remote: &Path, local: &Path, kind: VolumeFileKind) -> Result<()> {
        block_on(async {
            if kind.is_dir() {
                self.download_dir(remote, local).await
            } else {
                self.download_file(remote, local).await
            }
        })
    }

    pub fn upload(&self, local: &Path, remote: &Path) -> Result<()> {
        block_on(async {
            if local.is_dir() {
                self.upload_dir(local, remote).await
            } else {
                self.upload_file(local, remote).await
            }
        })
    }

    async fn remove_path_async(&self, path: &Path) -> Result<()> {
        match self.stat_kind_async(path).await? {
            VolumeFileKind::Directory => {
                for entry in self.list_dir_async(path).await? {
                    Box::pin(self.remove_path_async(&entry.path)).await?;
                }
                self.sftp
                    .remove_dir(remote_path(path))
                    .await
                    .with_context(|| {
                        format!("Failed to remove remote directory {}", path.display())
                    })
            }
            _ => self
                .sftp
                .remove_file(remote_path(path))
                .await
                .with_context(|| format!("Failed to remove remote file {}", path.display())),
        }
    }

    async fn create_dir_all_async(&self, path: &Path) -> Result<()> {
        let mut current = PathBuf::new();

        for component in path.components() {
            current.push(component.as_os_str());
            if current.as_os_str().is_empty() || current == Path::new("/") {
                continue;
            }
            if !self.exists_async(&current).await? {
                self.sftp
                    .create_dir(remote_path(&current))
                    .await
                    .with_context(|| {
                        format!("Failed to create remote directory {}", current.display())
                    })?;
            }
        }

        Ok(())
    }

    async fn exists_async(&self, path: &Path) -> Result<bool> {
        match self.sftp.symlink_metadata(remote_path(path)).await {
            Ok(_) => Ok(true),
            Err(error) if is_missing_remote_error(&error) => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("Failed to check remote path {}", path.display())),
        }
    }

    async fn list_dir_async(&self, path: &Path) -> Result<Vec<VolumeFileEntry>> {
        let mut entries = Vec::new();
        for entry in self
            .sftp
            .read_dir(remote_path(path))
            .await
            .with_context(|| format!("Failed to list remote directory {}", path.display()))?
        {
            let name = entry.file_name();
            entries.push(VolumeFileEntry {
                path: path.join(&name),
                name,
                kind: kind_from_file_type(entry.file_type()),
            });
        }
        entries.sort_by(|a, b| {
            b.kind
                .is_dir()
                .cmp(&a.kind.is_dir())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(entries)
    }

    async fn stat_kind_async(&self, path: &Path) -> Result<VolumeFileKind> {
        let metadata = self
            .sftp
            .symlink_metadata(remote_path(path))
            .await
            .with_context(|| format!("Remote path does not exist: {}", path.display()))?;
        Ok(kind_from_file_type(metadata.file_type()))
    }

    async fn download_file(&self, remote: &Path, local: &Path) -> Result<()> {
        if let Some(parent) = local.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!("Failed to create local directory {}", parent.display())
            })?;
        }

        let mut remote_file = self
            .sftp
            .open(remote_path(remote))
            .await
            .with_context(|| format!("Failed to open remote file {}", remote.display()))?;
        let mut local_file = File::create(local)
            .await
            .with_context(|| format!("Failed to create local file {}", local.display()))?;

        tokio::io::copy(&mut remote_file, &mut local_file)
            .await
            .with_context(|| {
                format!(
                    "Failed to download remote file {} to {}",
                    remote.display(),
                    local.display()
                )
            })?;
        local_file
            .flush()
            .await
            .with_context(|| format!("Failed to flush local file {}", local.display()))?;

        Ok(())
    }

    async fn download_dir(&self, remote: &Path, local: &Path) -> Result<()> {
        let created_root = !tokio::fs::try_exists(local)
            .await
            .with_context(|| format!("Failed to inspect local directory {}", local.display()))?;
        tokio::fs::create_dir_all(local)
            .await
            .with_context(|| format!("Failed to create local directory {}", local.display()))?;

        let result = async {
            let entries = self.list_dir_async(remote).await?;
            let (dirs, files): (Vec<_>, Vec<_>) =
                entries.into_iter().partition(|entry| entry.kind.is_dir());

            stream::iter(files)
                .map(|entry| async move {
                    let child_local = local.join(&entry.name);
                    self.download_file(&entry.path, &child_local).await
                })
                .buffer_unordered(DIRECTORY_FILE_CONCURRENCY)
                .try_collect::<Vec<_>>()
                .await?;

            stream::iter(dirs)
                .map(|entry| async move {
                    let child_local = local.join(&entry.name);
                    self.download_dir(&entry.path, &child_local).await
                })
                .buffer_unordered(DIRECTORY_SUBDIR_CONCURRENCY)
                .try_collect::<Vec<_>>()
                .await?;

            Ok(())
        }
        .await;

        if let Err(error) = result {
            if created_root {
                let _ = tokio::fs::remove_dir_all(local).await;
            }
            return Err(error);
        }

        Ok(())
    }

    async fn upload_file(&self, local: &Path, remote: &Path) -> Result<()> {
        if let Some(parent) = remote.parent() {
            self.create_dir_all_async(parent).await?;
        }

        let mut local_file = File::open(local)
            .await
            .with_context(|| format!("Failed to open local file {}", local.display()))?;
        let mut remote_file = self
            .sftp
            .create(remote_path(remote))
            .await
            .with_context(|| format!("Failed to create remote file {}", remote.display()))?;

        tokio::io::copy(&mut local_file, &mut remote_file)
            .await
            .with_context(|| {
                format!(
                    "Failed to upload local file {} to {}",
                    local.display(),
                    remote.display()
                )
            })?;
        remote_file
            .flush()
            .await
            .with_context(|| format!("Failed to flush remote file {}", remote.display()))?;
        remote_file
            .shutdown()
            .await
            .with_context(|| format!("Failed to close remote file {}", remote.display()))?;

        Ok(())
    }

    async fn upload_dir(&self, local: &Path, remote: &Path) -> Result<()> {
        let created_root = !self
            .exists_async(remote)
            .await
            .with_context(|| format!("Failed to inspect remote directory {}", remote.display()))?;
        self.create_dir_all_async(remote).await?;

        let result =
            async {
                let mut read_dir = tokio::fs::read_dir(local).await.with_context(|| {
                    format!("Failed to read local directory {}", local.display())
                })?;
                let mut dirs = Vec::new();
                let mut files = Vec::new();

                while let Some(entry) = read_dir.next_entry().await.with_context(|| {
                    format!("Failed to read local directory {}", local.display())
                })? {
                    let file_type = entry.file_type().await.with_context(|| {
                        format!("Failed to inspect local path {}", entry.path().display())
                    })?;
                    if file_type.is_dir() {
                        dirs.push((entry.path(), remote.join(entry.file_name())));
                    } else if file_type.is_file() {
                        files.push((entry.path(), remote.join(entry.file_name())));
                    }
                }

                stream::iter(files)
                    .map(|(local_path, remote_path)| async move {
                        self.upload_file(&local_path, &remote_path).await
                    })
                    .buffer_unordered(DIRECTORY_FILE_CONCURRENCY)
                    .try_collect::<Vec<_>>()
                    .await?;

                stream::iter(dirs)
                    .map(|(local_path, remote_path)| async move {
                        self.upload_dir(&local_path, &remote_path).await
                    })
                    .buffer_unordered(DIRECTORY_SUBDIR_CONCURRENCY)
                    .try_collect::<Vec<_>>()
                    .await?;

                Ok(())
            }
            .await;

        if let Err(error) = result {
            if created_root {
                let _ = self.remove_path_async(remote).await;
            }
            return Err(error);
        }

        Ok(())
    }
}

impl Drop for VolumeFileClient {
    fn drop(&mut self) {
        let _ = block_on(async {
            let _ = self.sftp.close().await;
            self.ssh
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await?;
            Ok::<_, anyhow::Error>(())
        });
    }
}

#[derive(Clone)]
struct RailwaySshClient;

impl client::Handler for RailwaySshClient {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

async fn connect_with_key(
    service_instance_id: &str,
    key_path: &Path,
) -> Result<(client::Handle<RailwaySshClient>, SftpSession)> {
    let key_pair = load_key(key_path)
        .with_context(|| format!("Failed to load SSH key {}", key_path.display()))?;
    let config = client::Config {
        inactivity_timeout: Some(Duration::from_secs(30)),
        preferred: Preferred {
            kex: Cow::Owned(vec![
                russh::kex::CURVE25519_PRE_RFC_8731,
                russh::kex::EXTENSION_SUPPORT_AS_CLIENT,
            ]),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut ssh = client::connect(Arc::new(config), (SSH_HOST, SSH_PORT), RailwaySshClient {})
        .await
        .context("Failed to open SSH connection")?;

    let auth_result = ssh
        .authenticate_publickey(
            service_instance_id,
            PrivateKeyWithHashAlg::new(
                Arc::new(key_pair),
                ssh.best_supported_rsa_hash().await?.flatten(),
            ),
        )
        .await
        .context("Failed to authenticate with SSH key")?;
    if !auth_result.success() {
        bail!("SSH key was rejected by Railway");
    }

    let channel = ssh
        .channel_open_session()
        .await
        .context("Failed to open SSH session channel")?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .context("Failed to start SFTP subsystem")?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .context("Failed to initialize SFTP session")?;

    Ok((ssh, sftp))
}

fn resolve_private_key_paths(identity_file: Option<PathBuf>) -> Result<Vec<PathBuf>> {
    if let Some(identity_file) = identity_file {
        return Ok(vec![expand_tilde(identity_file)]);
    }

    let mut paths = Vec::new();
    for key in find_local_ssh_keys()? {
        let private_key = public_to_private_key_path(&key.path);
        if private_key.exists() {
            paths.push(private_key);
        }
    }

    Ok(paths)
}

fn load_key(path: &Path) -> Result<russh::keys::PrivateKey> {
    match load_secret_key(path, None) {
        Ok(key) => Ok(key),
        Err(error) if std::io::stdin().is_terminal() => {
            let passphrase =
                Password::new(&format!("Enter passphrase for key '{}':", path.display()))
                    .without_confirmation()
                    .prompt()
                    .context("Failed to read SSH key passphrase")?;

            load_secret_key(path, Some(&passphrase)).map_err(|passphrase_error| {
                anyhow!(
                    "Failed to decrypt SSH key with passphrase: {passphrase_error}; initial error: {error}"
                )
            })
        }
        Err(error) => Err(error.into()),
    }
}

fn public_to_private_key_path(public_key: &Path) -> PathBuf {
    match public_key.file_name().and_then(|name| name.to_str()) {
        Some(name) if name.ends_with(".pub") => public_key.with_file_name(&name[..name.len() - 4]),
        _ => public_key.to_path_buf(),
    }
}

fn expand_tilde(path: PathBuf) -> PathBuf {
    let Some(path_str) = path.to_str() else {
        return path;
    };
    if path_str == "~" {
        return dirs::home_dir().unwrap_or(path);
    }
    if let Some(rest) = path_str.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    path
}

fn remote_path(path: &Path) -> String {
    path.display().to_string()
}

fn kind_from_file_type(file_type: russh_sftp::protocol::FileType) -> VolumeFileKind {
    if file_type.is_dir() {
        VolumeFileKind::Directory
    } else if file_type.is_file() {
        VolumeFileKind::File
    } else if file_type.is_symlink() {
        VolumeFileKind::Symlink
    } else {
        VolumeFileKind::Other
    }
}

fn is_missing_remote_error(error: &SftpError) -> bool {
    match error {
        SftpError::Status(status) if status.status_code == StatusCode::NoSuchFile => true,
        SftpError::Status(status) if status.status_code == StatusCode::Failure => status
            .error_message
            .to_ascii_lowercase()
            .contains("no such file or directory"),
        _ => false,
    }
}

fn block_on<F, T>(future: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Runtime::new()
            .context("Failed to create async runtime")?
            .block_on(future)
    }
}
