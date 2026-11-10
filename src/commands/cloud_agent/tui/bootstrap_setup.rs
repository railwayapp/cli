//! Bootstrap forms and project selection, kept inside the terminal pane.
use super::app::{HARNESSES, Target};
use crate::controllers::agent_bootstrap::Bootstrap;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub agent_id: String,
    pub agent_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub target: Target,
    pub name: String,
    pub repo: Option<String>,
    pub harness: String,
    pub snapshot: Option<Snapshot>,
    pub make_default: bool,
}

#[derive(Clone, Debug)]
pub enum DefaultState {
    Loading,
    Missing,
    Available,
    Ready(String),
    Failed(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LaunchChoice {
    #[default]
    Default,
    Named(String),
    None,
}

pub struct Picker {
    pub target: Target,
    pub return_to_prompt: bool,
    pub for_launch: bool,
    pub entries: Vec<Bootstrap>,
    pub has_default: bool,
    pub cursor: usize,
    pub loading: bool,
    pub saving: bool,
    pub error: Option<String>,
}

impl Picker {
    pub fn new(target: Target, return_to_prompt: bool) -> Self {
        Self {
            target,
            return_to_prompt,
            for_launch: false,
            entries: vec![],
            has_default: false,
            cursor: 0,
            loading: true,
            saving: false,
            error: None,
        }
    }

    pub fn no_default_index(&self) -> usize {
        self.entries.len() + usize::from(self.for_launch)
    }

    pub fn create_index(&self) -> usize {
        self.entries.len() + 1
    }

    pub fn entry_at(&self, index: usize) -> Option<&Bootstrap> {
        self.entries
            .get(index.checked_sub(usize::from(self.for_launch))?)
    }

    pub fn loaded(&mut self, result: Result<Vec<Bootstrap>, String>) {
        self.loading = false;
        match result {
            Ok(mut entries) => {
                self.has_default = entries.iter().any(|b| b.is_default);
                entries.retain(|b| b.status != "DEGRADED" && b.status != "FAILED");
                entries.sort_by_key(|b| b.name.to_lowercase());
                self.cursor = if self.return_to_prompt && !self.for_launch {
                    entries
                        .iter()
                        .position(|b| b.is_default)
                        .unwrap_or(entries.len() + 1)
                } else if self.for_launch {
                    0
                } else {
                    entries.len() + 1
                };
                self.entries = entries;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}

pub struct Form {
    pub target: Target,
    pub name: String,
    pub repo: String,
    pub harness: usize,
    pub field: usize,
    pub name_cursor: usize,
    pub repo_cursor: usize,
    pub snapshot: Option<Snapshot>,
    pub make_default: bool,
    pub defaults_loading: bool,
    pub return_to_prompt: bool,
    pub back_to_picker: bool,
    pub running: bool,
    pub finished: bool,
    pub steps: Vec<String>,
    pub error: Option<String>,
}

pub enum Action {
    None,
    Close,
    Submit(Box<Request>),
}

impl Form {
    pub fn new(target: Target, harness: usize) -> Self {
        Self {
            name: String::new(),
            repo: String::new(),
            target,
            harness: harness.min(HARNESSES.len() - 2),
            field: 0,
            name_cursor: 0,
            repo_cursor: 0,
            snapshot: None,
            make_default: true,
            defaults_loading: false,
            return_to_prompt: true,
            back_to_picker: false,
            running: false,
            finished: false,
            steps: vec![],
            error: None,
        }
    }

    pub fn default_field(&self) -> usize {
        if self.snapshot.is_some() { 1 } else { 3 }
    }
    pub fn submit_field(&self) -> usize {
        self.default_field() + 1
    }

    pub fn text(&mut self) -> Option<(&mut String, &mut usize)> {
        match self.field {
            0 => Some((&mut self.name, &mut self.name_cursor)),
            1 if self.snapshot.is_none() => Some((&mut self.repo, &mut self.repo_cursor)),
            _ => None,
        }
    }

    pub fn paste(&mut self, text: &str) {
        if self.running || self.finished {
            return;
        }
        if let Some((value, cursor)) = self.text() {
            let text: String = text.chars().filter(|c| !c.is_control()).collect();
            *cursor = (*cursor).min(value.len());
            value.insert_str(*cursor, &text);
            *cursor += text.len();
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        if self.running {
            return Action::None;
        }
        if key.code == KeyCode::Esc {
            return Action::Close;
        }
        if self.finished {
            return if key.code == KeyCode::Enter {
                Action::Close
            } else {
                Action::None
            };
        }
        let count = self.submit_field() + 1;
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.field = (self.field + 1) % count,
            KeyCode::BackTab | KeyCode::Up => self.field = (self.field + count - 1) % count,
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter
                if self.field == self.default_field() =>
            {
                if !self.defaults_loading {
                    self.make_default = !self.make_default;
                }
            }
            KeyCode::Left if self.snapshot.is_none() && self.field == 2 => {
                self.harness = (self.harness + HARNESSES.len() - 2) % (HARNESSES.len() - 1);
            }
            KeyCode::Right | KeyCode::Char(' ') if self.snapshot.is_none() && self.field == 2 => {
                self.harness = (self.harness + 1) % (HARNESSES.len() - 1);
            }
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Backspace
            | KeyCode::Delete => {
                if let Some((value, cursor)) = self.text() {
                    *cursor = (*cursor).min(value.len());
                    let previous = value[..*cursor]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(i, _)| i);
                    let next = value[*cursor..]
                        .chars()
                        .next()
                        .map_or(*cursor, |c| *cursor + c.len_utf8());
                    match key.code {
                        KeyCode::Left => *cursor = previous,
                        KeyCode::Right => *cursor = next,
                        KeyCode::Home => *cursor = 0,
                        KeyCode::End => *cursor = value.len(),
                        KeyCode::Backspace => {
                            value.replace_range(previous..*cursor, "");
                            *cursor = previous;
                        }
                        KeyCode::Delete => {
                            value.replace_range(*cursor..next, "");
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.paste(&c.to_string());
            }
            KeyCode::Enter if self.field == self.submit_field() => {
                if self.defaults_loading {
                    return Action::None;
                }
                if self.name.trim().is_empty() {
                    self.error = Some("Enter a bootstrap name.".into());
                    self.field = 0;
                } else {
                    self.running = true;
                    self.error = None;
                    self.steps.clear();
                    return Action::Submit(Box::new(Request {
                        target: self.target.clone(),
                        name: self.name.trim().into(),
                        repo: (!self.repo.trim().is_empty()).then(|| self.repo.trim().into()),
                        harness: HARNESSES[self.harness].into(),
                        snapshot: self.snapshot.clone(),
                        make_default: self.make_default,
                    }));
                }
            }
            KeyCode::Enter => self.field = (self.field + 1) % count,
            _ => {}
        }
        Action::None
    }
}

/// Keep the caret visible without splitting UTF-8 or wide terminal characters.
pub fn text_start(value: &str, cursor: usize, width: usize) -> usize {
    let cursor = cursor.min(value.len());
    let mut used = 0;
    let mut start = cursor;
    for (i, c) in value[..cursor].char_indices().rev() {
        used += console::measure_text_width(&c.to_string());
        if used >= width {
            break;
        }
        start = i;
    }
    start
}

#[cfg(test)]
mod tests {
    use super::*;
    fn form() -> Form {
        Form::new(
            Target {
                project_id: "p".into(),
                project_name: "Demo".into(),
                environment_id: "e".into(),
                environment_name: "production".into(),
            },
            0,
        )
    }
    #[test]
    fn bootstrap_fields_edit_unicode_at_the_caret_and_keep_the_caret_visible() {
        let mut f = form();
        f.paste("développement");
        f.on_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        f.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        f.on_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        f.paste("e");
        assert_eq!(f.name, "developpement");
        f.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        f.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(f.name, "developpemen");
        let value = "界界界é";
        let start = text_start(value, value.len(), 4);
        assert!(value.is_char_boundary(start));
        assert!(console::measure_text_width(&value[start..]) < 4);
    }

    #[test]
    fn bootstrap_setup_validates_name_and_keeps_repo_optional() {
        let mut f = form();
        f.field = f.submit_field();
        assert!(matches!(
            f.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::None
        ));
        assert!(!f.running);
        assert!(f.error.is_some());
        f.paste("dev\n");
        f.field = f.submit_field();
        let Action::Submit(req) = f.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        else {
            panic!("submit");
        };
        assert_eq!(req.name, "dev");
        assert_eq!(req.repo, None);
        assert_eq!(req.harness, "railway");
        f.paste("changed");
        assert_eq!(f.name, "dev");
        assert!(matches!(
            f.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::None
        ));
    }
}
