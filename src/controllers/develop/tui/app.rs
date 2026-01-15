use std::collections::HashMap;

use colored::Color;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::log_store::{LogRef, LogStore, StoredLogLine};
use crate::controllers::develop::LogLine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Local,
    Image,
    Service(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartRequest {
    Local,
    Image,
    Service(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiAction {
    None,
    Quit,
    Restart(RestartRequest),
}

#[derive(Debug, Clone)]
pub struct ServiceInfo {
    pub name: String,
    pub is_docker: bool,
    pub color: Color,
    pub var_count: usize,
    pub private_url: Option<String>,
    pub public_url: Option<String>,
    pub command: Option<String>,
    pub image: Option<String>,
    pub process_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub start: (usize, usize), // (row, col) in visible log area
    pub end: (usize, usize),
}

impl Selection {
    pub fn normalized(&self) -> ((usize, usize), (usize, usize)) {
        if self.start <= self.end {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        let ((sr, sc), (er, ec)) = self.normalized();
        if row < sr || row > er {
            return false;
        }
        if row == sr && row == er {
            return col >= sc && col <= ec;
        }
        if row == sr {
            return col >= sc;
        }
        if row == er {
            return col <= ec;
        }
        true
    }
}

pub struct TuiApp {
    pub current_tab: Tab,
    pub scroll_offset: usize,
    pub follow_mode: bool,
    pub show_info: bool,
    pub log_store: LogStore,
    pub services: Vec<ServiceInfo>,
    pub selection: Option<Selection>,
    pub selecting: bool,
    pub copied_feedback: Option<std::time::Instant>,
    pub copy_failed: Option<std::time::Instant>,
    pub needs_clear: bool,
    service_name_to_idx: HashMap<String, usize>,
    visible_height: usize,
    code_count: usize,
    image_count: usize,
    log_area_top: u16,
    log_area_height: u16,
}

impl TuiApp {
    pub fn new(services: Vec<ServiceInfo>) -> Self {
        let service_name_to_idx = services
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i))
            .collect();

        let code_count = services.iter().filter(|s| !s.is_docker).count();
        let image_count = services.iter().filter(|s| s.is_docker).count();

        let initial_tab = if code_count > 1 {
            Tab::Local
        } else if image_count > 1 {
            Tab::Image
        } else if !services.is_empty() {
            Tab::Service(0)
        } else {
            Tab::Local
        };

        Self {
            current_tab: initial_tab,
            scroll_offset: 0,
            follow_mode: true,
            show_info: true,
            log_store: LogStore::new(services.len()),
            services,
            selection: None,
            selecting: false,
            copied_feedback: None,
            copy_failed: None,
            needs_clear: false,
            service_name_to_idx,
            visible_height: 20,
            code_count,
            image_count,
            log_area_top: 1,
            log_area_height: 20,
        }
    }

    pub fn show_local_tab(&self) -> bool {
        self.code_count > 1
    }

    pub fn show_image_tab(&self) -> bool {
        self.image_count > 1
    }

    pub fn set_visible_height(&mut self, height: usize) {
        self.visible_height = height;
    }

    pub fn set_log_area(&mut self, top: u16, height: u16) {
        self.log_area_top = top;
        self.log_area_height = height;
    }

    pub fn push_log(&mut self, log: LogLine, is_docker: bool) {
        let service_idx = self
            .service_name_to_idx
            .get(&log.service_name)
            .copied()
            .unwrap_or(0);

        let stored = StoredLogLine {
            service_name: log.service_name,
            message: log.message,
            color: log.color,
        };

        self.log_store.push(service_idx, stored, is_docker);

        if self.follow_mode {
            self.scroll_to_bottom();
        }
    }

    /// Returns (action, tab_changed)
    pub fn handle_key(&mut self, key: KeyEvent) -> (TuiAction, bool) {
        let prev_tab = self.current_tab;

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return (TuiAction::Quit, false),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return (TuiAction::Quit, false);
            }

            // Copy selection
            KeyCode::Char('y') => {
                self.copy_selection();
            }

            // Restart
            KeyCode::Char('r') => {
                let request = match self.current_tab {
                    Tab::Local => RestartRequest::Local,
                    Tab::Image => RestartRequest::Image,
                    Tab::Service(idx) => RestartRequest::Service(idx),
                };
                return (TuiAction::Restart(request), false);
            }

            // Toggle info pane
            KeyCode::Char('i') => {
                self.show_info = !self.show_info;
            }

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
                if !self.follow_mode {
                    self.scroll_down(1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.exit_follow_mode();
                self.scroll_up(1);
            }
            KeyCode::PageDown => {
                if !self.follow_mode {
                    self.scroll_down(20);
                }
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
                self.needs_clear = true;
                if self.follow_mode {
                    self.scroll_to_bottom();
                }
            }

            _ => {}
        }

        let tab_changed = self.current_tab != prev_tab;
        (TuiAction::None, tab_changed)
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        let row = event.row;
        let col = event.column;

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Check if click is in log area
                if row >= self.log_area_top && row < self.log_area_top + self.log_area_height {
                    let log_row = (row - self.log_area_top) as usize;
                    self.selection = Some(Selection {
                        start: (log_row, col as usize),
                        end: (log_row, col as usize),
                    });
                    self.selecting = true;
                } else {
                    self.selection = None;
                    self.selecting = false;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.selecting => {
                if row >= self.log_area_top && row < self.log_area_top + self.log_area_height {
                    let log_row = (row - self.log_area_top) as usize;
                    if let Some(sel) = &mut self.selection {
                        sel.end = (log_row, col as usize);
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.selecting = false;
                // Auto-copy if selection is non-trivial
                if let Some(sel) = &self.selection {
                    if sel.start != sel.end {
                        self.copy_selection();
                    } else {
                        self.selection = None;
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                // Don't exit follow mode when scrolling down - it's a no-op at bottom
                if !self.follow_mode {
                    self.scroll_down(3);
                }
            }
            MouseEventKind::ScrollUp => {
                self.exit_follow_mode();
                self.scroll_up(3);
            }
            _ => {}
        }
    }

    fn copy_selection(&mut self) {
        let Some(sel) = &self.selection else { return };

        let text = self.get_selected_text(sel);
        if text.is_empty() {
            return;
        }

        let now = std::time::Instant::now();
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&text)) {
            Ok(()) => self.copied_feedback = Some(now),
            Err(_) => self.copy_failed = Some(now),
        }
        self.selection = None;
    }

    fn get_selected_text(&self, sel: &Selection) -> String {
        let ((sr, sc), (er, ec)) = sel.normalized();
        let visible_height = self.log_area_height as usize;

        let logs = self.current_logs();
        let total = logs.len();
        let start_idx = if self.follow_mode {
            total.saturating_sub(visible_height)
        } else {
            self.scroll_offset
        };

        let mut result = String::new();
        for (vis_row, log_idx) in (start_idx..total).enumerate() {
            if vis_row > er {
                break;
            }
            if vis_row < sr {
                continue;
            }

            let (service_name, message) = match &logs[log_idx] {
                LogRef::Entry(e) => (e.line.service_name.as_str(), e.line.message.as_str()),
                LogRef::Service(_idx, line) => (line.service_name.as_str(), line.message.as_str()),
            };

            let full_line = format!("[{}] {}", service_name, message);

            let line_chars: Vec<char> = full_line.chars().collect();
            let line_len = line_chars.len();

            let col_start = if vis_row == sr { sc } else { 0 };
            let col_end = if vis_row == er {
                ec.min(line_len.saturating_sub(1))
            } else {
                line_len.saturating_sub(1)
            };

            if col_start <= col_end && col_start < line_len {
                let selected: String = line_chars[col_start..=col_end.min(line_len - 1)]
                    .iter()
                    .collect();
                result.push_str(&selected);
            }

            if vis_row < er {
                result.push('\n');
            }
        }

        result
    }

    fn current_logs(&self) -> Vec<LogRef<'_>> {
        match self.current_tab {
            Tab::Local => self
                .log_store
                .local_logs
                .iter()
                .map(LogRef::Entry)
                .collect(),
            Tab::Image => self
                .log_store
                .image_logs
                .iter()
                .map(LogRef::Entry)
                .collect(),
            Tab::Service(idx) => self
                .log_store
                .services
                .get(idx)
                .map(|buf| {
                    buf.lines
                        .iter()
                        .map(|line| LogRef::Service(idx, line))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn exit_follow_mode(&mut self) {
        if self.follow_mode {
            let total = self.current_log_count();
            self.scroll_offset = total.saturating_sub(self.visible_height);
            self.follow_mode = false;
            self.needs_clear = true;
        }
    }

    fn select_tab(&mut self, visual_idx: usize) {
        let tab = self.visual_to_tab(visual_idx);
        if let Some(t) = tab {
            if self.current_tab != t {
                self.needs_clear = true;
            }
            self.current_tab = t;
            self.scroll_offset = 0;
            self.selection = None;
            if self.follow_mode {
                self.scroll_to_bottom();
            }
        }
    }

    fn visual_to_tab(&self, visual_idx: usize) -> Option<Tab> {
        let mut idx = visual_idx;

        if self.show_local_tab() {
            if idx == 0 {
                return Some(Tab::Local);
            }
            idx -= 1;
        }

        if self.show_image_tab() {
            if idx == 0 {
                return Some(Tab::Image);
            }
            idx -= 1;
        }

        if idx < self.services.len() {
            Some(Tab::Service(idx))
        } else {
            None
        }
    }

    fn total_visible_tabs(&self) -> usize {
        let mut count = self.services.len();
        if self.show_local_tab() {
            count += 1;
        }
        if self.show_image_tab() {
            count += 1;
        }
        count
    }

    fn next_tab(&mut self) {
        let total_tabs = self.total_visible_tabs();
        if total_tabs == 0 {
            return;
        }
        let current_idx = self.tab_index();
        let next_idx = (current_idx + 1) % total_tabs;
        self.select_tab(next_idx);
    }

    fn prev_tab(&mut self) {
        let total_tabs = self.total_visible_tabs();
        if total_tabs == 0 {
            return;
        }
        let current_idx = self.tab_index();
        let prev_idx = if current_idx == 0 {
            total_tabs - 1
        } else {
            current_idx - 1
        };
        self.select_tab(prev_idx);
    }

    pub fn tab_index(&self) -> usize {
        let mut idx = 0;

        match self.current_tab {
            Tab::Local => idx,
            Tab::Image => {
                if self.show_local_tab() {
                    idx += 1;
                }
                idx
            }
            Tab::Service(i) => {
                if self.show_local_tab() {
                    idx += 1;
                }
                if self.show_image_tab() {
                    idx += 1;
                }
                idx + i
            }
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
        let total = self.current_log_count();
        let max_scroll = total.saturating_sub(self.visible_height);
        self.scroll_offset = (self.scroll_offset + amount).min(max_scroll);

        // Auto-enable follow mode when scrolled to bottom
        if self.scroll_offset >= max_scroll && !self.follow_mode {
            self.follow_mode = true;
            self.needs_clear = true;
        }
    }

    fn scroll_up(&mut self, amount: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    fn scroll_to_bottom(&mut self) {
        let total = self.current_log_count();
        self.scroll_offset = total.saturating_sub(self.visible_height);
    }
}
