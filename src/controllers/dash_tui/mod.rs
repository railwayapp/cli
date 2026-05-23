mod data;
mod project;
mod service;
mod ui;

use std::io::stdout;
use std::panic;
use std::time::Duration;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::Color;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use self::data::{
    DashboardProject, ProjectCard, ProjectLoadTarget, load_dashboard_project, load_project_cards,
};
use self::project::{
    EnvironmentSelectorState, ProjectScreenState, ProjectsBackNavigation, handle_project_screen_key,
};
use self::service::{
    ServiceConfirmationState, ServiceScreenState, handle_service_screen_key,
    load_service_deployments, redeploy_service as run_service_redeploy,
    restart_service as run_service_restart, rollback_service_deployment as run_service_rollback,
};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const RAILWAY_VIOLET: Color = Color::Rgb(127, 86, 217);
const RAILWAY_PURPLE: Color = Color::Rgb(155, 107, 255);
const RAILWAY_PINK: Color = Color::Rgb(236, 72, 153);
const RAILWAY_LAVENDER: Color = Color::Rgb(221, 214, 254);
const RAILWAY_MUTED: Color = Color::Rgb(161, 152, 190);
const RAILWAY_PANEL: Color = Color::Rgb(91, 78, 129);
const RAILWAY_ERROR: Color = Color::Rgb(248, 113, 113);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DashboardAuthMode {
    Workspace,
    LinkedProject {
        project_id: String,
        environment_id: String,
    },
}

#[derive(Clone, Debug)]
pub struct DashTuiParams {
    pub project: Option<String>,
    pub environment: Option<String>,
    pub auth_mode: DashboardAuthMode,
}

#[derive(Clone, Debug)]
struct DashApp {
    params: DashTuiParams,
    screen: DashboardScreen,
    spinner_tick: usize,
}

#[derive(Clone, Debug)]
enum DashboardScreen {
    Projects(ProjectsScreenState),
    Project(ProjectScreenState),
    Service(ServiceScreenState),
}

#[derive(Clone, Debug)]
struct ProjectsScreenState {
    cards: Vec<ProjectCard>,
    selected: usize,
    filter: String,
    filter_mode: bool,
    loading: bool,
    error: Option<String>,
    current_request_id: u64,
    initial_selection_hint: Option<String>,
}

enum LoaderEvent {
    ProjectsLoaded {
        request_id: u64,
        result: std::result::Result<Vec<ProjectCard>, String>,
    },
    ProjectLoaded {
        request_id: u64,
        result: std::result::Result<DashboardProject, String>,
    },
    ServiceDeploymentsLoaded {
        request_id: u64,
        result: std::result::Result<Vec<crate::controllers::deployment::ServiceDeployment>, String>,
    },
    ServiceRedeployed {
        result: std::result::Result<String, String>,
    },
    ServiceRestarted {
        result: std::result::Result<String, String>,
    },
    ServiceRolledBack {
        result: std::result::Result<String, String>,
    },
}

enum HandleKeyAction {
    None,
    OpenProject {
        project_id: String,
        return_to_projects: ProjectsBackNavigation,
    },
    OpenSelectedService,
    RefreshProjects,
    RefreshProject,
    RedeployService,
    RestartService,
    RollbackDeployment,
    BackToProjects,
    BackToProject,
    OpenEnvironmentSelector,
}

impl DashApp {
    fn new(params: DashTuiParams) -> Self {
        let screen = match &params.auth_mode {
            DashboardAuthMode::Workspace => match &params.project {
                Some(project_id) => DashboardScreen::Project(ProjectScreenState::new(
                    ProjectLoadTarget {
                        project_id: project_id.clone(),
                        environment_hint: params.environment.clone(),
                    },
                    Some(ProjectsBackNavigation::Reload {
                        initial_selection_hint: Some(project_id.clone()),
                    }),
                )),
                None => DashboardScreen::Projects(ProjectsScreenState::new(None)),
            },
            DashboardAuthMode::LinkedProject {
                project_id,
                environment_id,
            } => DashboardScreen::Project(ProjectScreenState::new(
                ProjectLoadTarget {
                    project_id: project_id.clone(),
                    environment_hint: Some(environment_id.clone()),
                },
                None,
            )),
        };

        Self {
            params,
            screen,
            spinner_tick: 0,
        }
    }

    fn start_initial_load(&mut self, tx: &mpsc::UnboundedSender<LoaderEvent>) {
        match self.screen {
            DashboardScreen::Projects(_) => self.refresh_projects(tx),
            DashboardScreen::Project(_) => self.refresh_project(tx),
            DashboardScreen::Service(_) => self.refresh_service_deployments(tx),
        }
    }

    fn refresh_projects(&mut self, tx: &mpsc::UnboundedSender<LoaderEvent>) {
        let DashboardScreen::Projects(state) = &mut self.screen else {
            return;
        };

        state.loading = true;
        state.error = None;
        state.current_request_id += 1;
        let request_id = state.current_request_id;
        let tx = tx.clone();

        tokio::spawn(async move {
            let result = load_project_cards()
                .await
                .map_err(|error| error.to_string());
            let _ = tx.send(LoaderEvent::ProjectsLoaded { request_id, result });
        });
    }

    fn refresh_project(&mut self, tx: &mpsc::UnboundedSender<LoaderEvent>) {
        let DashboardScreen::Project(state) = &mut self.screen else {
            return;
        };

        state.loading = true;
        state.error = None;
        state.current_request_id += 1;
        let request_id = state.current_request_id;
        let target = state.target.clone();
        let tx = tx.clone();

        tokio::spawn(async move {
            let result = load_dashboard_project(target)
                .await
                .map_err(|error| error.to_string());
            let _ = tx.send(LoaderEvent::ProjectLoaded { request_id, result });
        });
    }

    fn open_project(
        &mut self,
        project_id: String,
        return_to_projects: Option<ProjectsBackNavigation>,
        tx: &mpsc::UnboundedSender<LoaderEvent>,
    ) {
        let environment_hint = self.params.environment.clone();
        self.screen = DashboardScreen::Project(ProjectScreenState::new(
            ProjectLoadTarget {
                project_id,
                environment_hint,
            },
            return_to_projects,
        ));
        self.refresh_project(tx);
    }

    fn open_selected_service(&mut self, tx: &mpsc::UnboundedSender<LoaderEvent>) {
        let DashboardScreen::Project(state) = &self.screen else {
            return;
        };

        let Some(service_screen) = ServiceScreenState::from_project(state) else {
            return;
        };

        self.screen = DashboardScreen::Service(service_screen);
        self.refresh_service_deployments(tx);
    }

    fn refresh_service_deployments(&mut self, tx: &mpsc::UnboundedSender<LoaderEvent>) {
        let DashboardScreen::Service(state) = &mut self.screen else {
            return;
        };

        state.start_loading();
        let request_id = state.current_request_id;
        let detail = state.detail.clone();
        let tx = tx.clone();

        tokio::spawn(async move {
            let result = load_service_deployments(&detail)
                .await
                .map_err(|error| error.to_string());
            let _ = tx.send(LoaderEvent::ServiceDeploymentsLoaded { request_id, result });
        });
    }

    fn redeploy_service(&mut self, tx: &mpsc::UnboundedSender<LoaderEvent>) {
        let DashboardScreen::Service(state) = &mut self.screen else {
            return;
        };

        let deployment_id = match state.selected_confirmation().cloned() {
            Some(ServiceConfirmationState::Redeploy { deployment_id }) => deployment_id,
            _ => return,
        };

        state.confirmation = None;
        state.loading = true;
        state.error = None;
        state.clear_toast();
        let detail = state.detail.clone();
        let tx = tx.clone();

        tokio::spawn(async move {
            let result = run_service_redeploy(&detail, &deployment_id)
                .await
                .map_err(|error| error.to_string());
            let _ = tx.send(LoaderEvent::ServiceRedeployed { result });
        });
    }

    fn restart_service(&mut self, tx: &mpsc::UnboundedSender<LoaderEvent>) {
        let DashboardScreen::Service(state) = &mut self.screen else {
            return;
        };

        if !matches!(
            state.selected_confirmation(),
            Some(ServiceConfirmationState::Restart { .. })
        ) {
            return;
        }

        state.confirmation = None;
        state.loading = true;
        state.error = None;
        state.clear_toast();
        let detail = state.detail.clone();
        let tx = tx.clone();

        tokio::spawn(async move {
            let result = run_service_restart(&detail)
                .await
                .map_err(|error| error.to_string());
            let _ = tx.send(LoaderEvent::ServiceRestarted { result });
        });
    }

    fn rollback_service_deployment(&mut self, tx: &mpsc::UnboundedSender<LoaderEvent>) {
        let DashboardScreen::Service(state) = &mut self.screen else {
            return;
        };

        let deployment_id = match state.selected_confirmation().cloned() {
            Some(ServiceConfirmationState::Rollback { deployment_id }) => deployment_id,
            _ => return,
        };

        state.confirmation = None;
        state.loading = true;
        state.error = None;
        state.clear_toast();
        let tx = tx.clone();

        tokio::spawn(async move {
            let result = run_service_rollback(&deployment_id)
                .await
                .map_err(|error| error.to_string());
            let _ = tx.send(LoaderEvent::ServiceRolledBack { result });
        });
    }

    fn open_environment_selector(&mut self) {
        let DashboardScreen::Project(state) = &mut self.screen else {
            return;
        };

        let Some(project) = &state.project else {
            return;
        };

        let environments = project.accessible_environments();
        if environments.is_empty() {
            return;
        }

        let selected = environments
            .iter()
            .position(|environment| environment.id == project.selected_environment_id)
            .unwrap_or(0);
        state.environment_selector = Some(EnvironmentSelectorState { selected });
    }

    fn switch_environment(
        &mut self,
        selected_index: usize,
        tx: &mpsc::UnboundedSender<LoaderEvent>,
    ) {
        let DashboardScreen::Project(state) = &mut self.screen else {
            return;
        };

        let Some(project) = &state.project else {
            return;
        };

        let accessible_environments = project.accessible_environments();
        let Some(environment) = accessible_environments.get(selected_index) else {
            return;
        };

        state.target.environment_hint = Some(environment.id.clone());
        state.environment_selector = None;
        self.refresh_project(tx);
    }

    fn back_to_project(&mut self) {
        let DashboardScreen::Service(state) = &self.screen else {
            return;
        };

        self.screen = DashboardScreen::Project((*state.return_to_project).clone());
    }

    fn back_to_projects(&mut self, tx: &mpsc::UnboundedSender<LoaderEvent>) {
        let DashboardScreen::Project(state) = &self.screen else {
            return;
        };

        let navigation = state
            .return_to_projects
            .clone()
            .or_else(|| match self.params.auth_mode {
                DashboardAuthMode::Workspace => Some(ProjectsBackNavigation::Reload {
                    initial_selection_hint: Some(state.target.project_id.clone()),
                }),
                DashboardAuthMode::LinkedProject { .. } => None,
            });

        match navigation {
            Some(ProjectsBackNavigation::Restore(restored)) => {
                self.screen = DashboardScreen::Projects(*restored);
            }
            Some(ProjectsBackNavigation::Reload {
                initial_selection_hint,
            }) => {
                self.screen =
                    DashboardScreen::Projects(ProjectsScreenState::new(initial_selection_hint));
                self.refresh_projects(tx);
            }
            None => {}
        }
    }

    fn handle_loader_event(&mut self, event: LoaderEvent, tx: &mpsc::UnboundedSender<LoaderEvent>) {
        match event {
            LoaderEvent::ProjectsLoaded { request_id, result } => {
                let DashboardScreen::Projects(state) = &mut self.screen else {
                    return;
                };

                if request_id != state.current_request_id {
                    return;
                }

                match result {
                    Ok(cards) => state.apply_loaded_cards(cards),
                    Err(error) => state.set_error(error),
                }
            }
            LoaderEvent::ProjectLoaded { request_id, result } => {
                let DashboardScreen::Project(state) = &mut self.screen else {
                    return;
                };

                if request_id != state.current_request_id {
                    return;
                }

                match result {
                    Ok(project) => state.apply_loaded_project(project),
                    Err(error) => state.set_error(error),
                }
            }
            LoaderEvent::ServiceDeploymentsLoaded { request_id, result } => {
                let DashboardScreen::Service(state) = &mut self.screen else {
                    return;
                };

                if request_id != state.current_request_id {
                    return;
                }

                match result {
                    Ok(deployments) => state.apply_loaded_deployments(deployments),
                    Err(error) => state.set_error(error),
                }
            }
            LoaderEvent::ServiceRedeployed { result } => {
                let DashboardScreen::Service(state) = &mut self.screen else {
                    return;
                };

                state.loading = false;
                match result {
                    Ok(deployment_id) => {
                        state.set_toast(
                            format!("Redeploy triggered for deployment {deployment_id}"),
                            false,
                        );
                        self.refresh_service_deployments(tx);
                    }
                    Err(error) => {
                        state.set_toast(error, true);
                    }
                }
            }
            LoaderEvent::ServiceRestarted { result } => {
                let DashboardScreen::Service(state) = &mut self.screen else {
                    return;
                };

                state.loading = false;
                match result {
                    Ok(deployment_id) => {
                        state.set_toast(
                            format!("Restart triggered for deployment {deployment_id}"),
                            false,
                        );
                        self.refresh_service_deployments(tx);
                    }
                    Err(error) => {
                        state.set_toast(error, true);
                    }
                }
            }
            LoaderEvent::ServiceRolledBack { result } => {
                let DashboardScreen::Service(state) = &mut self.screen else {
                    return;
                };

                state.loading = false;
                match result {
                    Ok(deployment_id) => {
                        state.set_toast(
                            format!("Rollback triggered to deployment {deployment_id}"),
                            false,
                        );
                        self.refresh_service_deployments(tx);
                    }
                    Err(error) => {
                        state.set_toast(error, true);
                    }
                }
            }
        }
    }

    fn handle_event(
        &mut self,
        event: Event,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
        tx: &mpsc::UnboundedSender<LoaderEvent>,
    ) -> bool {
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                let size = terminal.size().unwrap_or_default();
                self.handle_key(key, Rect::new(0, 0, size.width, size.height), tx)
            }
            Event::Resize(_, _) => {
                let _ = terminal.clear();
                false
            }
            _ => false,
        }
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        terminal_area: Rect,
        tx: &mpsc::UnboundedSender<LoaderEvent>,
    ) -> bool {
        if matches!(key.code, KeyCode::Char('q'))
            || (matches!(key.code, KeyCode::Char('c'))
                && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return true;
        }

        if matches!(
            self.screen,
            DashboardScreen::Projects(ProjectsScreenState {
                filter_mode: true,
                ..
            })
        ) && let DashboardScreen::Projects(state) = &mut self.screen
        {
            handle_projects_filter_input(state, key);
            return false;
        }

        if let DashboardScreen::Project(state) = &mut self.screen
            && state.environment_selector.is_some()
        {
            return self.handle_environment_selector_key(key, tx);
        }

        let action = match &mut self.screen {
            DashboardScreen::Projects(state) => {
                handle_projects_screen_key(state, key, terminal_area)
            }
            DashboardScreen::Project(state) => handle_project_screen_key(state, key, terminal_area),
            DashboardScreen::Service(state) => handle_service_screen_key(state, key),
        };

        match action {
            HandleKeyAction::None => {}
            HandleKeyAction::OpenProject {
                project_id,
                return_to_projects,
            } => self.open_project(project_id, Some(return_to_projects), tx),
            HandleKeyAction::OpenSelectedService => self.open_selected_service(tx),
            HandleKeyAction::RefreshProjects => self.refresh_projects(tx),
            HandleKeyAction::RefreshProject => self.refresh_project(tx),
            HandleKeyAction::RedeployService => self.redeploy_service(tx),
            HandleKeyAction::RestartService => self.restart_service(tx),
            HandleKeyAction::RollbackDeployment => self.rollback_service_deployment(tx),
            HandleKeyAction::BackToProjects => self.back_to_projects(tx),
            HandleKeyAction::BackToProject => self.back_to_project(),
            HandleKeyAction::OpenEnvironmentSelector => self.open_environment_selector(),
        }

        false
    }

    fn handle_environment_selector_key(
        &mut self,
        key: KeyEvent,
        tx: &mpsc::UnboundedSender<LoaderEvent>,
    ) -> bool {
        let DashboardScreen::Project(state) = &mut self.screen else {
            return false;
        };
        let Some(selector) = &mut state.environment_selector else {
            return false;
        };

        let environment_count = state
            .project
            .as_ref()
            .map(|project| project.accessible_environments().len())
            .unwrap_or(0);

        match key.code {
            KeyCode::Esc | KeyCode::Backspace => state.environment_selector = None,
            KeyCode::Up | KeyCode::Char('i') => {
                selector.selected = selector.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('k') => {
                if environment_count > 0 {
                    selector.selected = (selector.selected + 1).min(environment_count - 1);
                }
            }
            KeyCode::Enter => {
                let selected = selector.selected;
                self.switch_environment(selected, tx);
            }
            _ => {}
        }

        false
    }

    fn on_tick(&mut self) {
        self.spinner_tick = (self.spinner_tick + 1) % SPINNER_FRAMES.len();
    }
}

impl ProjectsScreenState {
    fn new(initial_selection_hint: Option<String>) -> Self {
        Self {
            cards: Vec::new(),
            selected: 0,
            filter: String::new(),
            filter_mode: false,
            loading: false,
            error: None,
            current_request_id: 0,
            initial_selection_hint,
        }
    }

    fn visible_indices(&self) -> Vec<usize> {
        self.cards
            .iter()
            .enumerate()
            .filter_map(|(index, card)| card.matches_filter(&self.filter).then_some(index))
            .collect()
    }

    fn selected_card(&self) -> Option<&ProjectCard> {
        let visible = self.visible_indices();
        visible
            .get(self.selected)
            .and_then(|index| self.cards.get(*index))
    }

    fn move_left(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn move_right(&mut self) {
        let visible_len = self.visible_indices().len();
        if visible_len > 0 {
            self.selected = (self.selected + 1).min(visible_len - 1);
        }
    }

    fn move_up(&mut self, columns: usize) {
        self.selected = self.selected.saturating_sub(columns.max(1));
    }

    fn move_down(&mut self, columns: usize) {
        let visible_len = self.visible_indices().len();
        if visible_len > 0 {
            self.selected = (self.selected + columns.max(1)).min(visible_len - 1);
        }
    }

    fn apply_loaded_cards(&mut self, cards: Vec<ProjectCard>) {
        let preferred_id = self
            .selected_card()
            .map(|card| card.id.clone())
            .or_else(|| self.initial_selection_hint.clone());

        self.cards = cards;
        self.loading = false;
        self.error = None;
        self.clamp_selection();

        if let Some(preferred_id) = preferred_id {
            self.select_by_project_id(&preferred_id);
        }

        self.initial_selection_hint = None;
    }

    fn set_error(&mut self, error: String) {
        self.loading = false;
        self.error = Some(error);
        self.clamp_selection();
    }

    fn select_by_project_id(&mut self, project_id: &str) {
        let visible = self.visible_indices();
        if let Some(position) = visible
            .iter()
            .position(|index| self.cards[*index].id == project_id)
        {
            self.selected = position;
        } else {
            self.clamp_selection();
        }
    }

    fn clamp_selection(&mut self) {
        let visible_len = self.visible_indices().len();
        if visible_len == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(visible_len - 1);
        }
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;

    let rollback = scopeguard::guard((), |_| {
        restore_terminal();
    });

    execute!(stdout(), EnterAlternateScreen, Hide, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    std::mem::forget(rollback);

    Ok(terminal)
}

fn restore_terminal() {
    let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}

pub async fn run(params: DashTuiParams) -> Result<()> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    let mut terminal = setup_terminal()?;
    let _cleanup = scopeguard::guard((), |_| {
        restore_terminal();
    });

    let mut app = DashApp::new(params);
    let mut events = EventStream::new();
    let (loader_tx, mut loader_rx) = mpsc::unbounded_channel();
    app.start_initial_load(&loader_tx);

    let mut tick = tokio::time::interval(Duration::from_millis(120));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        terminal.draw(|frame| ui::render(frame, &app))?;

        tokio::select! {
            Some(Ok(event)) = events.next() => {
                if app.handle_event(event, &mut terminal, &loader_tx) {
                    break;
                }
            }
            Some(loader_event) = loader_rx.recv() => {
                app.handle_loader_event(loader_event, &loader_tx);
            }
            _ = tick.tick() => {
                app.on_tick();
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }

    Ok(())
}

fn handle_projects_screen_key(
    state: &mut ProjectsScreenState,
    key: KeyEvent,
    terminal_area: Rect,
) -> HandleKeyAction {
    let columns = ui::project_navigation_columns(terminal_area);

    match key.code {
        KeyCode::Up | KeyCode::Char('i') => state.move_up(columns),
        KeyCode::Down | KeyCode::Char('k') => state.move_down(columns),
        KeyCode::Left | KeyCode::Char('j') => state.move_left(),
        KeyCode::Right | KeyCode::Char('l') => state.move_right(),
        KeyCode::Enter => {
            if let Some(card) = state.selected_card().cloned() {
                return HandleKeyAction::OpenProject {
                    project_id: card.id,
                    return_to_projects: ProjectsBackNavigation::Restore(Box::new(state.clone())),
                };
            }
        }
        KeyCode::Char('/') => state.filter_mode = true,
        KeyCode::Char('r') => return HandleKeyAction::RefreshProjects,
        _ => {}
    }

    HandleKeyAction::None
}

fn handle_projects_filter_input(state: &mut ProjectsScreenState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => state.filter_mode = false,
        KeyCode::Enter => state.filter_mode = false,
        KeyCode::Backspace => {
            state.filter.pop();
            state.clamp_selection();
        }
        KeyCode::Delete => {
            state.filter.clear();
            state.selected = 0;
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.filter.clear();
            state.selected = 0;
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.filter.push(ch);
            state.selected = 0;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controllers::dash_tui::data::DashboardService;

    fn card(id: &str, name: &str) -> ProjectCard {
        ProjectCard {
            id: id.to_string(),
            name: name.to_string(),
            workspace_name: Some("workspace".to_string()),
            service_count: 2,
            environment_count: 3,
        }
    }

    fn service(id: &str, name: &str) -> DashboardService {
        DashboardService {
            id: id.to_string(),
            name: name.to_string(),
            active_in_environment: true,
            num_replicas: Some(1),
            latest_deployment: None,
            domains: Vec::new(),
            source_repo: None,
            source_image: None,
            cron_schedule: None,
            next_cron_run_at: None,
            start_command: None,
            volume_mounts: Vec::new(),
        }
    }

    fn project() -> DashboardProject {
        DashboardProject {
            id: "proj_123".to_string(),
            name: "api".to_string(),
            workspace_name: Some("workspace".to_string()),
            selected_environment_id: "env_123".to_string(),
            selected_environment_name: "production".to_string(),
            environments: Vec::new(),
            services: vec![service("svc_1", "one"), service("svc_2", "two")],
        }
    }

    #[test]
    fn refresh_preserves_selection_by_project_id() {
        let mut state = ProjectsScreenState::new(None);
        state.cards = vec![card("one", "one"), card("two", "two")];
        state.selected = 1;

        state.apply_loaded_cards(vec![card("zero", "zero"), card("two", "two")]);

        assert_eq!(
            state.selected_card().map(|card| card.id.as_str()),
            Some("two")
        );
    }

    #[test]
    fn initial_selection_hint_is_applied_after_first_load() {
        let mut state = ProjectsScreenState::new(Some("proj_two".to_string()));
        state.apply_loaded_cards(vec![card("proj_one", "one"), card("proj_two", "two")]);

        assert_eq!(
            state.selected_card().map(|card| card.id.as_str()),
            Some("proj_two")
        );
    }

    #[test]
    fn project_grid_has_at_least_one_column() {
        assert_eq!(ui::project_grid_columns(10), 1);
        assert!(ui::project_grid_columns(120) >= 1);
    }

    #[test]
    fn service_grid_has_at_least_one_column() {
        assert_eq!(ui::service_grid_columns(10), 1);
        assert!(ui::service_grid_columns(120) >= 1);
    }

    #[test]
    fn project_refresh_preserves_selected_service_by_id() {
        let mut state = ProjectScreenState::new(
            ProjectLoadTarget {
                project_id: "proj_123".to_string(),
                environment_hint: Some("production".to_string()),
            },
            None,
        );
        state.project = Some(project());
        state.selected_service = 1;

        let mut refreshed = project();
        refreshed.services = vec![service("svc_0", "zero"), service("svc_2", "two")];
        state.apply_loaded_project(refreshed);

        assert_eq!(
            state.selected_service().map(|service| service.id.as_str()),
            Some("svc_2")
        );
    }

    #[test]
    fn back_to_projects_restores_cached_screen_without_reloading() {
        let mut restored = ProjectsScreenState::new(Some("proj_two".to_string()));
        restored.cards = vec![card("proj_one", "one"), card("proj_two", "two")];
        restored.selected = 0;
        restored.filter = "tw".to_string();
        restored.current_request_id = 7;

        let mut app = DashApp {
            params: DashTuiParams {
                project: None,
                environment: None,
                auth_mode: DashboardAuthMode::Workspace,
            },
            screen: DashboardScreen::Project(ProjectScreenState::new(
                ProjectLoadTarget {
                    project_id: "proj_two".to_string(),
                    environment_hint: Some("production".to_string()),
                },
                Some(ProjectsBackNavigation::Restore(Box::new(restored.clone()))),
            )),
            spinner_tick: 0,
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        app.back_to_projects(&tx);

        match app.screen {
            DashboardScreen::Projects(state) => {
                assert_eq!(
                    state.selected_card().map(|card| card.id.as_str()),
                    Some("proj_two")
                );
                assert_eq!(state.filter, "tw");
                assert_eq!(state.current_request_id, 7);
                assert!(!state.loading);
            }
            DashboardScreen::Project(_) | DashboardScreen::Service(_) => {
                panic!("expected projects screen after backing out")
            }
        }
    }

    #[test]
    fn open_service_and_back_restores_project_screen_state() {
        let mut project_state = ProjectScreenState::new(
            ProjectLoadTarget {
                project_id: "proj_123".to_string(),
                environment_hint: Some("production".to_string()),
            },
            None,
        );
        project_state.project = Some(project());
        project_state.selected_service = 1;

        let mut app = DashApp {
            params: DashTuiParams {
                project: None,
                environment: None,
                auth_mode: DashboardAuthMode::Workspace,
            },
            screen: DashboardScreen::Project(project_state.clone()),
            spinner_tick: 0,
        };

        app.screen = DashboardScreen::Service(
            ServiceScreenState::from_project(&project_state)
                .expect("expected selected service to exist"),
        );

        match &app.screen {
            DashboardScreen::Service(state) => {
                assert_eq!(state.detail.service.id, "svc_2");
                assert_eq!(state.detail.project_name, "api");
                assert_eq!(state.detail.environment_name, "production");
            }
            DashboardScreen::Projects(_) | DashboardScreen::Project(_) => {
                panic!("expected service screen after opening selected service")
            }
        }

        app.back_to_project();

        match app.screen {
            DashboardScreen::Project(state) => {
                assert_eq!(
                    state.selected_service().map(|service| service.id.as_str()),
                    Some("svc_2")
                );
            }
            DashboardScreen::Projects(_) | DashboardScreen::Service(_) => {
                panic!("expected project screen after backing out of service detail")
            }
        }
    }
}
