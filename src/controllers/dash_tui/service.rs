use crossterm::event::{KeyCode, KeyEvent};

use super::{DashboardProject, HandleKeyAction};
use crate::controllers::dash_tui::{data::DashboardService, project::ProjectScreenState};

#[derive(Clone, Debug)]
pub(in crate::controllers::dash_tui) struct ServiceScreenState {
    pub(in crate::controllers::dash_tui) detail: ServiceDetail,
    pub(in crate::controllers::dash_tui) return_to_project: Box<ProjectScreenState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::controllers::dash_tui) struct ServiceDetail {
    pub(in crate::controllers::dash_tui) project_id: String,
    pub(in crate::controllers::dash_tui) project_name: String,
    pub(in crate::controllers::dash_tui) workspace_name: Option<String>,
    pub(in crate::controllers::dash_tui) environment_id: String,
    pub(in crate::controllers::dash_tui) environment_name: String,
    pub(in crate::controllers::dash_tui) service: DashboardService,
}

impl ServiceScreenState {
    pub(in crate::controllers::dash_tui) fn from_project(
        state: &ProjectScreenState,
    ) -> Option<Self> {
        let project = state.project.as_ref()?;
        let service = state.selected_service()?.clone();

        Some(Self {
            detail: service_detail_from_project(project, service),
            return_to_project: Box::new(state.clone()),
        })
    }
}

pub(in crate::controllers::dash_tui) fn handle_service_screen_key(
    key: KeyEvent,
) -> HandleKeyAction {
    match key.code {
        KeyCode::Esc | KeyCode::Backspace => HandleKeyAction::BackToProject,
        _ => HandleKeyAction::None,
    }
}

fn service_detail_from_project(
    project: &DashboardProject,
    service: DashboardService,
) -> ServiceDetail {
    ServiceDetail {
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        workspace_name: project.workspace_name.clone(),
        environment_id: project.selected_environment_id.clone(),
        environment_name: project.selected_environment_name.clone(),
        service,
    }
}
