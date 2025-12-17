mod app;
mod docker_logs;
mod log_store;
mod ui;

pub use app::ServiceInfo;
pub use docker_logs::{ServiceMapping, spawn_docker_logs};

use anyhow::Result;
use app::TuiApp;
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use super::LogLine;

pub async fn run(
    mut log_rx: mpsc::Receiver<LogLine>,
    mut docker_rx: mpsc::Receiver<LogLine>,
    services: Vec<ServiceInfo>,
) -> Result<()> {
    let mut terminal = ratatui::init();
    let _cleanup = scopeguard::guard((), |_| {
        ratatui::restore();
    });

    let mut app = TuiApp::new(services);
    let mut events = EventStream::new();

    loop {
        terminal.draw(|f| ui::render(&app, f))?;

        tokio::select! {
            Some(log) = log_rx.recv() => {
                app.push_log(log, false);
            }
            Some(log) = docker_rx.recv() => {
                app.push_log(log, true);
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
