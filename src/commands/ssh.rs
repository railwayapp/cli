use anyhow::{Context, Result};
use is_terminal::IsTerminal;
use tokio::io::AsyncReadExt;
use tokio::select;

use super::*;
use crate::{consts::TICK_STRING, controllers::terminal::TerminalClient};

/// Connect to a service via SSH
#[derive(Parser)]
pub struct Args {
    /// Project to connect to
    #[arg(value_name = "project-name")]
    project: String,

    /// Service to connect to
    #[arg(value_name = "service-name")]
    service: String,

    /// Deployment instance ID to connect to
    #[arg(long = "deployment-instance", value_name = "deployment-instance-id")]
    deployment_instance: Option<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
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

    let mut client = TerminalClient::new(
        &ws_url,
        &token,
        &args.project,
        &args.service,
        args.deployment_instance.as_deref(),
    )
    .await?;

    if !std::io::stdout().is_terminal() {
        anyhow::bail!("SSH connection requires a terminal");
    }

    let size = termion::terminal_size()?;
    client.send_window_size(size.0, size.1).await?;

    let mut stdin = tokio::io::stdin();
    let mut stdin_buf = [0u8; 1024];

    let raw_mode = termion::raw::IntoRawMode::into_raw_mode(std::io::stdout())?;

    spinner.finish();

    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;

    // Main event loop
    loop {
        select! {
            // Handle window resizes
            _ = sigwinch.recv() => {
                if let Ok(size) = termion::terminal_size() {
                    client.send_window_size(size.0, size.1).await?;
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
                        // PTY session has ended, exit immediately
                        drop(raw_mode);
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
    }

    drop(raw_mode);
    Ok(())
}
