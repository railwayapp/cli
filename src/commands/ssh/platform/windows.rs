use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use futures_util::stream::StreamExt;
use std::io::Write;
use tokio::io::AsyncReadExt;
use tokio::select;
use tokio::time::Duration;

use crate::controllers::terminal::TerminalClient;

// stub function because Windows does not support signals
pub async fn setup_signal_handlers() -> Result<()> {
    Ok(())
}

// Windows-specific event handling for the SSH session
pub async fn run_interactive_session(client: &mut TerminalClient) -> Result<()> {
    let mut stdin = tokio::io::stdin();
    let mut stdin_buf = [0u8; 1024];
    let mut exit_code = None;
    let mut needs_init = false;

    let _ = setup_signal_handlers().await?;

    // Event handling differs based on available features
    #[cfg(feature = "event-stream")]
    let run_result = run_with_event_stream(
        client,
        &mut stdin,
        &mut stdin_buf,
        &mut needs_init,
        &mut exit_code,
    )
    .await;

    #[cfg(not(feature = "event-stream"))]
    let run_result = run_with_polling(
        client,
        &mut stdin,
        &mut stdin_buf,
        &mut needs_init,
        &mut exit_code,
    )
    .await;

    // Clean up terminal
    let _ = terminal::disable_raw_mode();

    // Ensure cursor is visible with ANSI escape sequence
    print!("\x1b[?25h");
    std::io::stdout().flush()?;

    if let Some(code) = exit_code {
        std::process::exit(code);
    }

    run_result
}

#[cfg(feature = "event-stream")]
async fn run_with_event_stream(
    client: &mut TerminalClient,
    stdin: &mut tokio::io::Stdin,
    stdin_buf: &mut [u8; 1024],
    needs_init: &mut bool,
    exit_code: &mut Option<i32>,
) -> Result<()> {
    let mut event_stream = crossterm::event::EventStream::new();

    loop {
        // If reconnection happened and needs re-initialization, do it first
        if *needs_init {
            if let Err(e) = client.init_shell(None).await {
                eprintln!("Failed to re-initialize shell: {}", e);
                *exit_code = Some(1);
                break;
            }
            *needs_init = false;

            // Reset terminal state
            // Clear line and move cursor to beginning of line
            print!("\r\x1B[K");
            std::io::stdout().flush()?;

            // After successful initialization and ready, send window size
            if let Ok((cols, rows)) = terminal::size() {
                if let Err(e) = client.send_window_size(cols, rows).await {
                    if !e.to_string().contains("Shell not ready yet") {
                        eprintln!("Failed to send window size: {}", e);
                    }
                }
            }
        }

        // Check if the shell is ready for input
        let is_ready = client.is_ready();

        select! {
            // Handle crossterm events for Windows with event-stream
            maybe_event = event_stream.next().fuse(), if is_ready => {
                match maybe_event {
                    Some(Ok(Event::Key(KeyEvent { code: KeyCode::Char('c'), modifiers, .. }))) if modifiers.contains(KeyModifiers::CONTROL) => {
                        // Handle Ctrl+C like SIGINT
                        match client.send_signal(2).await {
                            Ok(_) => {},
                            Err(e) => {
                                if e.to_string().contains("reconnected but needs re-initialization") {
                                    *needs_init = true;
                                } else if !e.to_string().contains("Shell not ready yet") {
                                    return Err(e);
                                }
                            }
                        }
                        continue;
                    },
                    Some(Ok(Event::Resize(width, height))) => {
                        // Handle terminal resize
                        match client.send_window_size(width, height).await {
                            Ok(_) => {},
                            Err(e) => {
                                if e.to_string().contains("reconnected but needs re-initialization") {
                                    *needs_init = true;
                                } else if !e.to_string().contains("Shell not ready yet") {
                                    return Err(e);
                                }
                            }
                        }
                        continue;
                    },
                    Some(Ok(Event::Key(key))) => {
                        // Handle key input
                        if let Some(input) = key_event_to_string(key) {
                            match client.send_data(&input).await {
                                Ok(_) => {},
                                Err(e) => {
                                    if e.to_string().contains("reconnected but needs re-initialization") {
                                        *needs_init = true;
                                    } else if !e.to_string().contains("Shell not ready yet") {
                                        return Err(e);
                                    }
                                }
                            }
                        }
                    },
                    Some(Err(e)) => {
                        eprintln!("Error reading events: {}", e);
                        break;
                    },
                    None => break,
                }
            },

            result = stdin.read(stdin_buf), if is_ready => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&stdin_buf[..n]);
                        match client.send_data(&data).await {
                            Ok(_) => {},
                            Err(e) => {
                                if e.to_string().contains("reconnected but needs re-initialization") {
                                    *needs_init = true;
                                } else if !e.to_string().contains("Shell not ready yet") {
                                    return Err(e);
                                }
                            }
                        }
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
                        *exit_code = Some(0);
                        break;
                    }
                    Err(e) => {
                        if e.to_string().contains("reconnected but needs re-initialization") {
                            *needs_init = true;
                            continue;
                        } else {
                            eprintln!("Error: {}", e);
                            *exit_code = Some(1);
                            break;
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(not(feature = "event-stream"))]
async fn run_with_polling(
    client: &mut TerminalClient,
    stdin: &mut tokio::io::Stdin,
    stdin_buf: &mut [u8; 1024],
    needs_init: &mut bool,
    exit_code: &mut Option<i32>,
) -> Result<()> {
    let event_poll_timeout = Duration::from_millis(100);

    loop {
        // If reconnection happened and needs re-initialization, do it first
        if *needs_init {
            if let Err(e) = client.init_shell(None).await {
                eprintln!("Failed to re-initialize shell: {}", e);
                *exit_code = Some(1);
                break;
            }
            *needs_init = false;

            // Reset terminal state
            // Clear line and move cursor to beginning of line
            print!("\r\x1B[K");
            std::io::stdout().flush()?;

            // After successful initialization and ready, send window size
            if let Ok((cols, rows)) = terminal::size() {
                if let Err(e) = client.send_window_size(cols, rows).await {
                    if !e.to_string().contains("Shell not ready yet") {
                        eprintln!("Failed to send window size: {}", e);
                    }
                }
            }
        }

        // Check if the shell is ready for input
        let is_ready = client.is_ready();

        select! {
            result = stdin.read(stdin_buf), if is_ready => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&stdin_buf[..n]);
                        match client.send_data(&data).await {
                            Ok(_) => {},
                            Err(e) => {
                                if e.to_string().contains("reconnected but needs re-initialization") {
                                    *needs_init = true;
                                } else if !e.to_string().contains("Shell not ready yet") {
                                    return Err(e);
                                }
                            }
                        }
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
                        *exit_code = Some(0);
                        break;
                    }
                    Err(e) => {
                        if e.to_string().contains("reconnected but needs re-initialization") {
                            *needs_init = true;
                            continue;
                        } else {
                            eprintln!("Error: {}", e);
                            *exit_code = Some(1);
                            break;
                        }
                    }
                }
            }

            // Poll for crossterm events
            _ = tokio::time::sleep(event_poll_timeout) => {
                if is_ready && event::poll(Duration::from_millis(0))? {
                    match event::read()? {
                        Event::Key(KeyEvent { code: KeyCode::Char('c'), modifiers, .. }) if modifiers.contains(KeyModifiers::CONTROL) => {
                            // Handle Ctrl+C like SIGINT
                            match client.send_signal(2).await {
                                Ok(_) => {},
                                Err(e) => {
                                    if e.to_string().contains("reconnected but needs re-initialization") {
                                        *needs_init = true;
                                    } else if !e.to_string().contains("Shell not ready yet") {
                                        return Err(e);
                                    }
                                }
                            }
                        },
                        Event::Resize(width, height) => {
                            // Handle terminal resize
                            match client.send_window_size(width, height).await {
                                Ok(_) => {},
                                Err(e) => {
                                    if e.to_string().contains("reconnected but needs re-initialization") {
                                        *needs_init = true;
                                    } else if !e.to_string().contains("Shell not ready yet") {
                                        return Err(e);
                                    }
                                }
                            }
                        },
                        Event::Key(key) => {
                            // Handle key input
                            if let Some(input) = key_event_to_string(key) {
                                match client.send_data(&input).await {
                                    Ok(_) => {},
                                    Err(e) => {
                                        if e.to_string().contains("reconnected but needs re-initialization") {
                                            *needs_init = true;
                                        } else if !e.to_string().contains("Shell not ready yet") {
                                            return Err(e);
                                        }
                                    }
                                }
                            }
                        },
                        _ => {}
                    }
                }
            }
        }
    }

    Ok(())
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
