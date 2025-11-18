use anyhow::Result;
use crossterm::terminal;
use tokio::io::AsyncReadExt;
use tokio::select;
use tokio::sync::mpsc;

use crate::commands::ssh::common::{SessionTermination, parse_server_error, reset_terminal};
use crate::controllers::terminal::TerminalClient;

/// Set up Unix-specific signal handlers
pub async fn setup_signal_handlers() -> Result<(
    tokio::signal::unix::Signal,
    tokio::signal::unix::Signal,
    tokio::signal::unix::Signal,
)> {
    let sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let sigwinch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;

    Ok((sigint, sigterm, sigwinch))
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

/// Run the interactive SSH session with Unix-specific event handling
pub async fn run_interactive_session(mut client: TerminalClient) -> Result<SessionTermination> {
    let mut stdin = tokio::io::stdin();
    let mut stdin_buf = [0u8; 1024];
    let mut termination = None;

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
                                eprintln!("Error sending data: {e}");

                                let _ = ui_tx.send(UiMessage::ServerDone(
                                    SessionTermination::SendError(e.to_string())
                                )).await;

                                break;
                            }
                        },
                        ServerMessage::SendSignal(signal) => {
                            if let Err(e) = client.send_signal(signal).await {
                                eprintln!("Error sending signal: {e}");
                            }
                        },
                        ServerMessage::WindowResize(cols, rows) => {
                            if let Err(e) = client.send_window_size(cols, rows).await {
                                eprintln!("Error resizing window: {e}");
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
                            let _ = ui_tx.send(UiMessage::ServerDone(
                                parse_server_error(e.to_string())
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

    let (mut sigint, mut sigterm, mut sigwinch) = setup_signal_handlers().await?;

    // Main event loop for input and signals
    loop {
        select! {
            // Handle window resizes
            _ = sigwinch.recv() => {
                if let Ok((cols, rows)) = terminal::size() {
                   if shell_ready {
                        let _ = server_tx.send(ServerMessage::WindowResize(cols, rows)).await;
                   }
                }
                continue;
            }

            // Handle signals
            _ = sigint.recv() => {
                if shell_ready {
                    let _ = server_tx.send(ServerMessage::SendSignal(2)).await; // SIGINT
                }
                continue;
            }

            _ = sigterm.recv() => {
                if shell_ready {
                    let _ = server_tx.send(ServerMessage::SendSignal(15)).await; // SIGTERM
                }
                break;
            }

            // Handle input from terminal - only send if ready for input
            result = stdin.read(&mut stdin_buf) => {
                if ready_for_input {
                    match result {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            let data = String::from_utf8_lossy(&stdin_buf[..n]).to_string();
                            let _ = server_tx.send(ServerMessage::SendData(data)).await;
                        }
                        Err(e) => {
                            eprintln!("Error reading from stdin: {e}");
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

    // Clean up terminal when done
    reset_terminal(false)?;

    Ok(termination.unwrap_or(SessionTermination::Complete))
}
