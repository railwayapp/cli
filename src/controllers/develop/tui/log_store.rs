use std::collections::VecDeque;

use colored::Color;

const MAX_LINES: usize = 100_000;

#[derive(Debug, Clone)]
pub struct StoredLogLine {
    pub message: String,
    pub color: Color,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub service_idx: usize,
    pub line: StoredLogLine,
}

pub struct ServiceLogBuffer {
    pub lines: VecDeque<StoredLogLine>,
}

impl ServiceLogBuffer {
    pub fn new() -> Self {
        Self {
            lines: VecDeque::new(),
        }
    }

    pub fn push(&mut self, line: StoredLogLine) {
        if self.lines.len() >= MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }
}

pub struct LogStore {
    pub services: Vec<ServiceLogBuffer>,
    pub local_logs: VecDeque<LogEntry>,
    pub image_logs: VecDeque<LogEntry>,
}

impl LogStore {
    pub fn new(service_count: usize) -> Self {
        Self {
            services: (0..service_count)
                .map(|_| ServiceLogBuffer::new())
                .collect(),
            local_logs: VecDeque::new(),
            image_logs: VecDeque::new(),
        }
    }

    pub fn push(&mut self, service_idx: usize, line: StoredLogLine, is_docker: bool) {
        if service_idx < self.services.len() {
            self.services[service_idx].push(line.clone());
        }

        let entry = LogEntry { service_idx, line };

        let target = if is_docker {
            &mut self.image_logs
        } else {
            &mut self.local_logs
        };

        if target.len() >= MAX_LINES {
            target.pop_front();
        }
        target.push_back(entry);
    }

    pub fn local_len(&self) -> usize {
        self.local_logs.len()
    }

    pub fn image_len(&self) -> usize {
        self.image_logs.len()
    }

    pub fn service_len(&self, idx: usize) -> usize {
        self.services.get(idx).map(|s| s.len()).unwrap_or(0)
    }
}
