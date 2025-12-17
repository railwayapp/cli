mod app;
mod docker_logs;
mod log_store;
mod ui;

pub use app::ServiceInfo;
pub use docker_logs::{ServiceMapping, spawn_docker_logs};

use std::io::stdout;
use std::time::Duration;

use anyhow::Result;
use app::TuiApp;
use crossterm::cursor::Show;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, MouseEventKind,
};
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
        ratatui::restore();
        let _ = execute!(stdout(), DisableMouseCapture, Show);
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
                if process_event(&mut app, event) {
                    break;
                }
                // Drain any queued events to batch scroll and prevent momentum lag
                while let Ok(Some(Ok(event))) =
                    tokio::time::timeout(Duration::from_millis(1), events.next()).await
                {
                    if process_event(&mut app, event) {
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

fn process_event(app: &mut TuiApp, event: Event) -> bool {
    match event {
        Event::Key(key) => {
            if app.handle_key(key) {
                return true;
            }
        }
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                app.handle_mouse(mouse);
            }
            _ => {}
        },
        _ => {}
    }
    false
}
