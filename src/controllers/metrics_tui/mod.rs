mod app;
mod ui;

use std::io::stdout;
use std::panic;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::{StreamExt, stream};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use app::{
    MetricsApp, ProjectApp, ProjectDetailResult, ProjectRefreshOptions, ProjectRefreshResult,
    ServiceRefreshOptions, fetch_project_detail, fetch_project_http_summary, fetch_project_refresh,
    fetch_service_refresh,
};

use crate::commands::metrics::Sections;
use crate::controllers::database::DatabaseType;
use crate::queries::project::ProjectProject;

/// Parameters for single-service metrics TUI
#[derive(Clone)]
pub struct ServiceTuiParams {
    pub client: reqwest::Client,
    pub backboard: String,
    pub service_id: String,
    pub service_name: String,
    pub environment_id: String,
    pub environment_name: String,
    pub since_label: String,
    pub sections: Sections,
    pub is_db: bool,
    pub db_stats_supported: bool,
    pub method: Option<String>,
    pub path: Option<String>,
    pub volumes: Vec<crate::controllers::metrics::VolumeMetrics>,
    // For native SSH db stats (only set when is_db)
    pub db_type: Option<DatabaseType>,
    pub service_instance_id: Option<String>,
    /// Populated when a local preflight decided DB stats can't run (e.g. no SSH key).
    /// The Stats tab shows this instead of hanging on "Loading...".
    pub db_stats_preflight_error: Option<String>,
}

/// Parameters for project-wide metrics TUI (--all)
#[derive(Clone)]
pub struct ProjectTuiParams {
    pub client: reqwest::Client,
    pub backboard: String,
    pub project_id: String,
    pub project: ProjectProject,
    pub environment_id: String,
    pub environment_name: String,
    pub method: Option<String>,
    pub path: Option<String>,
    pub since_label: String,
    pub sections: Sections,
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    execute!(
        stdout(),
        EnterAlternateScreen,
        Hide,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal() {
    let _ = execute!(
        stdout(),
        LeaveAlternateScreen,
        Show,
        crossterm::event::DisableMouseCapture
    );
    let _ = disable_raw_mode();
}

/// Time range presets matching the Railway dashboard
const TIME_RANGES: [&str; 5] = ["1h", "6h", "1d", "7d", "30d"];
const POLL_INTERVALS_SECS: [u64; 5] = [5, 10, 30, 60, 180];
const FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);
const PROJECT_HTTP_CONCURRENCY: usize = 6;

pub(crate) fn normalize_time_range_label(since: &str) -> Option<String> {
    let normalized = since.trim().to_ascii_lowercase();
    TIME_RANGES
        .contains(&normalized.as_str())
        .then_some(normalized)
}

pub(crate) fn supported_time_ranges_label() -> String {
    TIME_RANGES.join(", ")
}

fn spawn_service_refresh(
    tx: mpsc::UnboundedSender<app::ServiceRefreshResult>,
    params: ServiceTuiParams,
    request_id: u64,
    time_range_idx: usize,
    options: ServiceRefreshOptions,
) {
    tokio::spawn(async move {
        let result = fetch_service_refresh(params, request_id, time_range_idx, options).await;
        let _ = tx.send(result);
    });
}

enum ProjectFetchMsg {
    Refresh(ProjectRefreshResult),
    Detail(ProjectDetailResult),
    Http(app::ProjectHttpResult),
}

fn spawn_project_refresh(
    tx: mpsc::UnboundedSender<ProjectFetchMsg>,
    params: ProjectTuiParams,
    request_id: u64,
    time_range_idx: usize,
    options: ProjectRefreshOptions,
) {
    tokio::spawn(async move {
        let result = fetch_project_refresh(params, request_id, time_range_idx, options).await;
        let _ = tx.send(ProjectFetchMsg::Refresh(result));
    });
}

fn spawn_project_detail(
    tx: mpsc::UnboundedSender<ProjectFetchMsg>,
    params: ProjectTuiParams,
    request: app::ProjectDetailRequest,
) {
    tokio::spawn(async move {
        let result = fetch_project_detail(params, request).await;
        let _ = tx.send(ProjectFetchMsg::Detail(result));
    });
}

fn spawn_project_http_summaries(
    tx: mpsc::UnboundedSender<ProjectFetchMsg>,
    params: ProjectTuiParams,
    request_id: u64,
    time_range_idx: usize,
    service_ids: Vec<String>,
) {
    tokio::spawn(async move {
        stream::iter(service_ids)
            .map(|service_id| {
                let params = params.clone();
                async move {
                    fetch_project_http_summary(params, request_id, time_range_idx, service_id).await
                }
            })
            .buffer_unordered(PROJECT_HTTP_CONCURRENCY)
            .for_each(|result| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(ProjectFetchMsg::Http(result));
                }
            })
            .await;
    });
}

/// Run single-service metrics TUI
pub async fn run(params: ServiceTuiParams) -> Result<()> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    let mut terminal = setup_terminal()?;
    let _cleanup = scopeguard::guard((), |_| {
        restore_terminal();
    });

    let mut app = MetricsApp::new(&params);
    let mut events = EventStream::new();
    let (refresh_tx, mut refresh_rx) = mpsc::unbounded_channel();
    let mut refresh_request_id = 0u64;
    let mut active_refresh_request_id: u64;

    refresh_request_id += 1;
    active_refresh_request_id = refresh_request_id;
    app.mark_refreshing();
    spawn_service_refresh(
        refresh_tx.clone(),
        params.clone(),
        refresh_request_id,
        app.time_range_idx,
        ServiceRefreshOptions::from_app(&app),
    );

    let mut poll_interval =
        tokio::time::interval(std::time::Duration::from_secs(app.poll_interval_secs()));
    poll_interval.tick().await; // consume first instant tick
    let mut render_interval = tokio::time::interval(FRAME_INTERVAL);
    render_interval.tick().await;
    let mut dirty = true;

    'main: loop {
        // Check if background db_stats fetch completed (non-blocking)
        if app.poll_db_stats(&params) {
            dirty = true;
        }
        app.maybe_start_db_stats_fetch(&params);

        tokio::select! {
            biased;
            _ = render_interval.tick(), if dirty || app.refreshing => {
                terminal.draw(|f| ui::render_service(&app, f))?;
                dirty = false;
            }
            Some(result) = refresh_rx.recv() => {
                if result.request_id == active_refresh_request_id {
                    app.apply_refresh_result(result);
                    dirty = true;
                }
            }
            Some(Ok(event)) = events.next() => {
                match event {
                    Event::Key(key) => {
                        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                            continue;
                        }
                        if app.handle_key(key) {
                            break 'main;
                        }
                        if app.time_range_changed {
                            app.time_range_changed = false;
                            poll_interval = tokio::time::interval(
                                std::time::Duration::from_secs(app.poll_interval_secs()),
                            );
                            poll_interval.tick().await;
                            app.force_refresh = true;
                        }
                        if app.force_refresh {
                            app.force_refresh = false;
                            refresh_request_id += 1;
                            active_refresh_request_id = refresh_request_id;
                            app.mark_refreshing();
                            spawn_service_refresh(
                                refresh_tx.clone(),
                                params.clone(),
                                refresh_request_id,
                                app.time_range_idx,
                                ServiceRefreshOptions::from_app(&app),
                            );
                        }
                        dirty = true;
                    }
                    Event::Mouse(mouse) => {
                        app.handle_mouse(mouse);
                        dirty = true;
                    }
                    Event::Resize(_, _) => {
                        let _ = terminal.clear();
                        dirty = true;
                    }
                    _ => {}
                }
            }
            _ = poll_interval.tick() => {
                refresh_request_id += 1;
                active_refresh_request_id = refresh_request_id;
                app.mark_refreshing();
                spawn_service_refresh(
                    refresh_tx.clone(),
                    params.clone(),
                    refresh_request_id,
                    app.time_range_idx,
                    ServiceRefreshOptions::from_app(&app),
                );
                dirty = true;
            }
            _ = tokio::signal::ctrl_c() => {
                break 'main;
            }
        }
    }

    Ok(())
}

/// Run project-wide metrics TUI (--all)
pub async fn run_project(params: ProjectTuiParams) -> Result<()> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    let mut terminal = setup_terminal()?;
    let _cleanup = scopeguard::guard((), |_| {
        restore_terminal();
    });

    let mut app = ProjectApp::new(&params);
    let mut events = EventStream::new();
    let (fetch_tx, mut fetch_rx) = mpsc::unbounded_channel();
    let mut refresh_request_id = 0u64;
    let mut active_refresh_request_id: u64;
    let mut detail_request_id = 0u64;
    let mut active_detail_request_id = 0u64;

    refresh_request_id += 1;
    active_refresh_request_id = refresh_request_id;
    app.mark_refreshing();
    spawn_project_refresh(
        fetch_tx.clone(),
        params.clone(),
        refresh_request_id,
        app.time_range_idx,
        ProjectRefreshOptions::from_app(&app),
    );

    let mut poll_interval =
        tokio::time::interval(std::time::Duration::from_secs(app.poll_interval_secs()));
    poll_interval.tick().await;
    let mut render_interval = tokio::time::interval(FRAME_INTERVAL);
    render_interval.tick().await;

    let mut prev_selected_idx = app.selected_idx;
    let mut dirty = true;

    'main: loop {
        // Adjust scroll before drawing
        let term_height = terminal.size()?.height;
        let max_table_body = (term_height / 3).max(3);
        let table_body_rows = (app.services.len() as u16).min(max_table_body).max(1);
        app.ensure_selection_visible(table_body_rows as usize);

        tokio::select! {
            biased;
            _ = render_interval.tick(), if dirty || app.refreshing || app.detail_loading => {
                terminal.draw(|f| ui::render_project(&app, f))?;
                dirty = false;
            }
            Some(msg) = fetch_rx.recv() => {
                match msg {
                    ProjectFetchMsg::Refresh(result) => {
                        if result.request_id == active_refresh_request_id {
                            let applied_services = app.apply_refresh_result(result);
                            if applied_services {
                                prev_selected_idx = app.selected_idx;
                                let http_jobs = app.http_summary_jobs();
                                if !http_jobs.is_empty() {
                                    spawn_project_http_summaries(
                                        fetch_tx.clone(),
                                        params.clone(),
                                        active_refresh_request_id,
                                        app.time_range_idx,
                                        http_jobs,
                                    );
                                }
                                if app.needs_detail_fetch() {
                                    detail_request_id += 1;
                                    active_detail_request_id = detail_request_id;
                                    if let Some(request) = app.selected_detail_request(detail_request_id) {
                                        app.mark_detail_loading(request.service_id.clone());
                                        spawn_project_detail(fetch_tx.clone(), params.clone(), request);
                                    }
                                }
                            }
                            dirty = true;
                        }
                    }
                    ProjectFetchMsg::Detail(result) => {
                        if result.request_id == active_detail_request_id {
                            app.apply_detail_result(result);
                            dirty = true;
                        }
                    }
                    ProjectFetchMsg::Http(result) => {
                        if result.request_id == active_refresh_request_id {
                            app.apply_http_result(result);
                            dirty = true;
                        }
                    }
                }
            }
            Some(Ok(event)) = events.next() => {
                match event {
                    Event::Key(key) => {
                        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                            continue;
                        }
                        if app.handle_key(key) {
                            break 'main;
                        }

                        // Fetch detail when selection changes
                        if app.selected_idx != prev_selected_idx {
                            prev_selected_idx = app.selected_idx;
                            if app.needs_detail_fetch() {
                                detail_request_id += 1;
                                active_detail_request_id = detail_request_id;
                                if let Some(request) = app.selected_detail_request(detail_request_id) {
                                    app.mark_detail_loading(request.service_id.clone());
                                    spawn_project_detail(fetch_tx.clone(), params.clone(), request);
                                }
                            }
                        }

                        if app.time_range_changed {
                            app.time_range_changed = false;
                            poll_interval = tokio::time::interval(
                                std::time::Duration::from_secs(app.poll_interval_secs()),
                            );
                            poll_interval.tick().await;
                            app.force_refresh = true;
                        }
                        if app.force_refresh {
                            app.force_refresh = false;
                            refresh_request_id += 1;
                            active_refresh_request_id = refresh_request_id;
                            app.mark_refreshing();
                            spawn_project_refresh(
                                fetch_tx.clone(),
                                params.clone(),
                                refresh_request_id,
                                app.time_range_idx,
                                ProjectRefreshOptions::from_app(&app),
                            );
                        }
                        dirty = true;
                    }
                    Event::Resize(_, _) => {
                        let _ = terminal.clear();
                        dirty = true;
                    }
                    _ => {}
                }
            }
            _ = poll_interval.tick() => {
                refresh_request_id += 1;
                active_refresh_request_id = refresh_request_id;
                app.mark_refreshing();
                spawn_project_refresh(
                    fetch_tx.clone(),
                    params.clone(),
                    refresh_request_id,
                    app.time_range_idx,
                    ProjectRefreshOptions::from_app(&app),
                );
                dirty = true;
            }
            _ = tokio::signal::ctrl_c() => {
                break 'main;
            }
        }
    }

    Ok(())
}
