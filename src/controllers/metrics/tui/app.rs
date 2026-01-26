use crossterm::event::{KeyCode, KeyEvent};

use super::super::MetricsData;

pub struct MetricsApp {
    pub services: Vec<(String, String)>,
    pub metrics: Vec<MetricsData>,
    pub selected_service: usize,
    pub time_range: String,
}

impl MetricsApp {
    pub fn new(services: Vec<(String, String)>, time_range: String) -> Self {
        Self {
            services,
            metrics: Vec::new(),
            selected_service: 0,
            time_range,
        }
    }

    pub fn update_metrics(&mut self, metrics: Vec<MetricsData>) {
        self.metrics = metrics;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected_service > 0 {
                    self.selected_service -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected_service < self.metrics.len().saturating_sub(1) {
                    self.selected_service += 1;
                }
            }
            KeyCode::Tab => {
                self.selected_service =
                    (self.selected_service + 1) % self.metrics.len().max(1);
            }
            _ => {}
        }
        false
    }

    pub fn get_selected_metrics(&self) -> Option<&MetricsData> {
        self.metrics.get(self.selected_service)
    }

    pub fn get_service_name(&self, service_id: &Option<String>) -> String {
        if let Some(id) = service_id {
            self.services
                .iter()
                .find(|(sid, _)| sid == id)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| id.clone())
        } else {
            "Unknown".to_string()
        }
    }
}
