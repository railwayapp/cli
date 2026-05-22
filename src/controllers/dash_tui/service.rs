use anyhow::{Result, anyhow};
use crossterm::event::{KeyCode, KeyEvent};

use super::{
    HandleKeyAction,
    data::{
        DashboardProject, DashboardService, ProjectLoadTarget, find_dashboard_service,
        load_dashboard_project_with_client,
    },
    project::{ProjectScreenState, ProjectsBackNavigation},
};
use crate::{
    client::GQLClient,
    commands::Configs,
    controllers::deployment::{
        ServiceDeployment, fetch_service_deployments, redeploy_latest_service_deployment,
    },
};

const SERVICE_DEPLOYMENTS_LIMIT: i64 = 20;

#[derive(Clone, Debug)]
pub(in crate::controllers::dash_tui) struct ServiceScreenState {
    pub(in crate::controllers::dash_tui) detail: ServiceDetail,
    pub(in crate::controllers::dash_tui) return_to_project: Box<ProjectScreenState>,
    pub(in crate::controllers::dash_tui) deployments: Vec<ServiceDeployment>,
    pub(in crate::controllers::dash_tui) loading: bool,
    pub(in crate::controllers::dash_tui) error: Option<String>,
    pub(in crate::controllers::dash_tui) current_request_id: u64,
    pub(in crate::controllers::dash_tui) redeploy_confirmation: Option<RedeployConfirmationState>,
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
pub(in crate::controllers::dash_tui) struct RedeployConfirmationState {
    pub(in crate::controllers::dash_tui) deployment_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::controllers::dash_tui) struct ServiceToast {
    pub(in crate::controllers::dash_tui) message: String,
    pub(in crate::controllers::dash_tui) is_error: bool,
}

#[derive(Clone, Debug)]
pub(in crate::controllers::dash_tui) struct ServiceScreenLoadedData {
    pub(in crate::controllers::dash_tui) detail: ServiceDetail,
    pub(in crate::controllers::dash_tui) return_to_project: Box<ProjectScreenState>,
    pub(in crate::controllers::dash_tui) deployments: Vec<ServiceDeployment>,
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
            loading: false,
            error: None,
            current_request_id: 0,
            redeploy_confirmation: None,
            toast: None,
        })
    }

    pub(in crate::controllers::dash_tui) fn start_loading(&mut self) {
        self.loading = true;
        self.error = None;
        self.current_request_id += 1;
    }

    pub(in crate::controllers::dash_tui) fn apply_loaded_data(
        &mut self,
        data: ServiceScreenLoadedData,
    ) {
        self.detail = data.detail;
        self.return_to_project = data.return_to_project;
        self.deployments = data.deployments;
        self.loading = false;
        self.error = None;
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

    fn latest_redeployable_deployment_id(&self) -> Result<String> {
        let latest = self
            .detail
            .service
            .latest_deployment
            .as_ref()
            .ok_or_else(|| anyhow!("No deployment found for service"))?;

        if !latest.can_redeploy {
            return Err(anyhow!(
                "The latest deployment for service {} cannot be redeployed.",
                self.detail.service.name
            ));
        }

        Ok(latest.id.clone())
    }

    pub(in crate::controllers::dash_tui) fn open_redeploy_confirmation(&mut self) {
        match self.latest_redeployable_deployment_id() {
            Ok(deployment_id) => {
                self.redeploy_confirmation = Some(RedeployConfirmationState { deployment_id });
                self.clear_toast();
            }
            Err(error) => {
                self.set_toast(error.to_string(), true);
            }
        }
    }

    pub(in crate::controllers::dash_tui) fn service_id(&self) -> String {
        self.detail.service.id.clone()
    }

    pub(in crate::controllers::dash_tui) fn refresh_target(&self) -> ProjectLoadTarget {
        self.return_to_project.target.clone()
    }

    pub(in crate::controllers::dash_tui) fn return_to_projects_nav(
        &self,
    ) -> Option<ProjectsBackNavigation> {
        self.return_to_project.return_to_projects.clone()
    }
}

pub(in crate::controllers::dash_tui) async fn load_service_screen_data(
    target: ProjectLoadTarget,
    service_id: String,
    return_to_projects: Option<ProjectsBackNavigation>,
) -> Result<ServiceScreenLoadedData> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let project = load_dashboard_project_with_client(&configs, &client, target.clone()).await?;
    let service = find_dashboard_service(&project, &service_id)
        .cloned()
        .ok_or_else(|| anyhow!("Service does not exist in the selected environment"))?;

    let deployments = fetch_service_deployments(
        &client,
        &configs.get_backboard(),
        &project.id,
        &project.selected_environment_id,
        &service_id,
        SERVICE_DEPLOYMENTS_LIMIT,
    )
    .await?;

    let mut project_state = ProjectScreenState::new(target, return_to_projects);
    project_state.apply_loaded_project(project.clone());
    project_state.select_service_by_id(&service_id);

    Ok(ServiceScreenLoadedData {
        detail: service_detail_from_project(&project, service),
        return_to_project: Box::new(project_state),
        deployments,
    })
}

pub(in crate::controllers::dash_tui) async fn redeploy_service(
    detail: &ServiceDetail,
) -> Result<String> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    redeploy_latest_service_deployment(
        &client,
        &configs,
        &detail.project_id,
        &detail.environment_id,
        &detail.service.id,
        &detail.service.name,
    )
    .await
}

pub(in crate::controllers::dash_tui) fn handle_service_screen_key(
    state: &mut ServiceScreenState,
    key: KeyEvent,
) -> HandleKeyAction {
    if state.redeploy_confirmation.is_some() {
        return match key.code {
            KeyCode::Esc | KeyCode::Backspace => {
                state.redeploy_confirmation = None;
                HandleKeyAction::None
            }
            KeyCode::Enter => {
                state.redeploy_confirmation = None;
                HandleKeyAction::RedeployService
            }
            _ => HandleKeyAction::None,
        };
    }

    match key.code {
        KeyCode::Esc | KeyCode::Backspace => HandleKeyAction::BackToProject,
        KeyCode::Char('r') => HandleKeyAction::RefreshService,
        KeyCode::Char('R') => {
            state.open_redeploy_confirmation();
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
    use crate::controllers::dash_tui::data::DeploymentSummary;
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

    #[test]
    fn uppercase_r_opens_confirmation_for_redeployable_latest_deployment() {
        let mut state = ServiceScreenState {
            detail: detail(true),
            return_to_project: Box::new(ProjectScreenState::new(
                ProjectLoadTarget {
                    project_id: "proj_123".to_string(),
                    environment_hint: Some("production".to_string()),
                },
                None,
            )),
            deployments: Vec::new(),
            loading: false,
            error: None,
            current_request_id: 0,
            redeploy_confirmation: None,
            toast: None,
        };

        let action = handle_service_screen_key(&mut state, KeyEvent::from(KeyCode::Char('R')));

        assert!(matches!(action, HandleKeyAction::None));
        assert_eq!(
            state
                .redeploy_confirmation
                .as_ref()
                .map(|c| c.deployment_id.as_str()),
            Some("dep_123")
        );
    }
}
