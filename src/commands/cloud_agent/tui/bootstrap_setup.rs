//! The bootstrap creation form owns its draft and stays visible through cleanup.
use super::app::{HARNESSES, Target};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub target: Target,
    pub name: String,
    pub repo: Option<String>,
    pub harness: String,
}

#[derive(Clone, Debug)]
pub enum DefaultState {
    Loading,
    Missing,
    Ready(String),
    Failed(String),
}

pub struct Form {
    pub target: Target,
    pub name: String,
    pub repo: String,
    pub harness: usize,
    pub field: usize,
    pub running: bool,
    pub finished: bool,
    pub steps: Vec<String>,
    pub error: Option<String>,
}

pub enum Action {
    None,
    Close,
    Submit(Request),
}

impl Form {
    pub fn new(target: Target, harness: usize) -> Self {
        Self {
            name: String::new(),
            repo: String::new(),
            target,
            harness: if harness < HARNESSES.len() - 1 {
                harness
            } else {
                0
            },
            field: 0,
            running: false,
            finished: false,
            steps: vec![],
            error: None,
        }
    }

    pub fn paste(&mut self, text: &str) {
        if self.running || self.finished {
            return;
        }
        let value = match self.field {
            0 => &mut self.name,
            1 => &mut self.repo,
            _ => return,
        };
        value.extend(text.chars().filter(|c| !c.is_control()));
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
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.field = (self.field + 1) % 4,
            KeyCode::BackTab | KeyCode::Up => self.field = (self.field + 3) % 4,
            KeyCode::Left if self.field == 2 => {
                self.harness = (self.harness + HARNESSES.len() - 2) % (HARNESSES.len() - 1)
            }
            KeyCode::Right | KeyCode::Char(' ') if self.field == 2 => {
                self.harness = (self.harness + 1) % (HARNESSES.len() - 1)
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.paste(&c.to_string())
            }
            KeyCode::Backspace => match self.field {
                0 => {
                    self.name.pop();
                }
                1 => {
                    self.repo.pop();
                }
                _ => {}
            },
            KeyCode::Enter if self.field == 3 => {
                if self.name.trim().is_empty() {
                    self.error = Some("Enter a bootstrap name.".into());
                    self.field = 0;
                } else {
                    self.running = true;
                    self.error = None;
                    self.steps.clear();
                    return Action::Submit(Request {
                        target: self.target.clone(),
                        name: self.name.trim().into(),
                        repo: (!self.repo.trim().is_empty()).then(|| self.repo.trim().into()),
                        harness: HARNESSES[self.harness].into(),
                    });
                }
            }
            KeyCode::Enter => self.field = (self.field + 1) % 4,
            _ => {}
        }
        Action::None
    }
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
    fn bootstrap_setup_validates_name_and_keeps_repo_optional() {
        let mut f = form();
        f.field = 3;
        assert!(matches!(
            f.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::None
        ));
        assert!(!f.running);
        assert!(f.error.is_some());
        f.paste("dev\n");
        f.field = 3;
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
