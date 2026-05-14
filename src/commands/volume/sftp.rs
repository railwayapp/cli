use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result, anyhow};
use thiserror::Error;

pub(super) struct VolumeSftp {
    service_instance_id: String,
    mount_path: String,
    session: Option<russh::client::Handle<VolumeSftpHandler>>,
    sftp: Option<russh_sftp::client::SftpSession>,
    disconnected: Arc<AtomicBool>,
}

#[derive(Debug, Error)]
pub(super) enum VolumeSftpError {
    #[error(
        "Local path {0} already exists. Use --overwrite (or --override) to replace it, or choose a different LOCAL_PATH."
    )]
    LocalPathExists(PathBuf),
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
    pub(super) fn new(service_instance_id: String, mount_path: String) -> Self {
        Self {
            service_instance_id,
            mount_path,
            session: None,
            sftp: None,
            disconnected: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) async fn connect(&mut self) -> Result<&russh_sftp::client::SftpSession> {
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

    pub(super) async fn download(
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

    pub(super) fn download_destination(remote_path: &str, local_path: &Path) -> Result<PathBuf> {
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
