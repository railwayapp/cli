use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use futures_util::future::FutureExt;
use std::io::Write;
use tokio::io::AsyncReadExt;
use tokio::select;
use tokio::sync::mpsc;
use tokio::time::Duration;

use crate::commands::ssh::common::{SessionTermination, parse_server_error, reset_terminal};
use crate::controllers::terminal::TerminalClient;

// stub function because Windows does not support signals
pub async fn setup_signal_handlers() -> Result<()> {
    Ok(())
}

// Messages that can be sent to the UI task
enum UiMessage {
    // Server message handler has completed with termination status
    ServerDone(SessionTermination),
    // Shell is ready for input (combines ready and not in command progress)
    ReadyForInput(bool),
    // Shell is ready (needed for signals and window resizing)
    ShellReady(bool),
}

// Messages that can be sent to the server task
enum ServerMessage {
    // Send data to the server
    SendData(String),
    // Send a signal to the server
    SendSignal(u8),
    // Resize the terminal window
    WindowResize(u16, u16),
}

/// Windows-specific event handling for the SSH session
pub async fn run_interactive_session(client: TerminalClient) -> Result<SessionTermination> {
    #[cfg(feature = "event-stream")]
    let result = run_with_event_stream(client).await;

    #[cfg(not(feature = "event-stream"))]
    let result = run_with_polling(client).await;

    // Clean up terminal
    reset_terminal(false)?;

    result
}

#[cfg(feature = "event-stream")]
async fn run_with_event_stream(mut client: TerminalClient) -> Result<SessionTermination> {
    let mut stdin = tokio::io::stdin();
    let mut stdin_buf = [0u8; 1024];
    let mut termination = None;
    let mut event_stream = crossterm::event::EventStream::new();

    let (ui_tx, mut ui_rx) = mpsc::channel::<UiMessage>(100);
    let (server_tx, mut server_rx) = mpsc::channel::<ServerMessage>(100);

    let mut shell_ready = client.is_ready();
    let mut ready_for_input = client.is_ready_for_input();

    tokio::spawn(async move {
        loop {
            select! {
                // Handle incoming messages from the UI task
                Some(msg) = server_rx.recv() => {
                    match msg {
                        ServerMessage::SendData(data) => {
                            if let Err(e) = client.send_data(&data).await {
                                eprintln!("Error sending data: {}", e);
                                let _ = ui_tx.send(UiMessage::ServerDone(
                                    SessionTermination::ServerError(e.to_string())
                                )).await;
                                break;
                            }
                        },
                        ServerMessage::SendSignal(signal) => {
                            if let Err(e) = client.send_signal(signal).await {
                                eprintln!("Error sending signal: {}", e);
                            }
                        },
                        ServerMessage::WindowResize(cols, rows) => {
                            if let Err(e) = client.send_window_size(cols, rows).await {
                                eprintln!("Error resizing window: {}", e);
                            }
                        }
                    }
                }

                // Process messages from the server
                result = client.handle_server_messages() => {
                    match result {
                        Ok(()) => {
                            let _ = ui_tx.send(UiMessage::ServerDone(
                                SessionTermination::Complete
                            )).await;
                        },
                        Err(e) => {
                            eprintln!("Error in server messages: {}", e);
                            let _ = ui_tx.send(UiMessage::ServerDone(
                                SessionTermination::ServerError(e.to_string())
                            )).await;
                        }
                    }
                    break;
                }
            }

            if shell_ready != client.is_ready() {
                shell_ready = client.is_ready();
                let _ = ui_tx.send(UiMessage::ShellReady(shell_ready)).await;
            }

            if ready_for_input != client.is_ready_for_input() {
                ready_for_input = client.is_ready_for_input();
                let _ = ui_tx.send(UiMessage::ReadyForInput(ready_for_input)).await;
            }
        }
    });

    // Main event loop for input and events
    loop {
        select! {
            // Handle crossterm events for Windows with event-stream
            maybe_event = event_stream.next().fuse() => {
                if let Some(Ok(event)) = maybe_event {
                    match event {
                        Event::Key(KeyEvent { code: KeyCode::Char('c'), modifiers, .. }) if modifiers.contains(KeyModifiers::CONTROL) => {
                            // Handle Ctrl+C like SIGINT
                            if shell_ready {
                                let _ = server_tx.send(ServerMessage::SendSignal(2)).await;
                            }
                        },
                        Event::Resize(width, height) => {
                            // Handle terminal resize
                            if shell_ready {
                                let _ = server_tx.send(ServerMessage::WindowResize(width, height)).await;
                            }
                        },
                        Event::Key(key) => {
                            // Only handle key input if ready for input
                            if ready_for_input {
                                if let Some(input) = key_event_to_string(key) {
                                    let _ = server_tx.send(ServerMessage::SendData(input)).await;
                                }
                            }
                        },
                        _ => {}
                    }
                }
            }

            // Handle input from stdin (if using both stdin and events)
            result = stdin.read(&mut stdin_buf) => {
                if ready_for_input {
                    match result {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            let data = String::from_utf8_lossy(&stdin_buf[..n]).to_string();
                            let _ = server_tx.send(ServerMessage::SendData(data)).await;
                        }
                        Err(e) => {
                            eprintln!("Error reading from stdin: {}", e);
                            termination = Some(SessionTermination::StdinError(e.to_string()));
                            break;
                        }
                    }
                }
            }

            // Process messages from server task
            Some(msg) = ui_rx.recv() => {
                match msg {
                    UiMessage::ServerDone(term) => {
                        termination = Some(term);
                        break;
                    },
                    UiMessage::ShellReady(ready) => {
                        shell_ready = ready;
                    },
                    UiMessage::ReadyForInput(input_ready) => {
                        ready_for_input = input_ready;
                    }
                }
            }
        }
    }

    Ok(termination.unwrap_or(SessionTermination::Complete))
}

#[cfg(not(feature = "event-stream"))]
async fn run_with_polling(mut client: TerminalClient) -> Result<SessionTermination> {
    let mut stdin = tokio::io::stdin();
    let mut stdin_buf = [0u8; 1024];
    let mut termination = None;
    let event_poll_timeout = Duration::from_millis(100);

    let (ui_tx, mut ui_rx) = mpsc::channel::<UiMessage>(100);
    let (server_tx, mut server_rx) = mpsc::channel::<ServerMessage>(100);

    let mut shell_ready = client.is_ready();
    let mut ready_for_input = client.is_ready_for_input();

    tokio::spawn(async move {
        loop {
            select! {
                // Handle incoming messages from the UI task
                Some(msg) = server_rx.recv() => {
                    match msg {
                        ServerMessage::SendData(data) => {
                            if let Err(e) = client.send_data(&data).await {
                                eprintln!("Error sending data: {}", e);
                                let _ = ui_tx.send(UiMessage::ServerDone(
                                    SessionTermination::ServerError(e.to_string())
                                )).await;
                                break;
                            }
                        },
                        ServerMessage::SendSignal(signal) => {
                            if let Err(e) = client.send_signal(signal).await {
                                eprintln!("Error sending signal: {}", e);
                            }
                        },
                        ServerMessage::WindowResize(cols, rows) => {
                            if let Err(e) = client.send_window_size(cols, rows).await {
                                eprintln!("Error resizing window: {}", e);
                            }
                        }
                    }
                }

                // Process messages from the server
                result = client.handle_server_messages() => {
                    match result {
                        Ok(()) => {
                            let _ = ui_tx.send(UiMessage::ServerDone(
                                SessionTermination::Complete
                            )).await;
                        },
                        Err(e) => {
                            eprintln!("Error in server messages: {}", e);
                            let _ = ui_tx.send(UiMessage::ServerDone(
                                SessionTermination::ServerError(e.to_string())
                            )).await;
                        }
                    }
                    break;
                }
            }

            if shell_ready != client.is_ready() {
                shell_ready = client.is_ready();
                let _ = ui_tx.send(UiMessage::ShellReady(shell_ready)).await;
            }

            if ready_for_input != client.is_ready_for_input() {
                ready_for_input = client.is_ready_for_input();
                let _ = ui_tx.send(UiMessage::ReadyForInput(ready_for_input)).await;
            }
        }
    });

    // Main event loop for input and polling events
    loop {
        select! {
            // Handle input from stdin
            result = stdin.read(&mut stdin_buf) => {
                if ready_for_input {
                    match result {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            let data = String::from_utf8_lossy(&stdin_buf[..n]).to_string();
                            let _ = server_tx.send(ServerMessage::SendData(data)).await;
                        }
                        Err(e) => {
                            eprintln!("Error reading from stdin: {}", e);
                            termination = Some(SessionTermination::StdinError(e.to_string()));
                            break;
                        }
                    }
                }
            }

            // Process messages from server task
            Some(msg) = ui_rx.recv() => {
                match msg {
                    UiMessage::ServerDone(term) => {
                        termination = Some(term);
                        break;
                    },
                    UiMessage::ShellReady(ready) => {
                        shell_ready = ready;
                    },
                    UiMessage::ReadyForInput(input_ready) => {
                        ready_for_input = input_ready;
                    }
                }
            }

            // Poll for crossterm events
            _ = tokio::time::sleep(event_poll_timeout) => {
                if event::poll(Duration::from_millis(0))? {
                    match event::read()? {
                        Event::Key(KeyEvent { code: KeyCode::Char('c'), modifiers, .. }) if modifiers.contains(KeyModifiers::CONTROL) => {
                            // Handle Ctrl+C like SIGINT
                            if shell_ready {
                                let _ = server_tx.send(ServerMessage::SendSignal(2)).await;
                            }
                        },
                        Event::Resize(width, height) => {
                            // Handle terminal resize
                            if shell_ready {
                                let _ = server_tx.send(ServerMessage::WindowResize(width, height)).await;
                            }
                        },
                        Event::Key(key) => {
                            // Only handle key input if ready for input
                            if ready_for_input {
                                if let Some(input) = key_event_to_string(key) {
                                    let _ = server_tx.send(ServerMessage::SendData(input)).await;
                                }
                            }
                        },
                        _ => {}
                    }
                }
            }
        }
    }

    Ok(termination.unwrap_or(SessionTermination::Complete))
}

// Helper function to convert key events to strings
fn key_event_to_string(key: KeyEvent) -> Option<String> {
    match key.code {
        KeyCode::Char(c) => Some(c.to_string()),
        KeyCode::Enter => Some("\r".to_string()),
        KeyCode::Backspace => Some("\x08".to_string()),
        KeyCode::Esc => Some("\x1b".to_string()),
        _ => None,
    }
}
