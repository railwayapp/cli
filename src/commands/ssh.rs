use anyhow::{Context, Result};
use crossterm::terminal::{self, disable_raw_mode, enable_raw_mode};
use reqwest::Client;
use tokio::io::AsyncReadExt;
use tokio::select;

use super::*;
use crate::{
    consts::TICK_STRING,
    controllers::{
        environment::get_matched_environment,
        project::get_project,
        service::get_or_prompt_service,
        terminal::{SSHConnectParams, TerminalClient},
    },
};

/// Connect to a service via SSH
#[derive(Parser)]
pub struct Args {
    /// Project to connect to (defaults to linked project)
    #[clap(short, long)]
    project: Option<String>,

    #[clap(short, long)]
    /// Service to connect to (defaults to linked service)
    service: Option<String>,

    #[clap(short, long)]
    /// Environment to connect to (defaults to linked environment)
    environment: Option<String>,

    #[clap(short, long)]
    /// Deployment instance ID to connect to (defaults to first active instance)
    #[arg(long = "deployment-instance", value_name = "deployment-instance-id")]
    deployment_instance: Option<String>,
}

async fn get_ssh_connect_params(
    args: Args,
    configs: &Configs,
    client: &Client,
) -> Result<SSHConnectParams> {
    let linked_project = configs.get_linked_project().await?;

    let project_id = args.project.unwrap_or(linked_project.project.clone());
    let project = get_project(client, configs, project_id.clone()).await?;

    let environment = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());
    let environment_id = get_matched_environment(&project, environment)?.id;

    let service_id = get_or_prompt_service(linked_project.clone(), project, args.service)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No service found. Please specify a service to connect to via the `--service` flag."))?;

    Ok(SSHConnectParams {
        project_id,
        environment_id,
        service_id,
        deployment_instance_id: args.deployment_instance,
    })
}

#[cfg(unix)]
async fn setup_signal_handlers() -> Result<(
    tokio::signal::unix::Signal,
    tokio::signal::unix::Signal,
    tokio::signal::unix::Signal,
)> {
    let sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let sigwinch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;

    Ok((sigint, sigterm, sigwinch))
}

#[cfg(not(unix))]
async fn setup_signal_handlers() -> Result<()> {
    // On Windows, we don't have these Unix signals
    Ok(())
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let params = get_ssh_connect_params(args, &configs, &client).await?;

    let token = configs
        .get_railway_auth_token()
        .context("No authentication token found. Please login first with 'railway login'")?;

    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")?,
        )
        .with_message("Connecting to service...");

    spinner.enable_steady_tick(std::time::Duration::from_millis(100));

    let ws_url = format!("wss://{}", configs.get_relay_host_path());

    let mut client = TerminalClient::new(&ws_url, &token, &params).await?;

    // Wait a moment for the connection to stabilize before sending terminal size
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let (cols, rows) = terminal::size()?;
    client.send_window_size(cols, rows).await?;

    let mut stdin = tokio::io::stdin();
    let mut stdin_buf = [0u8; 1024];

    enable_raw_mode()?;

    spinner.finish();

    // Send window size again after connection is fully established and spinner is finished
    if let Ok((cols, rows)) = terminal::size() {
        client.send_window_size(cols, rows).await?;
    }

    // Signal handling is platform-specific
    #[cfg(unix)]
    let (mut sigint, mut sigterm, mut sigwinch) = setup_signal_handlers().await?;

    #[cfg(not(unix))]
    let _ = setup_signal_handlers().await?;

    // For Windows, we'll use crossterm's event stream if available
    #[cfg(all(not(unix), feature = "event-stream"))]
    let mut event_stream = crossterm::event::EventStream::new();

    // Fallback to polling if event-stream is not available
    #[cfg(all(not(unix), not(feature = "event-stream")))]
    let event_poll_timeout = std::time::Duration::from_millis(100);

    let mut exit_code = None;

    // Main event loop
    loop {
        #[cfg(unix)]
        {
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

        #[cfg(not(unix))]
        {
            // We need to handle Windows event handling differently based on whether the
            // event-stream feature is enabled
            #[cfg(feature = "event-stream")]
            {
                select! {
                    // Handle crossterm events for Windows with event-stream
                    maybe_event = event_stream.next().fuse() => {
                        match maybe_event {
                            Some(Ok(Event::Key(KeyEvent { code: KeyCode::Char('c'), modifiers, .. }))) if modifiers.contains(KeyModifiers::CONTROL) => {
                                // Handle Ctrl+C like SIGINT
                                client.send_signal(2).await?;
                                continue;
                            },
                            Some(Ok(Event::Resize(width, height))) => {
                                // Handle terminal resize
                                client.send_window_size(width, height).await?;
                                continue;
                            },
                            Some(Ok(Event::Key(key))) => {
                                // Handle regular key input
                                // Convert the key event to a string and send it
                                let input = match key.code {
                                    KeyCode::Char(c) => c.to_string(),
                                    KeyCode::Enter => "\r".to_string(),
                                    KeyCode::Backspace => "\x08".to_string(),
                                    KeyCode::Esc => "\x1b".to_string(),
                                    _ => continue,
                                };
                                client.send_data(&input).await?;
                            },
                            Some(Err(e)) => {
                                eprintln!("Error reading events: {}", e);
                                break;
                            },
                            None => break,
                        }
                    },

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

            // Use polling-based approach when event-stream is not available
            #[cfg(not(feature = "event-stream"))]
            {
                select! {
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

                    // Poll for crossterm events
                    _ = tokio::time::sleep(event_poll_timeout) => {
                        if event::poll(std::time::Duration::from_millis(0))? {
                            match event::read()? {
                                Event::Key(KeyEvent { code: KeyCode::Char('c'), modifiers, .. }) if modifiers.contains(KeyModifiers::CONTROL) => {
                                    // Handle Ctrl+C like SIGINT
                                    client.send_signal(2).await?;
                                },
                                Event::Resize(width, height) => {
                                    // Handle terminal resize
                                    client.send_window_size(width, height).await?;
                                },
                                Event::Key(key) => {
                                    // Handle regular key input
                                    // Convert the key event to a string and send it
                                    let input = match key.code {
                                        KeyCode::Char(c) => c.to_string(),
                                        KeyCode::Enter => "\r".to_string(),
                                        KeyCode::Backspace => "\x08".to_string(),
                                        KeyCode::Esc => "\x1b".to_string(),
                                        _ => continue,
                                    };
                                    client.send_data(&input).await?;
                                },
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    let _ = disable_raw_mode();

    if let Some(code) = exit_code {
        std::process::exit(code);
    }

    Ok(())
}
