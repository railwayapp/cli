mod app;
mod docker_logs;
mod log_store;
mod ui;

pub use app::ServiceInfo;
pub use docker_logs::{ServiceMapping, spawn_docker_logs};

use std::io::stdout;
use std::panic;
use std::time::Duration;

use anyhow::Result;
use app::TuiApp;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use super::LogLine;

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    execute!(stdout(), EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal() {
    let _ = execute!(stdout(), DisableMouseCapture);
    let _ = execute!(stdout(), LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
    // Ensure cursor starts at column 0 on a fresh line
    print!("\r\n");
    let _ = std::io::Write::flush(&mut stdout());
}

pub async fn run(
    mut log_rx: mpsc::Receiver<LogLine>,
    mut docker_rx: mpsc::Receiver<LogLine>,
    services: Vec<ServiceInfo>,
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
