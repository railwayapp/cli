use anyhow::Result;
use crossterm::terminal;
use std::io::Write;
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
    let mut needs_init = false;

    let (mut sigint, mut sigterm, mut sigwinch) = setup_signal_handlers().await?;

    // Main event loop
    loop {
        // If reconnection happened and needs re-initialization, do it first
        if needs_init {
            if let Err(e) = client.init_shell(None).await {
                eprintln!("Failed to re-initialize shell: {}", e);
                exit_code = Some(1);
                break;
            }
            needs_init = false;

            // Reset terminal state
            // Clear line and move cursor to beginning of line
            print!("\r\x1B[K");
            std::io::stdout().flush()?;

            // After re-initialization, send window size only if shell is ready
            if client.is_ready() {
                if let Ok((cols, rows)) = terminal::size() {
                    if let Err(e) = client.send_window_size(cols, rows).await {
                        if !e.to_string().contains("Shell not ready yet") {
                            eprintln!("Failed to update window size: {}", e);
                        }
                    }
                }
            }
        }

        // Check if shell is ready for input
        let is_ready = client.is_ready();

        select! {
            // Handle window resizes
            _ = sigwinch.recv() => {
                if let Ok((cols, rows)) = terminal::size() {
                    if is_ready {
                        match client.send_window_size(cols, rows).await {
                            Ok(_) => {},
                            Err(e) => {
                                if e.to_string().contains("reconnected but needs re-initialization") {
                                    needs_init = true;
                                } else if !e.to_string().contains("Shell not ready yet") {
                                    eprintln!("Failed to update window size: {}", e);
                                }
                            }
                        }
                    }
                }
                continue;
            }
            // Handle signals
            _ = sigint.recv() => {
                if is_ready {
                    match client.send_signal(2).await { // SIGINT
                        Ok(_) => {},
                        Err(e) => {
                            if e.to_string().contains("reconnected but needs re-initialization") {
                                needs_init = true;
                            } else if !e.to_string().contains("Shell not ready yet") {
                                eprintln!("Failed to send SIGINT: {}", e);
                            }
                        }
                    }
                }
                continue;
            }
            _ = sigterm.recv() => {
                if is_ready {
                    match client.send_signal(15).await { // SIGTERM
                        Ok(_) => {},
                        Err(e) => {
                            if !e.to_string().contains("reconnected but needs re-initialization")
                               && !e.to_string().contains("Shell not ready yet") {
                                eprintln!("Failed to send SIGTERM: {}", e);
                            }
                        }
                    }
                }
                break;
            }
            // Handle input from terminal only if shell is ready
            result = stdin.read(&mut stdin_buf), if is_ready => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&stdin_buf[..n]);
                        match client.send_data(&data).await {
                            Ok(_) => {},
                            Err(e) => {
                                if e.to_string().contains("reconnected but needs re-initialization") {
                                    needs_init = true;
                                } else if !e.to_string().contains("Shell not ready yet") {
                                    eprintln!("Error sending data: {}", e);
                                    exit_code = Some(1);
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Error reading from stdin: {}", e);
                        exit_code = Some(1);
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
                        if e.to_string().contains("reconnected but needs re-initialization") {
                            needs_init = true;
                            continue;
                        } else {
                            eprintln!("Error: {}", e);
                            exit_code = Some(1);
                            break;
                        }
                    }
                }
            }
        }
    }

    // Clean up terminal when done
    let _ = terminal::disable_raw_mode();

    // Ensure cursor is visible with ANSI escape sequence
    print!("\x1b[?25h");
    std::io::stdout().flush()?;

    if let Some(code) = exit_code {
        std::process::exit(code);
    }

    Ok(())
}
