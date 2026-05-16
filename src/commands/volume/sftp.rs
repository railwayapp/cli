use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use thiserror::Error;

pub(crate) struct VolumeSftp {
    service_instance_id: String,
    mount_path: String,
    session: Option<russh::client::Handle<VolumeSftpHandler>>,
    sftp: Option<russh_sftp::client::SftpSession>,
    disconnected: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub(crate) struct VolumeFileEntry {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) kind: &'static str,
    pub(crate) size: u64,
}

pub(crate) struct VolumeFileTree {
    entries: Vec<VolumeFileEntry>,
}

impl VolumeFileTree {
    fn new(mut entries: Vec<VolumeFileEntry>) -> Self {
        entries.sort_by_key(|entry| entry.name.to_lowercase());
        Self { entries }
    }

    pub(crate) fn entries(&self) -> &[VolumeFileEntry] {
        &self.entries
    }
}

impl fmt::Display for VolumeFileTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const COLUMN_GAP: usize = 2;
        const MAX_WIDTH: usize = 80;

        let names = self
            .entries
            .iter()
            .map(|entry| match entry.kind {
                "directory" => format!("{}/", entry.name),
                _ => entry.name.clone(),
            })
            .collect::<Vec<_>>();
        let name_width = names.iter().map(String::len).max().unwrap_or(0);
        let column_width = name_width + COLUMN_GAP;
        let columns = if column_width == 0 {
            1
        } else {
            (MAX_WIDTH / column_width).max(1)
        };

        for row in self.entries.chunks(columns).zip(names.chunks(columns)) {
            let (entries, names) = row;
            for (index, (entry, name)) in entries.iter().zip(names).enumerate() {
                let styled_name = match entry.kind {
                    "directory" => name.blue().bold().to_string(),
                    "symlink" => name.cyan().to_string(),
                    _ => name.clone(),
                };

                if index + 1 == entries.len() {
                    write!(f, "{styled_name}")?;
                } else {
                    write!(
                        f,
                        "{styled_name:<width$}",
                        width = column_width + styled_name.len() - name.len()
                    )?;
                }
            }
            writeln!(f)?;
        }

        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum VolumeSftpError {
    #[error(
        "Local path {0} already exists. Use --overwrite (or --override) to replace it, or choose a different LOCAL_PATH."
    )]
    LocalPathExists(PathBuf),
    #[error(
        "Remote path {0} already exists. Use --overwrite to replace it, or choose a different REMOTE_PATH."
    )]
    RemotePathExists(String),
}

struct VolumeSftpHandler {
    disconnected: Arc<AtomicBool>,
}

impl VolumeSftpHandler {
    fn new(disconnected: Arc<AtomicBool>) -> Self {
        Self { disconnected }
    }
}

const ADDR: &str = "ssh.railway.com";

impl russh::client::Handler for VolumeSftpHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        // no idea if Railway has a pre-defined list of server keys, can help prevent mitm attacks
        Ok(true)
    }

    async fn disconnected(
        &mut self,
        reason: russh::client::DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        self.disconnected.store(true, Ordering::SeqCst);

        match reason {
            russh::client::DisconnectReason::ReceivedDisconnect(_) => Ok(()),
            russh::client::DisconnectReason::Error(err) => Err(err),
        }
    }
}

impl VolumeSftp {
    pub(crate) fn new(service_instance_id: String, mount_path: String) -> Self {
        Self {
            service_instance_id,
            mount_path,
            session: None,
            sftp: None,
            disconnected: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) async fn connect(&mut self) -> Result<&russh_sftp::client::SftpSession> {
        if self.session.is_none() || self.is_disconnected() {
            self.disconnected.store(false, Ordering::SeqCst);

            let mut session = russh::client::connect(
                Arc::new(russh::client::Config::default()),
                (ADDR, 22),
                VolumeSftpHandler::new(Arc::clone(&self.disconnected)),
            )
            .await
            .with_context(|| format!("Failed to connect to Railway SFTP at {ADDR}"))?;

            super::ssh_key::authenticate(&mut session, &self.service_instance_id).await?;

            let channel = session
                .channel_open_session()
                .await
                .context("Failed to open SSH session channel")?;

            channel
                .request_subsystem(true, "sftp")
                .await
                .context("Failed to request SFTP subsystem")?;

            let sftp = russh_sftp::client::SftpSession::new(channel.into_stream())
                .await
                .context("Failed to initialize SFTP session")?;

            self.session = Some(session);
            self.sftp = Some(sftp);
        }

        self.sftp.as_ref().with_context(|| {
            format!(
                "SFTP session is not connected for service instance {}",
                self.service_instance_id
            )
        })
    }

    pub(crate) async fn download(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        overwrite: bool,
    ) -> Result<PathBuf> {
        let local_path = Self::download_destination(remote_path, local_path)?;

        match self
            .download_once(remote_path, &local_path, overwrite)
            .await
        {
            Ok(()) => Ok(local_path),
            Err(_err) if self.is_disconnected() => self
                .download_once(remote_path, &local_path, overwrite)
                .await
                .with_context(|| format!("Failed to download {remote_path} after reconnect"))
                .map(|()| local_path),
            Err(err) => Err(err),
        }
    }

    pub(crate) async fn upload(
        &mut self,
        local_path: &Path,
        remote_path: &str,
        overwrite: bool,
    ) -> Result<String> {
        let remote_path = Self::upload_destination(local_path, remote_path)?;

        match self.upload_once(local_path, &remote_path, overwrite).await {
            Ok(()) => Ok(remote_path),
            Err(_err) if self.is_disconnected() => self
                .upload_once(local_path, &remote_path, overwrite)
                .await
                .with_context(|| {
                    format!("Failed to upload {} after reconnect", local_path.display())
                })
                .map(|()| remote_path),
            Err(err) => Err(err),
        }
    }

    pub(crate) async fn delete_file(&mut self, remote_path: &str) -> Result<()> {
        match self.delete_file_once(remote_path).await {
            Ok(()) => Ok(()),
            Err(_err) if self.is_disconnected() => self
                .delete_file_once(remote_path)
                .await
                .with_context(|| format!("Failed to delete {remote_path} after reconnect")),
            Err(err) => Err(err),
        }
    }

    pub(crate) async fn rename(&mut self, old_path: &str, new_path: &str) -> Result<()> {
        match self.rename_once(old_path, new_path).await {
            Ok(()) => Ok(()),
            Err(_err) if self.is_disconnected() => self
                .rename_once(old_path, new_path)
                .await
                .with_context(|| format!("Failed to rename {old_path} after reconnect")),
            Err(err) => Err(err),
        }
    }

    pub(crate) async fn list_files(&mut self, remote_path: &str) -> Result<VolumeFileTree> {
        match self.list_files_once(remote_path).await {
            Ok(entries) => Ok(VolumeFileTree::new(entries)),
            Err(_err) if self.is_disconnected() => self
                .list_files_once(remote_path)
                .await
                .with_context(|| format!("Failed to list {remote_path} after reconnect"))
                .map(VolumeFileTree::new),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn download_destination(remote_path: &str, local_path: &Path) -> Result<PathBuf> {
        if local_path.is_dir() {
            let filename = remote_path
                .rsplit('/')
                .find(|segment| !segment.is_empty())
                .ok_or_else(|| anyhow!("Could not infer a local filename from {remote_path}"))?;
            Ok(local_path.join(filename))
        } else {
            Ok(local_path.to_path_buf())
        }
    }

    pub(crate) fn upload_destination(local_path: &Path, remote_path: &str) -> Result<String> {
        if remote_path.ends_with('/') {
            let filename = local_path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    anyhow!(
                        "Could not infer a remote filename from local path {}",
                        local_path.display()
                    )
                })?;
            Ok(format!("{remote_path}{filename}"))
        } else {
            Ok(remote_path.to_string())
        }
    }

    fn is_disconnected(&self) -> bool {
        self.disconnected.load(Ordering::SeqCst)
            || self
                .session
                .as_ref()
                .is_some_and(russh::client::Handle::is_closed)
    }

    async fn download_once(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        overwrite: bool,
    ) -> Result<()> {
        let remote_path = self.mount_prefixed_path(remote_path);
        let sftp = self.connect().await?;
        let mut remote_file = sftp
            .open(&remote_path)
            .await
            .with_context(|| format!("Failed to open remote file {remote_path}"))?;

        let local_path_exists = tokio::fs::try_exists(local_path).await.with_context(|| {
            format!(
                "Failed to check if local file {} exists",
                local_path.display()
            )
        })?;

        if local_path_exists && !overwrite {
            return Err(VolumeSftpError::LocalPathExists(local_path.to_path_buf()).into());
        }

        let mut local_file = tokio::fs::File::create(local_path)
            .await
            .with_context(|| format!("Failed to create local file {}", local_path.display()))?;

        tokio::io::copy(&mut remote_file, &mut local_file)
            .await
            .with_context(|| {
                format!(
                    "Failed to copy remote file {remote_path} to local file {}",
                    local_path.display()
                )
            })?;

        Ok(())
    }

    async fn upload_once(
        &mut self,
        local_path: &Path,
        remote_path: &str,
        overwrite: bool,
    ) -> Result<()> {
        let remote_path = self.mount_prefixed_path(remote_path);
        let sftp = self.connect().await?;
        let remote_path_exists = sftp
            .try_exists(&remote_path)
            .await
            .with_context(|| format!("Failed to check if remote file {remote_path} exists"))?;

        if remote_path_exists && !overwrite {
            return Err(VolumeSftpError::RemotePathExists(remote_path).into());
        }

        let mut local_file = tokio::fs::File::open(local_path)
            .await
            .with_context(|| format!("Failed to open local file {}", local_path.display()))?;
        let mut remote_file = sftp
            .create(&remote_path)
            .await
            .with_context(|| format!("Failed to create remote file {remote_path}"))?;

        tokio::io::copy(&mut local_file, &mut remote_file)
            .await
            .with_context(|| {
                format!(
                    "Failed to copy local file {} to remote file {remote_path}",
                    local_path.display()
                )
            })?;

        Ok(())
    }

    async fn delete_file_once(&mut self, remote_path: &str) -> Result<()> {
        let remote_path = self.mount_prefixed_path(remote_path);
        let sftp = self.connect().await?;
        sftp.remove_file(&remote_path)
            .await
            .with_context(|| format!("Failed to delete remote file {remote_path}"))?;

        Ok(())
    }

    async fn rename_once(&mut self, old_path: &str, new_path: &str) -> Result<()> {
        let old_path = self.mount_prefixed_path(old_path);
        let new_path = self.mount_prefixed_path(new_path);
        let sftp = self.connect().await?;
        sftp.rename(&old_path, &new_path)
            .await
            .with_context(|| format!("Failed to rename remote file {old_path} to {new_path}"))?;

        Ok(())
    }

    async fn list_files_once(&mut self, remote_path: &str) -> Result<Vec<VolumeFileEntry>> {
        let mounted_remote_path = self.mount_prefixed_path(remote_path);
        let sftp = self.connect().await?;
        let entries = sftp
            .read_dir(&mounted_remote_path)
            .await
            .with_context(|| format!("Failed to list remote directory {mounted_remote_path}"))?
            .map(|entry| {
                let metadata = entry.metadata();
                let name = entry.file_name();
                VolumeFileEntry {
                    path: Self::join_remote_path(remote_path, &name),
                    name,
                    kind: match metadata {
                        _ if metadata.is_dir() => "directory",
                        _ if metadata.is_regular() => "file",
                        _ if metadata.is_symlink() => "symlink",
                        _ => "other",
                    },
                    size: metadata.len(),
                }
            })
            .collect();

        Ok(entries)
    }

    fn join_remote_path(parent: &str, name: &str) -> String {
        let parent = parent.trim_end_matches('/');
        if parent.is_empty() || parent == "/" {
            format!("/{name}")
        } else {
            format!("{parent}/{name}")
        }
    }

    // crazy that Rust has no std library that handles UnixPaths exclusively
    fn mount_prefixed_path(&self, path: &str) -> String {
        let mount_path = self.mount_path.trim_end_matches('/');
        if mount_path.is_empty() || mount_path == "/" {
            return format!("/{}", path.trim_start_matches('/'));
        }

        if path == mount_path
            || path
                .strip_prefix(mount_path)
                .is_some_and(|suffix| suffix.starts_with('/'))
        {
            return path.to_string();
        }

        let path = path.trim_start_matches('/');
        if path.is_empty() {
            mount_path.to_string()
        } else {
            format!("{mount_path}/{path}")
        }
    }
}
