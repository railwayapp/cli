use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use colored::Colorize;
use futures_util::{StreamExt, TryStreamExt, stream};
use russh_sftp::{
    client::error::Error as SftpClientError, client::fs::Metadata, protocol::StatusCode,
};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

pub(crate) struct VolumeSftp {
    service_instance_id: String,
    mount_path: String,
    session: Option<russh::client::Handle<VolumeSftpHandler>>,
    sftp: Option<russh_sftp::client::SftpSession>,
    disconnected: Arc<AtomicBool>,
    transfer_concurrency: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct VolumeFileEntry {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) kind: &'static str,
    pub(crate) size: u64,
    pub(crate) modified_at: Option<DateTime<Utc>>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalOverwritePolicy {
    None,
    Path(PathBuf),
    All,
}

impl LocalOverwritePolicy {
    pub(crate) fn from_bool(overwrite: bool) -> Self {
        if overwrite { Self::All } else { Self::None }
    }

    fn allows(&self, path: &Path) -> bool {
        match self {
            Self::None => false,
            Self::Path(allowed_path) => allowed_path == path,
            Self::All => true,
        }
    }
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
pub(crate) const DEFAULT_TRANSFER_CONCURRENCY: usize = 32;
const DOWNLOAD_TRANSFER_BUFFER_SIZE: usize = 2 * 1024 * 1024;
const DIRECTORY_UPLOAD_TRANSFER_BUFFER_SIZE: usize = 2 * 1024 * 1024;
const UPLOAD_TRANSFER_BUFFER_SIZE: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct VolumeTransferProgress {
    pub(crate) current_path: String,
    pub(crate) completed: usize,
    pub(crate) total: usize,
}

pub(crate) type VolumeTransferProgressCallback = Arc<dyn Fn(VolumeTransferProgress) + Send + Sync>;

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
            transfer_concurrency: DEFAULT_TRANSFER_CONCURRENCY,
        }
    }

    pub(crate) fn set_transfer_concurrency(&mut self, transfer_concurrency: usize) {
        self.transfer_concurrency = transfer_concurrency.max(1);
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

            crate::controllers::ssh::authenticate(&mut session, &self.service_instance_id).await?;

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
        if self.stat(remote_path).await?.is_dir() {
            return self.download_dir(remote_path, local_path, overwrite).await;
        }

        self.download_file(remote_path, local_path, overwrite).await
    }

    pub(crate) async fn download_file(
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

    pub(crate) async fn download_dir(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        overwrite: bool,
    ) -> Result<PathBuf> {
        self.download_dir_with_progress(remote_path, local_path, overwrite, None)
            .await
    }

    pub(crate) async fn download_dir_with_progress(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        overwrite: bool,
        progress: Option<VolumeTransferProgressCallback>,
    ) -> Result<PathBuf> {
        self.download_dir_with_progress_and_overwrite_policy(
            remote_path,
            local_path,
            LocalOverwritePolicy::from_bool(overwrite),
            progress,
        )
        .await
    }

    pub(crate) async fn download_dir_with_progress_and_overwrite_policy(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        overwrite_policy: LocalOverwritePolicy,
        progress: Option<VolumeTransferProgressCallback>,
    ) -> Result<PathBuf> {
        let local_path = Self::download_destination(remote_path, local_path)?;

        match self
            .download_dir_once(
                remote_path,
                &local_path,
                &overwrite_policy,
                progress.clone(),
            )
            .await
        {
            Ok(()) => Ok(local_path),
            Err(_err) if self.is_disconnected() => self
                .download_dir_once(remote_path, &local_path, &overwrite_policy, progress)
                .await
                .with_context(|| {
                    format!("Failed to download directory {remote_path} after reconnect")
                })
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
        let local_metadata = tokio::fs::metadata(local_path)
            .await
            .with_context(|| format!("Failed to stat local path {}", local_path.display()))?;

        if local_metadata.is_dir() {
            return self.upload_dir(local_path, remote_path, overwrite).await;
        }

        let remote_path_is_dir = self.remote_path_is_dir(remote_path).await?;
        let remote_path = Self::upload_destination(local_path, remote_path, remote_path_is_dir)?;

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

    pub(crate) async fn upload_dir(
        &mut self,
        local_path: &Path,
        remote_path: &str,
        overwrite: bool,
    ) -> Result<String> {
        let remote_path_is_dir = self.remote_path_is_dir(remote_path).await?;
        let remote_path = Self::upload_destination(local_path, remote_path, remote_path_is_dir)?;

        match self
            .upload_dir_once(local_path, &remote_path, overwrite)
            .await
        {
            Ok(()) => Ok(remote_path),
            Err(_err) if self.is_disconnected() => self
                .upload_dir_once(local_path, &remote_path, overwrite)
                .await
                .with_context(|| {
                    format!(
                        "Failed to upload directory {} after reconnect",
                        local_path.display()
                    )
                })
                .map(|()| remote_path),
            Err(err) => Err(err),
        }
    }

    pub(crate) async fn delete(&mut self, remote_path: &str) -> Result<()> {
        match self.delete_once(remote_path).await {
            Ok(()) => Ok(()),
            Err(_err) if self.is_disconnected() => self
                .delete_once(remote_path)
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

    pub(crate) async fn stat(&mut self, remote_path: &str) -> Result<Metadata> {
        match self.stat_once(remote_path).await {
            Ok(metadata) => Ok(metadata),
            Err(_err) if self.is_disconnected() => self
                .stat_once(remote_path)
                .await
                .with_context(|| format!("Failed to stat {remote_path} after reconnect")),
            Err(err) => Err(err),
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
        Self::download_file_with_sftp(sftp, &remote_path, local_path, overwrite).await
    }

    async fn download_file_with_sftp(
        sftp: &russh_sftp::client::SftpSession,
        remote_path: &str,
        local_path: &Path,
        overwrite: bool,
    ) -> Result<()> {
        let remote_file = sftp
            .open(remote_path)
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

        let local_file = tokio::fs::File::create(local_path)
            .await
            .with_context(|| format!("Failed to create local file {}", local_path.display()))?;

        let mut remote_file =
            tokio::io::BufReader::with_capacity(DOWNLOAD_TRANSFER_BUFFER_SIZE, remote_file);
        let mut local_file =
            tokio::io::BufWriter::with_capacity(DOWNLOAD_TRANSFER_BUFFER_SIZE, local_file);

        tokio::io::copy_buf(&mut remote_file, &mut local_file)
            .await
            .with_context(|| {
                format!(
                    "Failed to copy remote file {remote_path} to local file {}",
                    local_path.display()
                )
            })?;
        local_file
            .flush()
            .await
            .with_context(|| format!("Failed to flush local file {}", local_path.display()))?;

        Ok(())
    }

    async fn upload_once(
        &mut self,
        local_path: &Path,
        remote_path: &str,
        overwrite: bool,
    ) -> Result<()> {
        let remote_path = self.mount_prefixed_path(remote_path);

        if let Some(parent) = Self::parent_remote_path(&remote_path) {
            self.create_remote_dir(&parent, true).await?;
        }

        let sftp = self.connect().await?;
        Self::upload_file_with_sftp(
            sftp,
            local_path,
            &remote_path,
            overwrite,
            UPLOAD_TRANSFER_BUFFER_SIZE,
        )
        .await
    }

    async fn upload_file_with_sftp(
        sftp: &russh_sftp::client::SftpSession,
        local_path: &Path,
        remote_path: &str,
        overwrite: bool,
        buffer_size: usize,
    ) -> Result<()> {
        let remote_path_exists = Self::remote_path_exists(sftp, remote_path)
            .await
            .with_context(|| format!("Failed to check if remote file {remote_path} exists"))?;

        if remote_path_exists && !overwrite {
            return Err(VolumeSftpError::RemotePathExists(remote_path.to_string()).into());
        }

        let local_file = tokio::fs::File::open(local_path)
            .await
            .with_context(|| format!("Failed to open local file {}", local_path.display()))?;
        let mut remote_file = sftp
            .create(remote_path)
            .await
            .with_context(|| format!("Failed to create remote file {remote_path}"))?;

        let mut local_file = tokio::io::BufReader::with_capacity(buffer_size, local_file);

        tokio::io::copy_buf(&mut local_file, &mut remote_file)
            .await
            .with_context(|| {
                format!(
                    "Failed to copy local file {} to remote file {remote_path}",
                    local_path.display()
                )
            })?;
        remote_file
            .flush()
            .await
            .with_context(|| format!("Failed to flush remote file {remote_path}"))?;

        Ok(())
    }

    async fn download_dir_once(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        overwrite_policy: &LocalOverwritePolicy,
        progress: Option<VolumeTransferProgressCallback>,
    ) -> Result<()> {
        let local_path_exists = tokio::fs::try_exists(local_path).await.with_context(|| {
            format!(
                "Failed to check if local path {} exists",
                local_path.display()
            )
        })?;

        if local_path_exists && !local_path.is_dir() {
            if !overwrite_policy.allows(local_path) {
                return Err(VolumeSftpError::LocalPathExists(local_path.to_path_buf()).into());
            }
            tokio::fs::remove_file(local_path)
                .await
                .with_context(|| format!("Failed to remove local file {}", local_path.display()))?;
        }

        tokio::fs::create_dir_all(local_path)
            .await
            .with_context(|| {
                format!("Failed to create local directory {}", local_path.display())
            })?;

        let mut pending = vec![(remote_path.to_string(), local_path.to_path_buf())];
        let mut files = Vec::new();

        while let Some((remote_dir, local_dir)) = pending.pop() {
            for entry in self.list_files_once(&remote_dir).await? {
                let local_entry_path = local_dir.join(&entry.name);

                if entry.kind == "directory" {
                    tokio::fs::create_dir_all(&local_entry_path)
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to create local directory {}",
                                local_entry_path.display()
                            )
                        })?;
                    pending.push((entry.path, local_entry_path));
                } else {
                    files.push((
                        entry.path.clone(),
                        self.mount_prefixed_path(&entry.path),
                        local_entry_path,
                    ));
                }
            }
        }

        if let LocalOverwritePolicy::Path(overwrite_path) = overwrite_policy {
            files.sort_by_key(|(_, _, local_file)| local_file != overwrite_path);
        }

        if !files.is_empty() {
            let total = files.len();
            let completed = Arc::new(AtomicUsize::new(0));
            let concurrency = if matches!(overwrite_policy, LocalOverwritePolicy::Path(_)) {
                1
            } else {
                self.transfer_concurrency
            };
            let sftp = self.connect().await?;

            stream::iter(files)
                .map(|(display_remote_file, remote_file, local_file)| {
                    let completed = Arc::clone(&completed);
                    let progress = progress.clone();
                    let overwrite = overwrite_policy.allows(&local_file);

                    async move {
                        if let Some(progress) = &progress {
                            progress(VolumeTransferProgress {
                                current_path: display_remote_file.clone(),
                                completed: completed.load(Ordering::SeqCst),
                                total,
                            });
                        }

                        Self::download_file_with_sftp(sftp, &remote_file, &local_file, overwrite)
                            .await?;

                        let completed = completed.fetch_add(1, Ordering::SeqCst) + 1;
                        if let Some(progress) = &progress {
                            progress(VolumeTransferProgress {
                                current_path: display_remote_file,
                                completed,
                                total,
                            });
                        }

                        Ok::<(), anyhow::Error>(())
                    }
                })
                .buffer_unordered(concurrency)
                .try_collect::<Vec<_>>()
                .await?;
        }

        Ok(())
    }

    async fn upload_dir_once(
        &mut self,
        local_path: &Path,
        remote_path: &str,
        overwrite: bool,
    ) -> Result<()> {
        self.create_remote_dir(remote_path, overwrite).await?;

        let mut pending = vec![(local_path.to_path_buf(), remote_path.to_string())];
        let mut files = Vec::new();

        while let Some((local_dir, remote_dir)) = pending.pop() {
            let mut entries = tokio::fs::read_dir(&local_dir).await.with_context(|| {
                format!("Failed to read local directory {}", local_dir.display())
            })?;

            while let Some(entry) = entries.next_entry().await.with_context(|| {
                format!(
                    "Failed to read local directory entry in {}",
                    local_dir.display()
                )
            })? {
                let local_entry_path = entry.path();
                let name = entry.file_name().into_string().map_err(|name| {
                    anyhow!(
                        "Could not infer a remote filename from local path {}",
                        PathBuf::from(name).display()
                    )
                })?;
                let remote_entry_path = Self::join_remote_path(&remote_dir, &name);
                let file_type = entry.file_type().await.with_context(|| {
                    format!(
                        "Failed to read local file type for {}",
                        local_entry_path.display()
                    )
                })?;

                if file_type.is_dir() {
                    self.create_remote_dir(&remote_entry_path, overwrite)
                        .await?;
                    pending.push((local_entry_path, remote_entry_path));
                } else {
                    files.push((
                        local_entry_path,
                        self.mount_prefixed_path(&remote_entry_path),
                    ));
                }
            }
        }

        if !files.is_empty() {
            let concurrency = self.transfer_concurrency;
            let sftp = self.connect().await?;

            stream::iter(files)
                .map(|(local_file, remote_file)| async move {
                    Self::upload_file_with_sftp(
                        sftp,
                        &local_file,
                        &remote_file,
                        overwrite,
                        DIRECTORY_UPLOAD_TRANSFER_BUFFER_SIZE,
                    )
                    .await
                })
                .buffer_unordered(concurrency)
                .try_collect::<Vec<_>>()
                .await?;
        }

        Ok(())
    }

    async fn create_remote_dir(&mut self, remote_path: &str, overwrite: bool) -> Result<()> {
        let remote_path = self.mount_prefixed_path(remote_path);
        let sftp = self.connect().await?;
        let remote_path_exists = Self::remote_path_exists(sftp, &remote_path)
            .await
            .with_context(|| format!("Failed to check if remote directory {remote_path} exists"))?;

        if remote_path_exists {
            if overwrite {
                return Ok(());
            }
            return Err(VolumeSftpError::RemotePathExists(remote_path).into());
        }

        sftp.create_dir(&remote_path)
            .await
            .with_context(|| format!("Failed to create remote directory {remote_path}"))?;

        Ok(())
    }

    async fn remote_path_exists(
        sftp: &russh_sftp::client::SftpSession,
        remote_path: &str,
    ) -> Result<bool, SftpClientError> {
        match sftp.try_exists(remote_path).await {
            Ok(exists) => Ok(exists),
            Err(err) if Self::is_not_found_status(&err) => Ok(false),
            Err(err) => Err(err),
        }
    }

    async fn remote_path_is_dir(&mut self, remote_path: &str) -> Result<bool> {
        let mounted_remote_path = self.mount_prefixed_path(remote_path);
        let sftp = self.connect().await?;

        match sftp.metadata(&mounted_remote_path).await {
            Ok(metadata) => Ok(metadata.is_dir()),
            Err(err) if Self::is_not_found_status(&err) => Ok(false),
            Err(err) => Err(err)
                .with_context(|| format!("Failed to stat remote path {mounted_remote_path}")),
        }
    }

    fn is_not_found_status(err: &SftpClientError) -> bool {
        matches!(
            err,
            SftpClientError::Status(status)
                if status.status_code == StatusCode::NoSuchFile
                    || (status.status_code == StatusCode::Failure
                        && status
                            .error_message
                            .to_lowercase()
                            .contains("no such file or directory"))
        )
    }

    async fn delete_once(&mut self, remote_path: &str) -> Result<()> {
        if self.remote_path_is_dir(remote_path).await? {
            return self.delete_dir_once(remote_path).await;
        }

        self.delete_file_once(remote_path).await
    }

    async fn delete_dir_once(&mut self, remote_path: &str) -> Result<()> {
        let mut pending = vec![remote_path.to_string()];
        let mut files = Vec::new();
        let mut dirs = Vec::new();

        while let Some(remote_dir) = pending.pop() {
            dirs.push(remote_dir.clone());

            for entry in self.list_files_once(&remote_dir).await? {
                if entry.kind == "directory" {
                    pending.push(entry.path);
                } else {
                    files.push(entry.path);
                }
            }
        }

        for file in files {
            self.delete_file_once(&file).await?;
        }

        for dir in dirs.into_iter().rev() {
            self.remove_empty_dir_once(&dir).await?;
        }

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

    async fn remove_empty_dir_once(&mut self, remote_path: &str) -> Result<()> {
        let remote_path = self.mount_prefixed_path(remote_path);
        let sftp = self.connect().await?;
        sftp.remove_dir(&remote_path)
            .await
            .with_context(|| format!("Failed to delete remote directory {remote_path}"))?;

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
                    modified_at: metadata.modified().ok().map(DateTime::<Utc>::from),
                }
            })
            .collect();

        Ok(entries)
    }

    async fn stat_once(&mut self, remote_path: &str) -> Result<Metadata> {
        let remote_path = self.mount_prefixed_path(remote_path);
        let sftp = self.connect().await?;
        sftp.metadata(&remote_path)
            .await
            .with_context(|| format!("Failed to stat remote path {remote_path}"))
    }

    fn join_remote_path(parent: &str, name: &str) -> String {
        let parent = parent.trim_end_matches('/');
        if parent.is_empty() || parent == "/" {
            format!("/{name}")
        } else {
            format!("{parent}/{name}")
        }
    }

    fn parent_remote_path(path: &str) -> Option<String> {
        let path = path.trim_end_matches('/');
        let (parent, _) = path.rsplit_once('/')?;
        if parent.is_empty() {
            None
        } else {
            Some(parent.to_string())
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

    // When downloading to a directory, infer the filename from the remote path.
    // Example: downloading /data/app.log into ./logs/ writes ./logs/app.log.
    fn download_destination(remote_path: &str, local_path: &Path) -> Result<PathBuf> {
        if !local_path.is_dir() {
            Ok(local_path.to_path_buf())
        } else {
            let filename = remote_path
                .rsplit('/')
                .find(|segment| !segment.is_empty())
                .ok_or_else(|| anyhow!("Could not infer a local filename from {remote_path}"))?;
            Ok(local_path.join(filename))
        }
    }

    // When uploading to a directory, infer the filename from the local path.
    // Example: uploading ./app.log into /data/ writes /data/app.log.
    fn upload_destination(
        local_path: &Path,
        remote_path: &str,
        remote_path_is_dir: bool,
    ) -> Result<String> {
        if !remote_path.ends_with('/') && !remote_path_is_dir {
            Ok(remote_path.to_string())
        } else {
            let filename = local_path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    anyhow!(
                        "Could not infer a remote filename from local path {}",
                        local_path.display()
                    )
                })?;
            Ok(Self::join_remote_path(remote_path, filename))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_destination_uses_exact_path_when_remote_is_not_directory() {
        assert_eq!(
            VolumeSftp::upload_destination(Path::new("dump.sql"), "/backups/dump.sql", false)
                .unwrap(),
            "/backups/dump.sql"
        );
    }

    #[test]
    fn upload_destination_appends_filename_for_trailing_slash() {
        assert_eq!(
            VolumeSftp::upload_destination(Path::new("dump.sql"), "/backups/", false).unwrap(),
            "/backups/dump.sql"
        );
    }

    #[test]
    fn upload_destination_appends_filename_for_existing_remote_directory_without_slash() {
        assert_eq!(
            VolumeSftp::upload_destination(Path::new("dump.sql"), "/backups", true).unwrap(),
            "/backups/dump.sql"
        );
    }
}
