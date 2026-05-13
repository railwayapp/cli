use std::path::PathBuf;

use crate::controllers::volume_files::VolumeFileEntry;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone)]
pub enum BrowserAction {
    Continue,
    Quit,
    Refresh,
    OpenSelected,
    Parent,
    DownloadSelected,
    EditSelected,
    StartUpload,
    OpenLocalSelected,
    LocalParent,
    SubmitSelectedUpload,
    RefreshLocal,
    ConfirmOverwrite,
    CancelPrompt,
}

#[derive(Debug, Clone)]
pub enum PendingTransfer {
    Download {
        remote: VolumeFileEntry,
        local: PathBuf,
    },
    Upload {
        local: PathBuf,
        remote: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserMode {
    Browse,
    UploadPicker,
    ConfirmOverwrite,
    Help,
}

#[derive(Debug, Clone)]
pub struct LocalFileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

#[derive(Debug)]
pub struct VolumeBrowserApp {
    pub service_name: String,
    pub volume_name: String,
    pub mount_path: PathBuf,
    pub current_path: PathBuf,
    pub local_dir: PathBuf,
    pub entries: Vec<VolumeFileEntry>,
    pub selected: usize,
    pub local_current_path: PathBuf,
    pub local_entries: Vec<LocalFileEntry>,
    pub local_selected: usize,
    pub status: Option<String>,
    pub error: Option<String>,
    pub mode: BrowserMode,
    pub pending_transfer: Option<PendingTransfer>,
}

impl VolumeBrowserApp {
    pub fn new(
        service_name: String,
        volume_name: String,
        mount_path: PathBuf,
        local_dir: PathBuf,
    ) -> Self {
        Self {
            service_name,
            volume_name,
            current_path: mount_path.clone(),
            mount_path,
            local_current_path: local_dir.clone(),
            local_dir,
            entries: Vec::new(),
            selected: 0,
            local_entries: Vec::new(),
            local_selected: 0,
            status: None,
            error: None,
            mode: BrowserMode::Browse,
            pending_transfer: None,
        }
    }

    pub fn selected_entry(&self) -> Option<&VolumeFileEntry> {
        self.entries.get(self.selected)
    }

    pub fn selected_local_entry(&self) -> Option<&LocalFileEntry> {
        self.local_entries.get(self.local_selected)
    }

    pub fn set_entries(&mut self, mut entries: Vec<VolumeFileEntry>) {
        entries.sort_by(|a, b| {
            b.kind
                .is_dir()
                .cmp(&a.kind.is_dir())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.name.cmp(&b.name))
        });

        self.entries = entries;
        if self.entries.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.entries.len() - 1);
        }
    }

    pub fn set_local_entries(&mut self, mut entries: Vec<LocalFileEntry>) {
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.name.cmp(&b.name))
        });

        self.local_entries = entries;
        if self.local_entries.is_empty() {
            self.local_selected = 0;
        } else {
            self.local_selected = self.local_selected.min(self.local_entries.len() - 1);
        }
    }

    pub fn set_status<S: Into<String>>(&mut self, status: S) {
        self.status = Some(status.into());
        self.error = None;
    }

    pub fn set_error<S: Into<String>>(&mut self, error: S) {
        self.error = Some(error.into());
        self.status = None;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> BrowserAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return BrowserAction::Quit;
        }

        match self.mode {
            BrowserMode::Browse => self.handle_browse_key(key),
            BrowserMode::UploadPicker => self.handle_upload_key(key),
            BrowserMode::ConfirmOverwrite => self.handle_confirm_key(key),
            BrowserMode::Help => self.handle_help_key(key),
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> BrowserAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => BrowserAction::Quit,
            KeyCode::Char('?') => {
                self.mode = BrowserMode::Help;
                BrowserAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                BrowserAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.entries.is_empty() {
                    self.selected = (self.selected + 1).min(self.entries.len() - 1);
                }
                BrowserAction::Continue
            }
            KeyCode::Enter | KeyCode::Right => BrowserAction::OpenSelected,
            KeyCode::Left | KeyCode::Backspace => BrowserAction::Parent,
            KeyCode::Char('r') => BrowserAction::Refresh,
            KeyCode::Char('d') => BrowserAction::DownloadSelected,
            KeyCode::Char('e') => BrowserAction::EditSelected,
            KeyCode::Char('u') => {
                self.mode = BrowserMode::UploadPicker;
                BrowserAction::StartUpload
            }
            _ => BrowserAction::Continue,
        }
    }

    fn handle_upload_key(&mut self, key: KeyEvent) -> BrowserAction {
        match key.code {
            KeyCode::Esc => {
                self.mode = BrowserMode::Browse;
                BrowserAction::CancelPrompt
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.local_selected = self.local_selected.saturating_sub(1);
                BrowserAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.local_entries.is_empty() {
                    self.local_selected =
                        (self.local_selected + 1).min(self.local_entries.len() - 1);
                }
                BrowserAction::Continue
            }
            KeyCode::Right => BrowserAction::OpenLocalSelected,
            KeyCode::Enter => {
                self.mode = BrowserMode::Browse;
                BrowserAction::SubmitSelectedUpload
            }
            KeyCode::Left | KeyCode::Backspace => BrowserAction::LocalParent,
            KeyCode::Char('r') => BrowserAction::RefreshLocal,
            _ => BrowserAction::Continue,
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> BrowserAction {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => {
                self.mode = BrowserMode::Browse;
                BrowserAction::ConfirmOverwrite
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => {
                self.mode = BrowserMode::Browse;
                BrowserAction::CancelPrompt
            }
            _ => BrowserAction::Continue,
        }
    }

    fn handle_help_key(&mut self, key: KeyEvent) -> BrowserAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                self.mode = BrowserMode::Browse;
                BrowserAction::Continue
            }
            _ => BrowserAction::Continue,
        }
    }
}
