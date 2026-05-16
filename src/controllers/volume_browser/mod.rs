mod app;
mod ui;

use std::{
    io::stdout,
    panic,
    path::{Path, PathBuf},
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use crossterm::{
    cursor::{Hide, Show},
    event::{Event, EventStream, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;

use crate::commands::volume::sftp::{VolumeFileEntry, VolumeSftp, VolumeSftpError};

use app::{
    BrowserAction, BusyState, ConfirmAction, VolumeBrowserApp, normalize_remote_dir,
    parent_remote_dir,
};

pub struct VolumeBrowserParams {
    pub service_instance_id: String,
    pub volume_name: String,
    pub mount_path: String,
    pub remote_path: String,
}

struct RefreshResult {
    request_id: u64,
    remote_dir: String,
    result: Result<Vec<VolumeFileEntry>>,
}

pub async fn run(params: VolumeBrowserParams) -> Result<()> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    let mut app = VolumeBrowserApp::new(
        params.volume_name.clone(),
        params.mount_path.clone(),
        params.remote_path.clone(),
    )?;

    let initial_remote_dir = app.remote_dir.clone();
    let initial_entries = fetch_entries(&params, &initial_remote_dir)
        .await
        .with_context(|| format!("Failed to load remote directory {initial_remote_dir}"))?;
    app.apply_remote_entries(initial_entries);

    let mut terminal = setup_terminal()?;
    let _cleanup = scopeguard::guard((), |_| {
        restore_terminal();
    });

    let mut events = EventStream::new();
    let (refresh_tx, mut refresh_rx) = mpsc::unbounded_channel::<RefreshResult>();
    let mut refresh_request_id = 0u64;
    let mut active_refresh_request_id = 0u64;

    let mut render_interval = tokio::time::interval(std::time::Duration::from_millis(16));
    render_interval.tick().await;
    let mut dirty = true;

    'main: loop {
        tokio::select! {
            biased;
            _ = render_interval.tick(), if dirty || app.is_busy() => {
                terminal.draw(|frame| ui::render(&app, frame))?;
                dirty = false;
            }
            Some(refresh) = refresh_rx.recv() => {
                if refresh.request_id == active_refresh_request_id {
                    app.remote_dir = refresh.remote_dir;
                    match refresh.result {
                        Ok(entries) => app.apply_remote_entries(entries),
                        Err(err) => app.set_error(err.to_string()),
                    }
                    dirty = true;
                }
            }
            Some(Ok(event)) = events.next() => {
                match event {
                    Event::Key(key) => {
                        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                            continue;
                        }
                        match app.handle_key(key) {
                            BrowserAction::Continue => {}
                            BrowserAction::Quit => break 'main,
                            BrowserAction::Refresh => {
                                spawn_refresh(
                                    &refresh_tx,
                                    &params,
                                    &mut refresh_request_id,
                                    &mut active_refresh_request_id,
                                    app.remote_dir.clone(),
                                );
                                app.mark_refreshing();
                            }
                            BrowserAction::OpenRemoteDirectory(remote_dir) => {
                                spawn_refresh(
                                    &refresh_tx,
                                    &params,
                                    &mut refresh_request_id,
                                    &mut active_refresh_request_id,
                                    normalize_remote_dir(&remote_dir),
                                );
                                app.mark_refreshing();
                            }
                            BrowserAction::Download { local_path, remote_path, is_dir, overwrite } => {
                                app.mark_busy(BusyState::Downloading, "Downloading...");
                                terminal.draw(|frame| ui::render(&app, frame))?;
                                handle_download(&mut app, &params, local_path, remote_path, is_dir, overwrite).await;
                            }
                            BrowserAction::Upload { local_path, remote_path, overwrite } => {
                                app.mark_busy(BusyState::Uploading, "Uploading...");
                                terminal.draw(|frame| ui::render(&app, frame))?;
                                let uploaded = handle_upload(&mut app, &params, local_path, remote_path, overwrite).await;
                                if uploaded {
                                    spawn_refresh(
                                        &refresh_tx,
                                        &params,
                                        &mut refresh_request_id,
                                        &mut active_refresh_request_id,
                                        app.remote_dir.clone(),
                                    );
                                    app.mark_refreshing();
                                }
                            }
                            BrowserAction::Edit { remote_path } => {
                                app.mark_busy(BusyState::Editing, "Opening editor...");
                                terminal.draw(|frame| ui::render(&app, frame))?;
                                restore_terminal();
                                let edit_result = edit_remote_file(&params, &remote_path).await;
                                terminal = setup_terminal()?;
                                match edit_result {
                                    Ok(()) => {
                                        app.set_status(format!("Edited and uploaded {remote_path}"));
                                        spawn_refresh(
                                            &refresh_tx,
                                            &params,
                                            &mut refresh_request_id,
                                            &mut active_refresh_request_id,
                                            parent_remote_dir(&remote_path),
                                        );
                                        app.mark_refreshing();
                                    }
                                    Err(err) => app.set_error(err.to_string()),
                                }
                            }
                        }
                        dirty = true;
                    }
                    Event::Resize(_, _) => {
                        terminal.clear()?;
                        dirty = true;
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

fn spawn_refresh(
    tx: &mpsc::UnboundedSender<RefreshResult>,
    params: &VolumeBrowserParams,
    refresh_request_id: &mut u64,
    active_refresh_request_id: &mut u64,
    remote_dir: String,
) {
    *refresh_request_id += 1;
    *active_refresh_request_id = *refresh_request_id;
    let request_id = *refresh_request_id;
    let tx = tx.clone();
    let params = VolumeBrowserParams {
        service_instance_id: params.service_instance_id.clone(),
        volume_name: params.volume_name.clone(),
        mount_path: params.mount_path.clone(),
        remote_path: params.remote_path.clone(),
    };

    tokio::spawn(async move {
        let result = fetch_entries(&params, &remote_dir).await;
        let _ = tx.send(RefreshResult {
            request_id,
            remote_dir,
            result,
        });
    });
}

async fn fetch_entries(
    params: &VolumeBrowserParams,
    remote_dir: &str,
) -> Result<Vec<VolumeFileEntry>> {
    fetch_entries_inner(
        params.service_instance_id.clone(),
        params.mount_path.clone(),
        remote_dir,
    )
    .await
}

async fn fetch_entries_inner(
    service_instance_id: String,
    mount_path: String,
    remote_dir: &str,
) -> Result<Vec<VolumeFileEntry>> {
    let mut sftp = VolumeSftp::new(service_instance_id, mount_path);
    sftp.list_files(remote_dir)
        .await
        .map(|tree| tree.entries().to_vec())
}

async fn handle_download(
    app: &mut VolumeBrowserApp,
    params: &VolumeBrowserParams,
    local_path: PathBuf,
    remote_path: String,
    is_dir: bool,
    overwrite: bool,
) {
    let mut sftp = VolumeSftp::new(
        params.service_instance_id.clone(),
        params.mount_path.clone(),
    );

    let download_result = if is_dir {
        sftp.download_dir(&remote_path, &local_path, overwrite)
            .await
    } else {
        sftp.download_file(&remote_path, &local_path, overwrite)
            .await
    };

    match download_result {
        Ok(downloaded_path) => app.set_status(format!(
            "Downloaded {remote_path} to {}",
            downloaded_path.display()
        )),
        Err(err) => {
            if err
                .downcast_ref::<VolumeSftpError>()
                .is_some_and(|err| matches!(err, VolumeSftpError::LocalPathExists(_)))
            {
                app.request_overwrite(
                    ConfirmAction::Download,
                    local_path,
                    remote_path,
                    is_dir,
                    "A local path already exists at the download destination.".to_string(),
                );
            } else {
                app.set_error(err.to_string());
            }
        }
    }
}

async fn handle_upload(
    app: &mut VolumeBrowserApp,
    params: &VolumeBrowserParams,
    local_path: PathBuf,
    remote_path: String,
    overwrite: bool,
) -> bool {
    let mut sftp = VolumeSftp::new(
        params.service_instance_id.clone(),
        params.mount_path.clone(),
    );

    match sftp.upload(&local_path, &remote_path, overwrite).await {
        Ok(uploaded_path) => {
            app.set_status(format!(
                "Uploaded {} to {uploaded_path}",
                local_path.display()
            ));
            true
        }
        Err(err) => {
            if err
                .downcast_ref::<VolumeSftpError>()
                .is_some_and(|err| matches!(err, VolumeSftpError::RemotePathExists(_)))
            {
                app.request_overwrite(
                    ConfirmAction::Upload,
                    local_path,
                    remote_path,
                    false,
                    "A remote file already exists at the upload destination.".to_string(),
                );
            } else {
                app.set_error(err.to_string());
            }
            false
        }
    }
}

async fn edit_remote_file(params: &VolumeBrowserParams, remote_path: &str) -> Result<()> {
    let temp_path = temp_edit_path(remote_path)?;
    let mut sftp = VolumeSftp::new(
        params.service_instance_id.clone(),
        params.mount_path.clone(),
    );

    sftp.download(remote_path, &temp_path, true).await?;
    run_editor(&temp_path).await?;
    sftp.upload(&temp_path, remote_path, true).await?;
    let _ = tokio::fs::remove_file(&temp_path).await;
    Ok(())
}

fn temp_edit_path(remote_path: &str) -> Result<PathBuf> {
    let filename = remote_path
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("volume-file");
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is before UNIX epoch")?
        .as_millis();
    Ok(std::env::temp_dir().join(format!(
        "railway-volume-edit-{}-{millis}-{filename}",
        std::process::id()
    )))
}

async fn run_editor(path: &Path) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| default_editor().to_string());
    let command = format!("{} {}", editor, shell_quote(&path.display().to_string()));

    let (shell, args): (String, Vec<String>) = if cfg!(target_os = "windows") {
        (
            std::env::var("COMSPEC").unwrap_or_else(|_| "cmd".to_string()),
            vec!["/C".to_string(), command],
        )
    } else {
        (
            std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string()),
            vec!["-lc".to_string(), command],
        )
    };

    let status = tokio::process::Command::new(shell)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .with_context(|| format!("Failed to open editor command: {editor}"))?;

    if !status.success() {
        return Err(anyhow!("Editor exited with status {status}"));
    }

    Ok(())
}

fn default_editor() -> &'static str {
    if cfg!(target_os = "windows") {
        "notepad"
    } else {
        "vi"
    }
}

fn shell_quote(value: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal() {
    let _ = execute!(stdout(), LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}
