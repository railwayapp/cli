mod app;
mod ui;

use std::io::stdout;
use std::panic;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use app::{MetricsApp, ProjectApp};

use crate::commands::metrics::Sections;
use crate::controllers::database::DatabaseType;
use crate::queries::project::ProjectProject;

/// Parameters for single-service metrics TUI
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

pub(crate) fn normalize_time_range_label(since: &str) -> Option<String> {
    let normalized = since.trim().to_ascii_lowercase();
    TIME_RANGES
        .contains(&normalized.as_str())
        .then_some(normalized)
}

pub(crate) fn supported_time_ranges_label() -> String {
    TIME_RANGES.join(", ")
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

    // Initial fetch
    app.refresh(&params).await;

    let mut poll_interval =
        tokio::time::interval(std::time::Duration::from_secs(app.poll_interval_secs()));
    poll_interval.tick().await; // consume first instant tick

    'main: loop {
        // Check if background db_stats fetch completed (non-blocking)
        app.poll_db_stats(&params);
        app.maybe_start_db_stats_fetch(&params);

        terminal.draw(|f| ui::render_service(&app, f))?;

        tokio::select! {
            biased;
            Some(Ok(event)) = events.next() => {
                match event {
                    Event::Key(key) => {
                        if app.handle_key(key) {
                            break 'main;
                        }
                        if app.force_refresh {
                            app.force_refresh = false;
                            app.refresh(&params).await;
                        }
                        if app.time_range_changed {
                            app.time_range_changed = false;
                            poll_interval = tokio::time::interval(
                                std::time::Duration::from_secs(app.poll_interval_secs()),
                            );
                            poll_interval.tick().await;
                            app.refresh(&params).await;
                        }
                    }
                    Event::Mouse(mouse) => {
                        app.handle_mouse(mouse);
                    }
                    Event::Resize(_, _) => {
                        let _ = terminal.clear();
                    }
                    _ => {}
                }
            }
            _ = poll_interval.tick() => {
                app.refresh(&params).await;
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

    app.refresh(&params).await;

    let mut poll_interval =
        tokio::time::interval(std::time::Duration::from_secs(app.poll_interval_secs()));
    poll_interval.tick().await;

    let mut prev_selected_idx = app.selected_idx;

    'main: loop {
        // Adjust scroll before drawing
        let term_height = terminal.size()?.height;
        let max_table_body = (term_height / 3).max(3);
        let table_body_rows = (app.services.len() as u16).min(max_table_body).max(1);
        app.ensure_selection_visible(table_body_rows as usize);

        terminal.draw(|f| ui::render_project(&app, f))?;

        tokio::select! {
            biased;
            Some(Ok(event)) = events.next() => {
                match event {
                    Event::Key(key) => {
                        if app.handle_key(key) {
                            break 'main;
                        }

                        // Fetch detail when selection changes
                        if app.selected_idx != prev_selected_idx {
                            prev_selected_idx = app.selected_idx;
                            if app.needs_detail_fetch() {
                                app.refresh_selected_detail(&params).await;
                            }
                        }

                        if app.force_refresh {
                            app.force_refresh = false;
                            app.refresh(&params).await;
                        }
                        if app.time_range_changed {
                            app.time_range_changed = false;
                            poll_interval = tokio::time::interval(
                                std::time::Duration::from_secs(app.poll_interval_secs()),
                            );
                            poll_interval.tick().await;
                            app.refresh(&params).await;
                        }
                    }
                    Event::Resize(_, _) => {
                        let _ = terminal.clear();
                    }
                    _ => {}
                }
            }
            _ = poll_interval.tick() => {
                app.refresh(&params).await;
            }
            _ = tokio::signal::ctrl_c() => {
                break 'main;
            }
        }
    }

    Ok(())
}
