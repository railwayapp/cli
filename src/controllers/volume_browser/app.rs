use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone)]
pub struct RemoteEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

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
    SubmitUpload(PathBuf),
    ConfirmOverwrite,
    CancelPrompt,
}

#[derive(Debug, Clone)]
pub enum PendingTransfer {
    Download { remote: RemoteEntry, local: PathBuf },
    Upload { local: PathBuf, remote: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserMode {
    Browse,
    UploadInput,
    ConfirmOverwrite,
    Help,
}

#[derive(Debug)]
pub struct VolumeBrowserApp {
    pub service_name: String,
    pub volume_name: String,
    pub mount_path: PathBuf,
    pub current_path: PathBuf,
    pub local_dir: PathBuf,
    pub entries: Vec<RemoteEntry>,
    pub selected: usize,
    pub status: Option<String>,
    pub error: Option<String>,
    pub mode: BrowserMode,
    pub upload_input: String,
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
            local_dir,
            entries: Vec::new(),
            selected: 0,
            status: None,
            error: None,
            mode: BrowserMode::Browse,
            upload_input: String::new(),
            pending_transfer: None,
        }
    }

    pub fn selected_entry(&self) -> Option<&RemoteEntry> {
        self.entries.get(self.selected)
    }

    pub fn set_entries(&mut self, mut entries: Vec<RemoteEntry>) {
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
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
            BrowserMode::UploadInput => self.handle_upload_key(key),
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
                self.mode = BrowserMode::UploadInput;
                self.upload_input.clear();
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
            KeyCode::Enter => {
                self.mode = BrowserMode::Browse;
                BrowserAction::SubmitUpload(PathBuf::from(self.upload_input.trim()))
            }
            KeyCode::Backspace => {
                self.upload_input.pop();
                BrowserAction::Continue
            }
            KeyCode::Char(ch) => {
                self.upload_input.push(ch);
                BrowserAction::Continue
            }
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
