mod app;
mod docker_logs;
mod log_store;
mod ui;

pub use app::ServiceInfo;
pub use docker_logs::{ServiceMapping, spawn_docker_logs};

use std::io::stdout;

use anyhow::Result;
use app::TuiApp;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream};
use crossterm::execute;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use super::LogLine;

pub async fn run(
    mut log_rx: mpsc::Receiver<LogLine>,
    mut docker_rx: mpsc::Receiver<LogLine>,
    services: Vec<ServiceInfo>,
) -> Result<()> {
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableMouseCapture)?;

    let _cleanup = scopeguard::guard((), |_| {
        let _ = execute!(stdout(), DisableMouseCapture);
        ratatui::restore();
    });

    let mut app = TuiApp::new(services);
    let mut events = EventStream::new();

    loop {
        terminal.draw(|f| ui::render(&mut app, f))?;

        tokio::select! {
            Some(log) = log_rx.recv() => {
                app.push_log(log, false);
            }
            Some(log) = docker_rx.recv() => {
                app.push_log(log, true);
            }
            Some(Ok(event)) = events.next() => {
                match event {
                    Event::Key(key) => {
                        if app.handle_key(key) {
                            break;
                        }
                    }
                    Event::Mouse(mouse) => {
                        app.handle_mouse(mouse);
                    }
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }

    Ok(())
}
