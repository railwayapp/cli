use anyhow::Result;
use crossterm::terminal;
use futures_util::FutureExt;
use tokio::io::AsyncReadExt;
use tokio::select;

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

/// Run the interactive SSH session with Unix-specific event handling
pub async fn run_interactive_session(client: &mut TerminalClient) -> Result<()> {
    let mut stdin = tokio::io::stdin();
    let mut stdin_buf = [0u8; 1024];
    let mut exit_code = None;

    let (mut sigint, mut sigterm, mut sigwinch) = setup_signal_handlers().await?;

    // Main event loop
    loop {
        select! {
            // Handle window resizes
            _ = sigwinch.recv() => {
                if let Ok((cols, rows)) = terminal::size() {
                    client.send_window_size(cols, rows).await?;
                }
                continue;
            }
            // Handle signals
            _ = sigint.recv() => {
                client.send_signal(2).await?; // SIGINT
                continue;
            }
            _ = sigterm.recv() => {
                client.send_signal(15).await?; // SIGTERM
                break;
            }
            // Handle input from terminal
            result = stdin.read(&mut stdin_buf) => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&stdin_buf[..n]);
                        client.send_data(&data).await?;
                    }
                    Err(e) => {
                        eprintln!("Error reading from stdin: {}", e);
                        break;
                    }
                }
            }
            // Handle messages from server
            result = client.handle_server_messages() => {
                match result {
                   Ok(()) => {
                        exit_code = Some(0);
                        break;
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        exit_code = Some(1);
                        break;
                    }
                }
            }
        }
    }

    // Clean up terminal when done
    let _ = terminal::disable_raw_mode();

    if let Some(code) = exit_code {
        std::process::exit(code);
    }

    Ok(())
}
