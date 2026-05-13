mod app;
mod ui;

use std::{
    env, fs,
    io::{self, stdout},
    panic,
    path::{Path, PathBuf},
    process::{self, Command},
};

pub use app::{BrowserAction, LocalFileEntry, PendingTransfer, VolumeBrowserApp};

use anyhow::{Context, Result, bail};
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::controllers::volume_files::{VolumeFileClient, VolumeFileKind};

pub enum VolumeBrowserOutput {
    Closed,
}

pub struct VolumeBrowserParams {
    pub service_instance_id: String,
    pub service_name: String,
    pub volume_name: String,
    pub mount_path: PathBuf,
    pub local_dir: PathBuf,
    pub identity_file: Option<PathBuf>,
}

pub fn run(params: VolumeBrowserParams) -> Result<VolumeBrowserOutput> {
    fs::create_dir_all(&params.local_dir).with_context(|| {
        format!(
            "Failed to create local transfer directory {}",
            params.local_dir.display()
        )
    })?;

    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    let remote = VolumeFileClient::connect(params.service_instance_id, params.identity_file)?;
    let mut app = VolumeBrowserApp::new(
        params.service_name,
        params.volume_name,
        params.mount_path,
        params.local_dir,
    );

    refresh_entries(&remote, &mut app);

    let mut terminal = setup_terminal()?;
    let _terminal_cleanup = scopeguard::guard((), |_| {
        restore_terminal();
    });

    loop {
        terminal.draw(|frame| ui::render(&app, frame))?;

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match app.handle_key(key) {
                BrowserAction::Continue => {}
                BrowserAction::Quit => return Ok(VolumeBrowserOutput::Closed),
                BrowserAction::Refresh => refresh_entries(&remote, &mut app),
                BrowserAction::OpenSelected => open_selected(&remote, &mut app),
                BrowserAction::Parent => open_parent(&remote, &mut app),
                BrowserAction::DownloadSelected => {
                    if queue_download(&mut app) {
                        run_pending_transfer(&remote, &mut app, &mut terminal)
                    }
                }
                BrowserAction::EditSelected => edit_selected(&remote, &mut app, &mut terminal),
                BrowserAction::StartUpload => {
                    refresh_local_entries(&mut app);
                    app.set_status("Select a local file or directory to upload");
                }
                BrowserAction::OpenLocalSelected => open_local_selected(&mut app),
                BrowserAction::LocalParent => open_local_parent(&mut app),
                BrowserAction::RefreshLocal => refresh_local_entries(&mut app),
                BrowserAction::SubmitSelectedUpload => {
                    if queue_selected_upload(&remote, &mut app) {
                        run_pending_transfer(&remote, &mut app, &mut terminal)
                    }
                }
                BrowserAction::ConfirmOverwrite => {
                    run_pending_transfer(&remote, &mut app, &mut terminal)
                }
                BrowserAction::CancelPrompt => {
                    app.pending_transfer = None;
                    app.set_status("Cancelled");
                }
            },
            Event::Resize(_, _) => terminal.clear()?,
            _ => {}
        }
    }
}

fn edit_selected(
    remote: &VolumeFileClient,
    app: &mut VolumeBrowserApp,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) {
    let Some(entry) = app.selected_entry().cloned() else {
        app.set_status("Nothing selected");
        return;
    };

    if entry.kind.is_dir() {
        app.set_status("Selected entry is a directory");
        return;
    }

    let result = run_with_restored_terminal(terminal, || {
        let temp_path = edit_temp_path(&entry.name);
        remote.download(&entry.path, &temp_path, VolumeFileKind::File)?;
        run_editor(&temp_path)?;
        remote.upload(&temp_path, &entry.path)?;
        let _ = fs::remove_file(&temp_path);
        Ok(format!("Edited and synced {}", entry.name))
    });

    match result {
        Ok(message) => {
            app.set_status(message);
            refresh_entries(remote, app);
        }
        Err(error) => app.set_error(error.to_string()),
    }
}

fn refresh_entries(remote: &VolumeFileClient, app: &mut VolumeBrowserApp) {
    match remote.list_dir(&app.current_path) {
        Ok(entries) => {
            app.set_entries(entries);
            app.set_status("Ready");
        }
        Err(error) => app.set_error(format!("Failed to list directory: {error}")),
    }
}

fn open_selected(remote: &VolumeFileClient, app: &mut VolumeBrowserApp) {
    let Some(entry) = app.selected_entry().cloned() else {
        return;
    };

    if !entry.kind.is_dir() {
        app.set_status("Selected entry is not a directory");
        return;
    }

    app.current_path = entry.path;
    app.selected = 0;
    refresh_entries(remote, app);
}

fn open_parent(remote: &VolumeFileClient, app: &mut VolumeBrowserApp) {
    if app.current_path == app.mount_path {
        app.set_status("Already at volume mount path");
        return;
    }

    let Some(parent) = app.current_path.parent() else {
        return;
    };
    let parent = parent.to_path_buf();
    if !parent.starts_with(&app.mount_path) {
        app.set_status("Cannot navigate above volume mount path");
        return;
    }

    app.current_path = parent;
    app.selected = 0;
    refresh_entries(remote, app);
}

fn queue_download(app: &mut VolumeBrowserApp) -> bool {
    let Some(entry) = app.selected_entry().cloned() else {
        app.set_status("Nothing selected");
        return false;
    };

    let local = app.local_dir.join(&entry.name);
    app.pending_transfer = Some(PendingTransfer::Download {
        remote: entry,
        local: local.clone(),
    });

    if local.exists() {
        app.mode = app::BrowserMode::ConfirmOverwrite;
        false
    } else {
        true
    }
}

fn refresh_local_entries(app: &mut VolumeBrowserApp) {
    match fs::read_dir(&app.local_current_path) {
        Ok(read_dir) => {
            let entries = read_dir
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let path = entry.path();
                    let metadata = entry.metadata().ok()?;
                    Some(LocalFileEntry {
                        name: entry.file_name().to_string_lossy().to_string(),
                        path,
                        is_dir: metadata.is_dir(),
                    })
                })
                .collect();
            app.set_local_entries(entries);
        }
        Err(error) => app.set_error(format!("Failed to list local directory: {error}")),
    }
}

fn open_local_selected(app: &mut VolumeBrowserApp) {
    let Some(entry) = app.selected_local_entry().cloned() else {
        app.set_status("Nothing selected");
        return;
    };

    if !entry.is_dir {
        app.set_status("Press Enter to upload selected file");
        return;
    }

    app.local_current_path = entry.path;
    app.local_selected = 0;
    refresh_local_entries(app);
}

fn open_local_parent(app: &mut VolumeBrowserApp) {
    let Some(parent) = app.local_current_path.parent() else {
        return;
    };
    app.local_current_path = parent.to_path_buf();
    app.local_selected = 0;
    refresh_local_entries(app);
}

fn queue_selected_upload(remote: &VolumeFileClient, app: &mut VolumeBrowserApp) -> bool {
    let Some(local_entry) = app.selected_local_entry().cloned() else {
        app.set_status("Nothing selected");
        return false;
    };

    let local = local_entry.path;
    if !local.exists() {
        app.set_error(format!("Local path does not exist: {}", local.display()));
        return false;
    }

    let Some(name) = local.file_name() else {
        app.set_error("Local path must include a file or directory name");
        return false;
    };

    let remote_path = app.current_path.join(name);
    app.pending_transfer = Some(PendingTransfer::Upload {
        local,
        remote: remote_path.clone(),
    });

    match remote.exists(&remote_path) {
        Ok(true) => {
            app.mode = app::BrowserMode::ConfirmOverwrite;
            false
        }
        Ok(false) => true,
        Err(error) => {
            app.set_error(format!("Failed to check remote destination: {error}"));
            false
        }
    }
}

fn run_pending_transfer(
    remote: &VolumeFileClient,
    app: &mut VolumeBrowserApp,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) {
    let Some(transfer) = app.pending_transfer.take() else {
        return;
    };

    let result = run_with_restored_terminal(terminal, || match transfer {
        PendingTransfer::Download {
            remote: entry,
            local,
        } => {
            if local.exists() {
                remove_local_path(&local)?;
            }
            remote.download(&entry.path, &local, entry.kind)?;
            Ok(format!("Downloaded to {}", local.display()))
        }
        PendingTransfer::Upload {
            local,
            remote: dest,
        } => {
            if remote.exists(&dest)? {
                remote.remove_path(&dest)?;
            }
            remote.upload(&local, &dest)?;
            Ok(format!("Uploaded to {}", dest.display()))
        }
    });

    match result {
        Ok(message) => {
            app.set_status(message);
            refresh_entries(remote, app);
        }
        Err(error) => app.set_error(error.to_string()),
    }
}

fn run_with_restored_terminal<F>(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    operation: F,
) -> Result<String>
where
    F: FnOnce() -> Result<String>,
{
    restore_terminal();
    let result = operation();
    let _ = execute!(stdout(), EnterAlternateScreen, Hide);
    let _ = crossterm::terminal::enable_raw_mode();
    let _ = terminal.clear();
    result
}

fn edit_temp_path(name: &str) -> PathBuf {
    let safe_name = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    env::temp_dir().join(format!("railway-volume-edit-{}-{safe_name}", process::id()))
}

fn run_editor(path: &Path) -> Result<()> {
    let editor = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let status = Command::new(editor)
        .arg(path)
        .status()
        .context("Failed to launch editor")?;

    if status.success() {
        Ok(())
    } else {
        bail!("Editor exited without saving changes")
    }
}

fn remove_local_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("Failed to inspect local path {}", path.display()))?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .with_context(|| format!("Failed to remove existing local path {}", path.display()))
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
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
