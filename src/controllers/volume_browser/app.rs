use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::commands::volume::sftp::VolumeFileEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserMode {
    Browse,
    Upload,
    Confirm,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyState {
    Idle,
    Refreshing,
    Downloading,
    Uploading,
    Editing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    Download,
    Upload,
}

#[derive(Debug, Clone)]
pub struct ConfirmRequest {
    pub action: ConfirmAction,
    pub title: String,
    pub message: String,
    pub local_path: PathBuf,
    pub remote_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserAction {
    Continue,
    Quit,
    Refresh,
    OpenRemoteDirectory(String),
    Download {
        local_path: PathBuf,
        remote_path: String,
        overwrite: bool,
    },
    Upload {
        local_path: PathBuf,
        remote_path: String,
        overwrite: bool,
    },
    Edit {
        remote_path: String,
    },
}

#[derive(Debug, Clone)]
pub struct LocalEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Debug)]
pub struct VolumeBrowserApp {
    pub volume_name: String,
    pub mount_path: String,
    pub remote_dir: String,
    pub remote_entries: Vec<VolumeFileEntry>,
    pub remote_selected: usize,
    pub local_cwd: PathBuf,
    pub local_entries: Vec<LocalEntry>,
    pub local_selected: usize,
    pub mode: BrowserMode,
    pub busy: BusyState,
    pub status: Option<String>,
    pub error: Option<String>,
    pub confirm: Option<ConfirmRequest>,
}

impl VolumeBrowserApp {
    pub fn new(volume_name: String, mount_path: String, remote_dir: String) -> Result<Self> {
        let local_cwd = std::env::current_dir().context("Failed to read current directory")?;
        let mut app = Self {
            volume_name,
            mount_path,
            remote_dir: normalize_remote_dir(&remote_dir),
            remote_entries: Vec::new(),
            remote_selected: 0,
            local_cwd,
            local_entries: Vec::new(),
            local_selected: 0,
            mode: BrowserMode::Browse,
            busy: BusyState::Idle,
            status: Some("Loading remote files...".to_string()),
            error: None,
            confirm: None,
        };
        app.refresh_local_entries();
        Ok(app)
    }

    pub fn apply_remote_entries(&mut self, entries: Vec<VolumeFileEntry>) {
        self.remote_entries = entries;
        self.remote_selected = self
            .remote_selected
            .min(self.remote_entries.len().saturating_sub(1));
        self.busy = BusyState::Idle;
        self.status = Some(format!("Loaded {}", self.remote_dir));
        self.error = None;
    }

    pub fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
        self.status = None;
        self.busy = BusyState::Idle;
    }

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some(message.into());
        self.error = None;
        self.busy = BusyState::Idle;
    }

    pub fn mark_refreshing(&mut self) {
        self.mark_busy(BusyState::Refreshing, "Refreshing...");
    }

    pub fn mark_busy(&mut self, busy: BusyState, message: impl Into<String>) {
        self.busy = busy;
        self.status = Some(message.into());
        self.error = None;
    }

    pub fn is_busy(&self) -> bool {
        self.busy != BusyState::Idle
    }

    pub fn selected_remote(&self) -> Option<&VolumeFileEntry> {
        self.remote_entries.get(self.remote_selected)
    }

    pub fn selected_local(&self) -> Option<&LocalEntry> {
        self.local_entries.get(self.local_selected)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> BrowserAction {
        self.error = None;

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return BrowserAction::Quit;
        }

        match self.mode {
            BrowserMode::Browse => self.handle_browse_key(key),
            BrowserMode::Upload => self.handle_upload_key(key),
            BrowserMode::Confirm => self.handle_confirm_key(key),
            BrowserMode::Help => self.handle_help_key(key),
        }
    }

    pub fn request_overwrite(
        &mut self,
        action: ConfirmAction,
        local_path: PathBuf,
        remote_path: String,
        message: String,
    ) {
        let title = match action {
            ConfirmAction::Download => "Overwrite local file?",
            ConfirmAction::Upload => "Overwrite remote file?",
        };
        self.confirm = Some(ConfirmRequest {
            action,
            title: title.to_string(),
            message,
            local_path,
            remote_path,
        });
        self.mode = BrowserMode::Confirm;
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> BrowserAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => BrowserAction::Quit,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_remote(-1);
                BrowserAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_remote(1);
                BrowserAction::Continue
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.remote_selected = 0;
                BrowserAction::Continue
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.remote_selected = self.remote_entries.len().saturating_sub(1);
                BrowserAction::Continue
            }
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
                let parent = parent_remote_dir(&self.remote_dir);
                if parent != self.remote_dir {
                    BrowserAction::OpenRemoteDirectory(parent)
                } else {
                    BrowserAction::Continue
                }
            }
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => {
                if let Some(entry) = self.selected_remote() {
                    if entry.kind == "directory" {
                        BrowserAction::OpenRemoteDirectory(entry.path.clone())
                    } else {
                        BrowserAction::Continue
                    }
                } else {
                    BrowserAction::Continue
                }
            }
            KeyCode::Char('r') | KeyCode::Char('R') => BrowserAction::Refresh,
            KeyCode::Char('?') => {
                self.mode = BrowserMode::Help;
                BrowserAction::Continue
            }
            KeyCode::Char('u') | KeyCode::Char('U') => {
                self.mode = BrowserMode::Upload;
                self.refresh_local_entries();
                BrowserAction::Continue
            }
            KeyCode::Char('d') | KeyCode::Char('D') => self.download_selected(false),
            KeyCode::Char('e') | KeyCode::Char('E') => {
                if let Some(entry) = self.selected_remote() {
                    if entry.kind == "directory" {
                        self.set_error("Directory edit is not supported yet.");
                        BrowserAction::Continue
                    } else {
                        BrowserAction::Edit {
                            remote_path: entry.path.clone(),
                        }
                    }
                } else {
                    BrowserAction::Continue
                }
            }
            _ => BrowserAction::Continue,
        }
    }

    fn handle_upload_key(&mut self, key: KeyEvent) -> BrowserAction {
        match key.code {
            KeyCode::Esc => {
                self.mode = BrowserMode::Browse;
                BrowserAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_local(-1);
                BrowserAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_local(1);
                BrowserAction::Continue
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.local_selected = 0;
                BrowserAction::Continue
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.local_selected = self.local_entries.len().saturating_sub(1);
                BrowserAction::Continue
            }
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
                if let Some(parent) = self.local_cwd.parent() {
                    self.local_cwd = parent.to_path_buf();
                    self.refresh_local_entries();
                }
                BrowserAction::Continue
            }
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => self.activate_local_entry(),
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.refresh_local_entries();
                BrowserAction::Continue
            }
            KeyCode::Char('?') => {
                self.mode = BrowserMode::Help;
                BrowserAction::Continue
            }
            _ => BrowserAction::Continue,
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> BrowserAction {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                let Some(confirm) = self.confirm.take() else {
                    self.mode = BrowserMode::Browse;
                    return BrowserAction::Continue;
                };
                self.mode = BrowserMode::Browse;
                match confirm.action {
                    ConfirmAction::Download => BrowserAction::Download {
                        local_path: confirm.local_path,
                        remote_path: confirm.remote_path,
                        overwrite: true,
                    },
                    ConfirmAction::Upload => BrowserAction::Upload {
                        local_path: confirm.local_path,
                        remote_path: confirm.remote_path,
                        overwrite: true,
                    },
                }
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                self.confirm = None;
                self.mode = BrowserMode::Browse;
                BrowserAction::Continue
            }
            _ => BrowserAction::Continue,
        }
    }

    fn handle_help_key(&mut self, key: KeyEvent) -> BrowserAction {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q') => {
                self.mode = BrowserMode::Browse;
            }
            _ => {}
        }
        BrowserAction::Continue
    }

    fn download_selected(&mut self, overwrite: bool) -> BrowserAction {
        let Some(entry) = self.selected_remote() else {
            return BrowserAction::Continue;
        };
        if entry.kind == "directory" {
            self.set_error("Directory download is not supported yet.");
            return BrowserAction::Continue;
        }

        BrowserAction::Download {
            local_path: self.local_cwd.clone(),
            remote_path: entry.path.clone(),
            overwrite,
        }
    }

    fn activate_local_entry(&mut self) -> BrowserAction {
        let Some(entry) = self.selected_local().cloned() else {
            return BrowserAction::Continue;
        };

        if entry.is_dir {
            self.local_cwd = entry.path;
            self.refresh_local_entries();
            return BrowserAction::Continue;
        }

        let remote_path = join_remote_path(&self.remote_dir, &entry.name);
        BrowserAction::Upload {
            local_path: entry.path,
            remote_path,
            overwrite: false,
        }
    }

    fn move_remote(&mut self, delta: isize) {
        self.remote_selected = move_index(self.remote_selected, self.remote_entries.len(), delta);
    }

    fn move_local(&mut self, delta: isize) {
        self.local_selected = move_index(self.local_selected, self.local_entries.len(), delta);
    }

    fn refresh_local_entries(&mut self) {
        match read_local_entries(&self.local_cwd) {
            Ok(entries) => {
                self.local_entries = entries;
                self.local_selected = self
                    .local_selected
                    .min(self.local_entries.len().saturating_sub(1));
            }
            Err(err) => {
                self.local_entries = Vec::new();
                self.local_selected = 0;
                self.set_error(err.to_string());
            }
        }
    }
}

fn read_local_entries(cwd: &Path) -> Result<Vec<LocalEntry>> {
    let mut entries = fs::read_dir(cwd)
        .with_context(|| format!("Failed to read local directory {}", cwd.display()))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let file_type = entry.file_type().ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            Some(LocalEntry {
                name,
                path: entry.path(),
                is_dir: file_type.is_dir(),
            })
        })
        .collect::<Vec<_>>();

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(entries)
}

fn move_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = current as isize + delta;
    next.clamp(0, len.saturating_sub(1) as isize) as usize
}

pub fn normalize_remote_dir(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        "/".to_string()
    } else {
        format!("/{}", trimmed.trim_matches('/'))
    }
}

pub fn parent_remote_dir(path: &str) -> String {
    let path = normalize_remote_dir(path);
    if path == "/" {
        return path;
    }
    let parent = path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("/");
    if parent.is_empty() {
        "/".to_string()
    } else {
        parent.to_string()
    }
}

pub fn join_remote_path(parent: &str, name: &str) -> String {
    let parent = normalize_remote_dir(parent);
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent};

    #[test]
    fn remote_paths_are_normalized() {
        assert_eq!(normalize_remote_dir(""), "/");
        assert_eq!(normalize_remote_dir("/"), "/");
        assert_eq!(normalize_remote_dir("data/backups/"), "/data/backups");
        assert_eq!(parent_remote_dir("/data/backups"), "/data");
        assert_eq!(parent_remote_dir("/data"), "/");
        assert_eq!(join_remote_path("/", "dump.sql"), "/dump.sql");
        assert_eq!(join_remote_path("/data", "dump.sql"), "/data/dump.sql");
    }

    #[test]
    fn enter_opens_remote_directories_but_not_files() {
        let mut app = VolumeBrowserApp {
            volume_name: "data".to_string(),
            mount_path: "/data".to_string(),
            remote_dir: "/".to_string(),
            remote_entries: vec![VolumeFileEntry {
                name: "backups".to_string(),
                path: "/backups".to_string(),
                kind: "directory",
                size: 0,
            }],
            remote_selected: 0,
            local_cwd: PathBuf::from("."),
            local_entries: Vec::new(),
            local_selected: 0,
            mode: BrowserMode::Browse,
            busy: BusyState::Idle,
            status: None,
            error: None,
            confirm: None,
        };

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            BrowserAction::OpenRemoteDirectory("/backups".to_string())
        );

        app.remote_entries[0].kind = "file";
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            BrowserAction::Continue
        );
    }

    #[test]
    fn overwrite_confirmation_replays_original_action_with_overwrite() {
        let mut app = VolumeBrowserApp {
            volume_name: "data".to_string(),
            mount_path: "/data".to_string(),
            remote_dir: "/".to_string(),
            remote_entries: Vec::new(),
            remote_selected: 0,
            local_cwd: PathBuf::from("."),
            local_entries: Vec::new(),
            local_selected: 0,
            mode: BrowserMode::Browse,
            busy: BusyState::Idle,
            status: None,
            error: None,
            confirm: None,
        };

        app.request_overwrite(
            ConfirmAction::Upload,
            PathBuf::from("dump.sql"),
            "/dump.sql".to_string(),
            "exists".to_string(),
        );

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            BrowserAction::Upload {
                local_path: PathBuf::from("dump.sql"),
                remote_path: "/dump.sql".to_string(),
                overwrite: true,
            }
        );
        assert_eq!(app.mode, BrowserMode::Browse);
    }
}
