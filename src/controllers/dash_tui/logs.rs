use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent};

use super::{HandleKeyAction, project::ProjectScreenState, service::ServiceScreenState};
use crate::commands::logs::DeployLogTarget;

const LOG_BUFFER_LIMIT: usize = 2_000;
const LOG_SCROLL_STEP: usize = 8;

#[derive(Clone, Debug)]
pub(in crate::controllers::dash_tui) struct LogsScreenState {
    pub(in crate::controllers::dash_tui) project_name: String,
    pub(in crate::controllers::dash_tui) environment_name: String,
    pub(in crate::controllers::dash_tui) service_name: Option<String>,
    pub(in crate::controllers::dash_tui) targets: Vec<DeployLogTarget>,
    pub(in crate::controllers::dash_tui) lines: VecDeque<String>,
    pub(in crate::controllers::dash_tui) loading: bool,
    pub(in crate::controllers::dash_tui) error: Option<String>,
    pub(in crate::controllers::dash_tui) paused: bool,
    pub(in crate::controllers::dash_tui) scroll_offset_from_bottom: usize,
    pub(in crate::controllers::dash_tui) current_request_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::controllers::dash_tui) struct LoadedLogs {
    pub(in crate::controllers::dash_tui) lines: Vec<String>,
}

impl LogsScreenState {
    pub(in crate::controllers::dash_tui) fn from_project(
        state: &ProjectScreenState,
    ) -> Option<Self> {
        let project = state.project.as_ref()?;
        let mut targets: Vec<_> = project
            .services
            .iter()
            .filter_map(|service| {
                service
                    .latest_deployment
                    .as_ref()
                    .map(|deployment| DeployLogTarget {
                        service_name: service.name.clone(),
                        deployment_id: deployment.id.clone(),
                    })
            })
            .collect();
        targets.sort_by(|a, b| a.service_name.cmp(&b.service_name));

        Some(Self::new(
            project.name.clone(),
            project.selected_environment_name.clone(),
            None,
            targets,
        ))
    }

    pub(in crate::controllers::dash_tui) fn from_service(state: &ServiceScreenState) -> Self {
        let targets = state
            .detail
            .service
            .latest_deployment
            .as_ref()
            .map(|deployment| {
                vec![DeployLogTarget {
                    service_name: state.detail.service.name.clone(),
                    deployment_id: deployment.id.clone(),
                }]
            })
            .unwrap_or_default();

        Self::new(
            state.detail.project_name.clone(),
            state.detail.environment_name.clone(),
            Some(state.detail.service.name.clone()),
            targets,
        )
    }

    fn new(
        project_name: String,
        environment_name: String,
        service_name: Option<String>,
        targets: Vec<DeployLogTarget>,
    ) -> Self {
        Self {
            project_name,
            environment_name,
            service_name,
            targets,
            lines: VecDeque::new(),
            loading: false,
            error: None,
            paused: false,
            scroll_offset_from_bottom: 0,
            current_request_id: 0,
        }
    }

    pub(in crate::controllers::dash_tui) fn start_loading(&mut self) {
        self.current_request_id += 1;
        self.lines.clear();
        self.loading = true;
        self.error = None;
        self.paused = false;
        self.scroll_offset_from_bottom = 0;
    }

    pub(in crate::controllers::dash_tui) fn apply_loaded_logs(&mut self, loaded: LoadedLogs) {
        self.lines = loaded.lines.into_iter().collect();
        self.loading = false;
        self.error = None;
        self.trim_lines();
        self.scroll_offset_from_bottom = 0;
    }

    pub(in crate::controllers::dash_tui) fn push_line(&mut self, line: String) {
        self.lines.push_back(line);
        self.trim_lines();
        self.loading = false;
        if !self.paused {
            self.scroll_offset_from_bottom = 0;
        }
    }

    pub(in crate::controllers::dash_tui) fn set_error(&mut self, error: String) {
        self.loading = false;
        self.error = Some(error);
    }

    pub(in crate::controllers::dash_tui) fn toggle_paused(&mut self) {
        self.paused = !self.paused;
        if !self.paused {
            self.scroll_offset_from_bottom = 0;
        }
    }

    pub(in crate::controllers::dash_tui) fn scroll_up(&mut self, amount: usize) {
        if self.lines.is_empty() {
            return;
        }

        self.paused = true;
        self.scroll_offset_from_bottom =
            (self.scroll_offset_from_bottom + amount).min(self.lines.len().saturating_sub(1));
    }

    pub(in crate::controllers::dash_tui) fn scroll_down(&mut self, amount: usize) {
        self.scroll_offset_from_bottom = self.scroll_offset_from_bottom.saturating_sub(amount);
    }

    pub(in crate::controllers::dash_tui) fn jump_top(&mut self) {
        if self.lines.is_empty() {
            return;
        }

        self.paused = true;
        self.scroll_offset_from_bottom = self.lines.len().saturating_sub(1);
    }

    pub(in crate::controllers::dash_tui) fn jump_bottom(&mut self) {
        self.paused = false;
        self.scroll_offset_from_bottom = 0;
    }

    pub(in crate::controllers::dash_tui) fn service_count(&self) -> usize {
        self.targets.len()
    }

    pub(in crate::controllers::dash_tui) fn is_service_scoped(&self) -> bool {
        self.service_name.is_some()
    }

    fn trim_lines(&mut self) {
        while self.lines.len() > LOG_BUFFER_LIMIT {
            self.lines.pop_front();
        }
        self.scroll_offset_from_bottom = self
            .scroll_offset_from_bottom
            .min(self.lines.len().saturating_sub(1));
    }
}

pub(in crate::controllers::dash_tui) fn handle_logs_screen_key(
    state: &mut LogsScreenState,
    key: KeyEvent,
) -> HandleKeyAction {
    match key.code {
        KeyCode::Esc | KeyCode::Backspace => HandleKeyAction::Back,
        KeyCode::Up | KeyCode::Char('i') => {
            state.scroll_up(1);
            HandleKeyAction::None
        }
        KeyCode::Down | KeyCode::Char('k') => {
            state.scroll_down(1);
            HandleKeyAction::None
        }
        KeyCode::PageUp => {
            state.scroll_up(LOG_SCROLL_STEP);
            HandleKeyAction::None
        }
        KeyCode::PageDown => {
            state.scroll_down(LOG_SCROLL_STEP);
            HandleKeyAction::None
        }
        KeyCode::Char('g') => {
            state.jump_top();
            HandleKeyAction::None
        }
        KeyCode::Char('G') => {
            state.jump_bottom();
            HandleKeyAction::None
        }
        KeyCode::Char('p') => {
            state.toggle_paused();
            HandleKeyAction::None
        }
        _ => HandleKeyAction::None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::controllers::dash_tui::{
        data::{DashboardProject, DashboardService, DeploymentSummary, ProjectLoadTarget},
        project::ProjectScreenState,
    };

    fn project_state() -> ProjectScreenState {
        let mut state = ProjectScreenState::new(ProjectLoadTarget {
            project_id: "proj_123".to_string(),
            environment_hint: Some("production".to_string()),
        });
        state.project = Some(DashboardProject {
            id: "proj_123".to_string(),
            name: "api".to_string(),
            workspace_name: Some("workspace".to_string()),
            selected_environment_id: "env_123".to_string(),
            selected_environment_name: "production".to_string(),
            environments: Vec::new(),
            services: vec![
                DashboardService {
                    id: "svc_123".to_string(),
                    name: "web".to_string(),
                    active_in_environment: true,
                    num_replicas: Some(1),
                    latest_deployment: Some(DeploymentSummary {
                        id: "dep_web".to_string(),
                        status: "SUCCESS".to_string(),
                        created_at: Utc::now(),
                        can_redeploy: true,
                        stopped: false,
                    }),
                    domains: Vec::new(),
                    source_repo: None,
                    source_image: None,
                    cron_schedule: None,
                    next_cron_run_at: None,
                    start_command: None,
                    volume_mounts: Vec::new(),
                },
                DashboardService {
                    id: "svc_456".to_string(),
                    name: "worker".to_string(),
                    active_in_environment: true,
                    num_replicas: Some(1),
                    latest_deployment: Some(DeploymentSummary {
                        id: "dep_worker".to_string(),
                        status: "SUCCESS".to_string(),
                        created_at: Utc::now(),
                        can_redeploy: true,
                        stopped: false,
                    }),
                    domains: Vec::new(),
                    source_repo: None,
                    source_image: None,
                    cron_schedule: None,
                    next_cron_run_at: None,
                    start_command: None,
                    volume_mounts: Vec::new(),
                },
            ],
        });
        state
    }

    #[test]
    fn logs_state_is_created_for_the_entire_environment() {
        let state = LogsScreenState::from_project(&project_state()).expect("logs state");

        assert_eq!(state.project_name, "api");
        assert_eq!(state.environment_name, "production");
        assert_eq!(state.service_name, None);
        assert!(!state.is_service_scoped());
        assert_eq!(state.targets.len(), 2);
        assert_eq!(state.targets[0].service_name, "web");
        assert_eq!(state.targets[1].service_name, "worker");
    }

    #[test]
    fn logs_state_can_be_scoped_to_a_single_service() {
        let project_state = project_state();
        let service_state = ServiceScreenState::from_project(&project_state).expect("service");
        let state = LogsScreenState::from_service(&service_state);

        assert_eq!(state.project_name, "api");
        assert_eq!(state.environment_name, "production");
        assert_eq!(state.service_name.as_deref(), Some("web"));
        assert!(state.is_service_scoped());
        assert_eq!(state.targets.len(), 1);
        assert_eq!(state.targets[0].service_name, "web");
        assert_eq!(state.targets[0].deployment_id, "dep_web");
    }

    #[test]
    fn toggling_pause_and_jump_bottom_resets_follow_mode() {
        let mut state = LogsScreenState::from_project(&project_state()).expect("logs state");
        state.push_line("line one".to_string());
        state.push_line("line two".to_string());

        state.toggle_paused();
        assert!(state.paused);

        state.scroll_up(1);
        assert_eq!(state.scroll_offset_from_bottom, 1);

        state.jump_bottom();
        assert!(!state.paused);
        assert_eq!(state.scroll_offset_from_bottom, 0);
    }
}
