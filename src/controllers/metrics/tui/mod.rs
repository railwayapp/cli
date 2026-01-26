mod app;
mod ui;

use std::io::stdout;
use std::panic;

use anyhow::Result;
use app::MetricsApp;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use reqwest::Client;
use tokio::time::{Duration, interval};

use crate::commands::Configs;
use crate::gql::queries::project::ProjectProject;

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal() {
    let _ = execute!(stdout(), LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}

pub async fn run(
    client: &Client,
    configs: &Configs,
    project_id: &str,
    environment_id: &str,
    service_id: Option<&str>,
    time_range: &str,
    project: &ProjectProject,
) -> Result<()> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    let mut terminal = setup_terminal()?;

    let _cleanup = scopeguard::guard((), |_| {
        restore_terminal();
    });

    let services: Vec<(String, String)> = project
        .services
        .edges
        .iter()
        .map(|s| (s.node.id.clone(), s.node.name.clone()))
        .collect();

    let mut app = MetricsApp::new(services, time_range.to_string());
    let mut events = EventStream::new();
    let mut refresh_interval = interval(Duration::from_secs(5));

    let start_date = super::parse_time_range(time_range)?;
    let metrics = super::fetch_metrics(
        client,
        configs,
        project_id,
        environment_id,
        service_id,
        start_date,
    )
    .await?;
    app.update_metrics(metrics);

    loop {
        terminal.draw(|f| ui::render(&mut app, f))?;

        tokio::select! {
            _ = refresh_interval.tick() => {
                let start_date = super::parse_time_range(time_range)?;
                if let Ok(metrics) = super::fetch_metrics(
                    client,
                    configs,
                    project_id,
                    environment_id,
                    service_id,
                    start_date,
                ).await {
                    app.update_metrics(metrics);
                }
            }
            Some(Ok(event)) = events.next() => {
                if let Event::Key(key) = event {
                    if app.handle_key(key) {
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }

    Ok(())
}
