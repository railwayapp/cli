use colored::Color;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

use super::log_store::{LogStore, StoredLogLine};
use crate::controllers::develop::LogLine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Local,
    Image,
    Service(usize),
}

#[derive(Debug, Clone)]
pub struct ServiceInfo {
    pub name: String,
    pub is_docker: bool,
    pub color: Color,
}

pub struct TuiApp {
    pub current_tab: Tab,
    pub scroll_offset: usize,
    pub follow_mode: bool,
    pub log_store: LogStore,
    pub services: Vec<ServiceInfo>,
    service_name_to_idx: std::collections::HashMap<String, usize>,
    visible_height: usize,
}

impl TuiApp {
    pub fn new(services: Vec<ServiceInfo>) -> Self {
        let service_name_to_idx = services
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i))
            .collect();

        Self {
            current_tab: Tab::Local,
            scroll_offset: 0,
            follow_mode: true,
            log_store: LogStore::new(services.len()),
            services,
            service_name_to_idx,
            visible_height: 20,
        }
    }

    pub fn set_visible_height(&mut self, height: usize) {
        self.visible_height = height;
    }

    pub fn push_log(&mut self, log: LogLine, is_docker: bool) {
        let service_idx = self
            .service_name_to_idx
            .get(&log.service_name)
            .copied()
            .unwrap_or(0);

        let stored = StoredLogLine {
            message: log.message,
            is_stderr: log.is_stderr,
            color: log.color,
        };

        self.log_store.push(service_idx, stored, is_docker);

        if self.follow_mode {
            self.scroll_to_bottom();
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,

            // Tab selection by number
            KeyCode::Char('1') => self.select_tab(0),
            KeyCode::Char('2') => self.select_tab(1),
            KeyCode::Char('3') => self.select_tab(2),
            KeyCode::Char('4') => self.select_tab(3),
            KeyCode::Char('5') => self.select_tab(4),
            KeyCode::Char('6') => self.select_tab(5),
            KeyCode::Char('7') => self.select_tab(6),
            KeyCode::Char('8') => self.select_tab(7),
            KeyCode::Char('9') => self.select_tab(8),

            // Tab cycling
            KeyCode::Tab => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.prev_tab();
                } else {
                    self.next_tab();
                }
            }
            KeyCode::BackTab => self.prev_tab(),

            // Scrolling
            KeyCode::Char('j') | KeyCode::Down => {
                self.exit_follow_mode();
                self.scroll_down(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.exit_follow_mode();
                self.scroll_up(1);
            }
            KeyCode::PageDown => {
                self.exit_follow_mode();
                self.scroll_down(20);
            }
            KeyCode::PageUp => {
                self.exit_follow_mode();
                self.scroll_up(20);
            }
            KeyCode::Char('g') => {
                self.scroll_to_top();
                self.follow_mode = false;
            }
            KeyCode::Char('G') => {
                self.scroll_to_bottom();
                self.follow_mode = true;
            }

            // Follow mode toggle
            KeyCode::Char('f') => {
                self.follow_mode = !self.follow_mode;
                if self.follow_mode {
                    self.scroll_to_bottom();
                }
            }

            _ => {}
        }
        false
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        match event.kind {
            MouseEventKind::ScrollDown => {
                self.exit_follow_mode();
                self.scroll_down(1);
            }
            MouseEventKind::ScrollUp => {
                self.exit_follow_mode();
                self.scroll_up(1);
            }
            _ => {}
        }
    }

    fn exit_follow_mode(&mut self) {
        if self.follow_mode {
            let total = self.current_log_count();
            self.scroll_offset = total.saturating_sub(self.visible_height);
            self.follow_mode = false;
        }
    }

    fn select_tab(&mut self, idx: usize) {
        let tab = match idx {
            0 => Tab::Local,
            1 => Tab::Image,
            n => {
                let service_idx = n - 2;
                if service_idx < self.services.len() {
                    Tab::Service(service_idx)
                } else {
                    return;
                }
            }
        };
        self.current_tab = tab;
        self.scroll_offset = 0;
        if self.follow_mode {
            self.scroll_to_bottom();
        }
    }

    fn next_tab(&mut self) {
        let total_tabs = 2 + self.services.len();
        let current_idx = self.tab_index();
        let next_idx = (current_idx + 1) % total_tabs;
        self.select_tab(next_idx);
    }

    fn prev_tab(&mut self) {
        let total_tabs = 2 + self.services.len();
        let current_idx = self.tab_index();
        let prev_idx = if current_idx == 0 {
            total_tabs - 1
        } else {
            current_idx - 1
        };
        self.select_tab(prev_idx);
    }

    pub fn tab_index(&self) -> usize {
        match self.current_tab {
            Tab::Local => 0,
            Tab::Image => 1,
            Tab::Service(i) => 2 + i,
        }
    }

    pub fn current_log_count(&self) -> usize {
        match self.current_tab {
            Tab::Local => self.log_store.local_len(),
            Tab::Image => self.log_store.image_len(),
            Tab::Service(i) => self.log_store.service_len(i),
        }
    }

    fn scroll_down(&mut self, amount: usize) {
        let max_scroll = self.current_log_count().saturating_sub(1);
        self.scroll_offset = (self.scroll_offset + amount).min(max_scroll);
    }

    fn scroll_up(&mut self, amount: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.current_log_count().saturating_sub(1);
    }
}
