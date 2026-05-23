use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;

use super::{HandleKeyAction, ui};
use crate::controllers::dash_tui::data::{DashboardProject, DashboardService, ProjectLoadTarget};

#[derive(Clone, Debug)]
pub(in crate::controllers::dash_tui) struct ProjectScreenState {
    pub(in crate::controllers::dash_tui) target: ProjectLoadTarget,
    pub(in crate::controllers::dash_tui) project: Option<DashboardProject>,
    pub(in crate::controllers::dash_tui) selected_service: usize,
    pub(in crate::controllers::dash_tui) loading: bool,
    pub(in crate::controllers::dash_tui) error: Option<String>,
    pub(in crate::controllers::dash_tui) current_request_id: u64,
    pub(in crate::controllers::dash_tui) environment_selector: Option<EnvironmentSelectorState>,
}

#[derive(Clone, Debug)]
pub(in crate::controllers::dash_tui) struct EnvironmentSelectorState {
    pub(in crate::controllers::dash_tui) selected: usize,
}

impl ProjectScreenState {
    pub(in crate::controllers::dash_tui) fn new(target: ProjectLoadTarget) -> Self {
        Self {
            target,
            project: None,
            selected_service: 0,
            loading: false,
            error: None,
            current_request_id: 0,
            environment_selector: None,
        }
    }

    pub(in crate::controllers::dash_tui) fn selected_service(&self) -> Option<&DashboardService> {
        self.project
            .as_ref()
            .and_then(|project| project.services.get(self.selected_service))
    }

    pub(in crate::controllers::dash_tui) fn apply_loaded_project(
        &mut self,
        project: DashboardProject,
    ) {
        let preferred_service_id = self.selected_service().map(|service| service.id.clone());
        self.loading = false;
        self.error = None;
        self.project = Some(project);
        self.environment_selector = None;
        self.clamp_selection();

        if let Some(preferred_service_id) = preferred_service_id {
            self.select_service_by_id(&preferred_service_id);
        }
    }

    pub(in crate::controllers::dash_tui) fn set_error(&mut self, error: String) {
        self.loading = false;
        self.error = Some(error);
        self.environment_selector = None;
        self.clamp_selection();
    }

    pub(in crate::controllers::dash_tui) fn move_left(&mut self) {
        self.selected_service = self.selected_service.saturating_sub(1);
    }

    pub(in crate::controllers::dash_tui) fn move_right(&mut self) {
        let len = self
            .project
            .as_ref()
            .map(|project| project.services.len())
            .unwrap_or(0);
        if len > 0 {
            self.selected_service = (self.selected_service + 1).min(len - 1);
        }
    }

    pub(in crate::controllers::dash_tui) fn move_up(&mut self, columns: usize) {
        self.selected_service = self.selected_service.saturating_sub(columns.max(1));
    }

    pub(in crate::controllers::dash_tui) fn move_down(&mut self, columns: usize) {
        let len = self
            .project
            .as_ref()
            .map(|project| project.services.len())
            .unwrap_or(0);
        if len > 0 {
            self.selected_service = (self.selected_service + columns.max(1)).min(len - 1);
        }
    }

    pub(in crate::controllers::dash_tui) fn select_service_by_id(&mut self, service_id: &str) {
        if let Some(project) = &self.project {
            if let Some(index) = project
                .services
                .iter()
                .position(|service| service.id == service_id)
            {
                self.selected_service = index;
            } else {
                self.clamp_selection();
            }
        }
    }

    pub(in crate::controllers::dash_tui) fn clamp_selection(&mut self) {
        let len = self
            .project
            .as_ref()
            .map(|project| project.services.len())
            .unwrap_or(0);
        if len == 0 {
            self.selected_service = 0;
        } else {
            self.selected_service = self.selected_service.min(len - 1);
        }
    }
}

pub(in crate::controllers::dash_tui) fn handle_project_screen_key(
    state: &mut ProjectScreenState,
    key: KeyEvent,
    terminal_area: Rect,
) -> HandleKeyAction {
    let [_, body, _] = ui::dashboard_sections(terminal_area);
    let [_, main_area] = ui::screen_sections(body);
    let [diagram_area, _] = ui::project_overview_sections(main_area);
    let (columns, _, _) = ui::service_grid_metrics(ui::panel_block("services").inner(diagram_area));

    match key.code {
        KeyCode::Esc | KeyCode::Backspace => return HandleKeyAction::Back,
        KeyCode::Up | KeyCode::Char('i') => state.move_up(columns),
        KeyCode::Down | KeyCode::Char('k') => state.move_down(columns),
        KeyCode::Left | KeyCode::Char('j') => state.move_left(),
        KeyCode::Right | KeyCode::Char('l') => state.move_right(),
        KeyCode::Enter => return HandleKeyAction::OpenSelectedService,
        KeyCode::Char('L') => return HandleKeyAction::OpenProjectLogs,
        KeyCode::Char('e') => return HandleKeyAction::OpenEnvironmentSelector,
        KeyCode::Char('r') => return HandleKeyAction::RefreshProject,
        _ => {}
    }

    HandleKeyAction::None
}
