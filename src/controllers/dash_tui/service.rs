use anyhow::{Result, anyhow};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    HandleKeyAction,
    data::{DashboardProject, DashboardService},
    project::ProjectScreenState,
};
use crate::{
    client::GQLClient,
    commands::Configs,
    controllers::deployment::{
        ServiceDeployment, fetch_service_deployments, redeploy_deployment,
        restart_latest_service_deployment, rollback_deployment,
    },
};

const SERVICE_DEPLOYMENTS_LIMIT: i64 = 20;

#[derive(Clone, Debug)]
pub(in crate::controllers::dash_tui) struct ServiceScreenState {
    pub(in crate::controllers::dash_tui) detail: ServiceDetail,
    pub(in crate::controllers::dash_tui) return_to_project: Box<ProjectScreenState>,
    pub(in crate::controllers::dash_tui) deployments: Vec<ServiceDeployment>,
    pub(in crate::controllers::dash_tui) selected_deployment: usize,
    pub(in crate::controllers::dash_tui) focus: ServiceFocus,
    pub(in crate::controllers::dash_tui) loading: bool,
    pub(in crate::controllers::dash_tui) error: Option<String>,
    pub(in crate::controllers::dash_tui) current_request_id: u64,
    pub(in crate::controllers::dash_tui) confirmation: Option<ServiceConfirmationState>,
    pub(in crate::controllers::dash_tui) toast: Option<ServiceToast>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::controllers::dash_tui) enum ServiceFocus {
    Overview,
    Deployments,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::controllers::dash_tui) enum ServiceConfirmationState {
    Redeploy { deployment_id: String },
    Restart { deployment_id: String },
    Rollback { deployment_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::controllers::dash_tui) struct ServiceToast {
    pub(in crate::controllers::dash_tui) message: String,
    pub(in crate::controllers::dash_tui) is_error: bool,
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
            deployments: Vec::new(),
            selected_deployment: 0,
            focus: ServiceFocus::Overview,
            loading: false,
            error: None,
            current_request_id: 0,
            confirmation: None,
            toast: None,
        })
    }

    pub(in crate::controllers::dash_tui) fn start_loading(&mut self) {
        self.loading = true;
        self.error = None;
        self.current_request_id += 1;
    }

    pub(in crate::controllers::dash_tui) fn apply_loaded_deployments(
        &mut self,
        deployments: Vec<ServiceDeployment>,
    ) {
        let selected_deployment_id = self
            .selected_deployment()
            .map(|deployment| deployment.id.clone());
        self.deployments = deployments;
        self.loading = false;
        self.error = None;
        self.clamp_selected_deployment();

        if let Some(selected_deployment_id) = selected_deployment_id {
            self.select_deployment_by_id(&selected_deployment_id);
        }
    }

    pub(in crate::controllers::dash_tui) fn set_error(&mut self, error: String) {
        self.loading = false;
        self.error = Some(error);
    }

    pub(in crate::controllers::dash_tui) fn set_toast(
        &mut self,
        message: impl Into<String>,
        is_error: bool,
    ) {
        self.toast = Some(ServiceToast {
            message: message.into(),
            is_error,
        });
    }

    pub(in crate::controllers::dash_tui) fn clear_toast(&mut self) {
        self.toast = None;
    }

    pub(in crate::controllers::dash_tui) fn selected_deployment(
        &self,
    ) -> Option<&ServiceDeployment> {
        self.deployments.get(self.selected_deployment)
    }

    pub(in crate::controllers::dash_tui) fn move_deployment_up(&mut self) {
        self.selected_deployment = self.selected_deployment.saturating_sub(1);
    }

    pub(in crate::controllers::dash_tui) fn move_deployment_down(&mut self) {
        if !self.deployments.is_empty() {
            self.selected_deployment =
                (self.selected_deployment + 1).min(self.deployments.len() - 1);
        }
    }

    pub(in crate::controllers::dash_tui) fn focus_deployments(&mut self) {
        self.focus = ServiceFocus::Deployments;
    }

    pub(in crate::controllers::dash_tui) fn focus_overview(&mut self) {
        self.focus = ServiceFocus::Overview;
    }

    pub(in crate::controllers::dash_tui) fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            ServiceFocus::Overview => ServiceFocus::Deployments,
            ServiceFocus::Deployments => ServiceFocus::Overview,
        };
    }

    fn latest_restartable_deployment_id(&self) -> Result<String> {
        self.detail
            .service
            .latest_deployment
            .as_ref()
            .map(|deployment| deployment.id.clone())
            .ok_or_else(|| anyhow!("No deployment found for service"))
    }

    pub(in crate::controllers::dash_tui) fn open_redeploy_confirmation(&mut self) {
        match self.selected_deployment() {
            Some(deployment) => {
                self.confirmation = Some(ServiceConfirmationState::Redeploy {
                    deployment_id: deployment.id.clone(),
                });
                self.clear_toast();
            }
            None => self.set_toast("No deployment selected.", true),
        }
    }

    pub(in crate::controllers::dash_tui) fn open_restart_confirmation(&mut self) {
        match self.latest_restartable_deployment_id() {
            Ok(deployment_id) => {
                self.confirmation = Some(ServiceConfirmationState::Restart { deployment_id });
                self.clear_toast();
            }
            Err(error) => self.set_toast(error.to_string(), true),
        }
    }

    pub(in crate::controllers::dash_tui) fn open_rollback_confirmation(&mut self) {
        match self.selected_deployment() {
            Some(deployment) => {
                self.confirmation = Some(ServiceConfirmationState::Rollback {
                    deployment_id: deployment.id.clone(),
                });
                self.clear_toast();
            }
            None => self.set_toast("No deployment selected.", true),
        }
    }

    pub(in crate::controllers::dash_tui) fn select_deployment_by_id(
        &mut self,
        deployment_id: &str,
    ) {
        if let Some(index) = self
            .deployments
            .iter()
            .position(|deployment| deployment.id == deployment_id)
        {
            self.selected_deployment = index;
        } else {
            self.clamp_selected_deployment();
        }
    }

    pub(in crate::controllers::dash_tui) fn clamp_selected_deployment(&mut self) {
        if self.deployments.is_empty() {
            self.selected_deployment = 0;
        } else {
            self.selected_deployment = self.selected_deployment.min(self.deployments.len() - 1);
        }
    }

    pub(in crate::controllers::dash_tui) fn selected_confirmation(
        &self,
    ) -> Option<&ServiceConfirmationState> {
        self.confirmation.as_ref()
    }
}

pub(in crate::controllers::dash_tui) async fn load_service_deployments(
    detail: &ServiceDetail,
) -> Result<Vec<ServiceDeployment>> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    load_service_deployments_with_client(
        &client,
        &configs,
        &detail.project_id,
        &detail.environment_id,
        &detail.service.id,
    )
    .await
}

async fn load_service_deployments_with_client(
    client: &reqwest::Client,
    configs: &Configs,
    project_id: &str,
    environment_id: &str,
    service_id: &str,
) -> Result<Vec<ServiceDeployment>> {
    fetch_service_deployments(
        client,
        &configs.get_backboard(),
        project_id,
        environment_id,
        service_id,
        SERVICE_DEPLOYMENTS_LIMIT,
    )
    .await
}

pub(in crate::controllers::dash_tui) async fn redeploy_service(
    _detail: &ServiceDetail,
    deployment_id: &str,
) -> Result<String> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    redeploy_deployment(&client, &configs.get_backboard(), deployment_id).await
}

pub(in crate::controllers::dash_tui) async fn restart_service(
    detail: &ServiceDetail,
) -> Result<String> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    restart_latest_service_deployment(
        &client,
        &configs,
        &detail.project_id,
        &detail.environment_id,
        &detail.service.id,
    )
    .await
}

pub(in crate::controllers::dash_tui) async fn rollback_service_deployment(
    deployment_id: &str,
) -> Result<String> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    rollback_deployment(&client, &configs.get_backboard(), deployment_id).await
}

pub(in crate::controllers::dash_tui) fn handle_service_screen_key(
    state: &mut ServiceScreenState,
    key: KeyEvent,
) -> HandleKeyAction {
    if let Some(confirmation) = state.selected_confirmation().cloned() {
        return match key.code {
            KeyCode::Esc | KeyCode::Backspace => {
                state.confirmation = None;
                HandleKeyAction::None
            }
            KeyCode::Enter => match confirmation {
                ServiceConfirmationState::Redeploy { .. } => HandleKeyAction::RedeployService,
                ServiceConfirmationState::Restart { .. } => HandleKeyAction::RestartService,
                ServiceConfirmationState::Rollback { .. } => HandleKeyAction::RollbackDeployment,
            },
            _ => HandleKeyAction::None,
        };
    }

    match key.code {
        KeyCode::Esc | KeyCode::Backspace => HandleKeyAction::BackToProject,
        KeyCode::Up | KeyCode::Char('i') if matches!(state.focus, ServiceFocus::Deployments) => {
            state.move_deployment_up();
            HandleKeyAction::None
        }
        KeyCode::Down | KeyCode::Char('k') if matches!(state.focus, ServiceFocus::Deployments) => {
            state.move_deployment_down();
            HandleKeyAction::None
        }
        KeyCode::BackTab => {
            state.focus_overview();
            HandleKeyAction::None
        }
        KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
            state.focus_overview();
            HandleKeyAction::None
        }
        KeyCode::Tab => {
            state.toggle_focus();
            HandleKeyAction::None
        }
        KeyCode::Left | KeyCode::Char('j') => {
            state.focus_overview();
            HandleKeyAction::None
        }
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('d') => {
            state.focus_deployments();
            HandleKeyAction::None
        }
        KeyCode::Char('r') => {
            state.open_restart_confirmation();
            HandleKeyAction::None
        }
        KeyCode::Char('D') => {
            state.open_redeploy_confirmation();
            HandleKeyAction::None
        }
        KeyCode::Char('R') => {
            state.open_rollback_confirmation();
            HandleKeyAction::None
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        commands::queries::deployments::DeploymentStatus,
        controllers::dash_tui::data::{DeploymentSummary, ProjectLoadTarget},
    };
    use chrono::Utc;

    fn detail(can_redeploy: bool) -> ServiceDetail {
        ServiceDetail {
            project_id: "proj_123".to_string(),
            project_name: "api".to_string(),
            workspace_name: Some("workspace".to_string()),
            environment_id: "env_123".to_string(),
            environment_name: "production".to_string(),
            service: DashboardService {
                id: "svc_123".to_string(),
                name: "web".to_string(),
                active_in_environment: true,
                num_replicas: Some(1),
                latest_deployment: Some(DeploymentSummary {
                    id: "dep_123".to_string(),
                    status: "SUCCESS".to_string(),
                    created_at: Utc::now(),
                    can_redeploy,
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
        }
    }

    fn service_state() -> ServiceScreenState {
        ServiceScreenState {
            detail: detail(true),
            return_to_project: Box::new(ProjectScreenState::new(
                ProjectLoadTarget {
                    project_id: "proj_123".to_string(),
                    environment_hint: Some("production".to_string()),
                },
                None,
            )),
            deployments: vec![
                ServiceDeployment {
                    id: "dep_123".to_string(),
                    status: DeploymentStatus::SUCCESS,
                    created_at: Utc::now(),
                    meta: None,
                },
                ServiceDeployment {
                    id: "dep_122".to_string(),
                    status: DeploymentStatus::FAILED,
                    created_at: Utc::now(),
                    meta: None,
                },
            ],
            selected_deployment: 0,
            focus: ServiceFocus::Overview,
            loading: false,
            error: None,
            current_request_id: 0,
            confirmation: None,
            toast: None,
        }
    }

    #[test]
    fn d_focuses_deployments_and_down_moves_selection() {
        let mut state = service_state();

        let action = handle_service_screen_key(&mut state, KeyEvent::from(KeyCode::Char('d')));
        assert!(matches!(action, HandleKeyAction::None));
        assert!(matches!(state.focus, ServiceFocus::Deployments));

        let action = handle_service_screen_key(&mut state, KeyEvent::from(KeyCode::Down));
        assert!(matches!(action, HandleKeyAction::None));
        assert_eq!(state.selected_deployment, 1);
    }

    #[test]
    fn tab_toggles_focus_between_overview_and_deployments() {
        let mut state = service_state();

        let action = handle_service_screen_key(&mut state, KeyEvent::from(KeyCode::Tab));
        assert!(matches!(action, HandleKeyAction::None));
        assert!(matches!(state.focus, ServiceFocus::Deployments));

        let action = handle_service_screen_key(&mut state, KeyEvent::from(KeyCode::Tab));
        assert!(matches!(action, HandleKeyAction::None));
        assert!(matches!(state.focus, ServiceFocus::Overview));
    }

    #[test]
    fn uppercase_d_opens_confirmation_for_selected_deployment() {
        let mut state = service_state();
        state.focus_deployments();
        state.selected_deployment = 1;

        let action = handle_service_screen_key(&mut state, KeyEvent::from(KeyCode::Char('D')));

        assert!(matches!(action, HandleKeyAction::None));
        assert_eq!(
            state.selected_confirmation(),
            Some(&ServiceConfirmationState::Redeploy {
                deployment_id: "dep_122".to_string()
            })
        );
    }

    #[test]
    fn lowercase_r_opens_restart_confirmation_for_latest_deployment() {
        let mut state = service_state();

        let action = handle_service_screen_key(&mut state, KeyEvent::from(KeyCode::Char('r')));

        assert!(matches!(action, HandleKeyAction::None));
        assert_eq!(
            state.selected_confirmation(),
            Some(&ServiceConfirmationState::Restart {
                deployment_id: "dep_123".to_string()
            })
        );
    }

    #[test]
    fn uppercase_r_opens_rollback_confirmation_for_selected_deployment() {
        let mut state = service_state();
        state.focus_deployments();
        state.selected_deployment = 1;

        let action = handle_service_screen_key(&mut state, KeyEvent::from(KeyCode::Char('R')));

        assert!(matches!(action, HandleKeyAction::None));
        assert_eq!(
            state.selected_confirmation(),
            Some(&ServiceConfirmationState::Rollback {
                deployment_id: "dep_122".to_string()
            })
        );
    }
}
