use anyhow::{bail, Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

use crate::{
    client::GQLClient,
    config::Configs,
    consts::TICK_STRING,
    controllers::terminal::{self, TerminalClient},
};

pub const SSH_CONNECTION_TIMEOUT_SECS: u64 = 30;
pub const SSH_MESSAGE_TIMEOUT_SECS: u64 = 10;
pub const SSH_MAX_CONNECT_ATTEMPTS: usize = 3;
pub const SSH_CONNECT_DELAY_SECS: u64 = 5;
pub const SSH_MAX_EMPTY_MESSAGES: usize = 100;

mod common;
mod platform;

use common::*;
use platform::*;

/// Connect to a service via SSH
#[derive(Parser, Clone)]
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

    /// SSH into the service inside a tmux session. Installs tmux if it's not installed
    #[clap(long)]
    tmux: bool,

    /// Command to execute instead of starting an interactive shell
    #[clap(trailing_var_arg = true)]
    command: Vec<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let params = get_ssh_connect_params(args.clone(), &configs, &client).await?;

    if args.tmux {
        run_tmux_session(&params).await?;
        return Ok(());
    }

    // Determine if we're running a command or interactive shell
    let running_command = !args.command.is_empty();

    let spinner = create_spinner(running_command);
    let mut terminal_client = create_client(&params).await?;

    if running_command {
        // Run single command
        execute_command(&mut terminal_client, args.command.join(" "), spinner).await?;
    } else {
        // Initialize interactive shell (default to bash)
        initialize_shell(&mut terminal_client, Some("bash".to_string()), spinner).await?;

        // Run the platform-specific event loop (unix/windows implements terminals differently)
        match run_interactive_session(terminal_client).await? {
            SessionTermination::Complete => {}
            term => {
                eprintln!("{}", term.message());
                std::process::exit(term.exit_code());
            }
        }
    }

    Ok(())
}

async fn run_tmux_session(params: &terminal::SSHConnectParams) -> Result<()> {
    let tmux_exists = check_if_command_exists(params, "tmux").await?;

    if !tmux_exists {
        install_tmux(params).await?;
    }

    loop {
        let mut terminal_client = create_client(params).await?;
        let spinner = create_spinner(true);

        initialize_shell(&mut terminal_client, Some("bash".to_string()), spinner).await?;

        terminal_client
            .send_data("exec tmux new-session -A -s railway\n")
            .await?;

        send_window_size(&mut terminal_client).await?;

        let termination = run_interactive_session(terminal_client).await?;

        match termination {
            SessionTermination::Complete => {
                break;
            }
            SessionTermination::ConnectionReset => {
                // Clean up terminal screen before reconnecting
                reset_terminal(true)?;

                // Add a small delay before reconnecting
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                println!("Connection reset. Reconnecting...");
                continue;
            }
            term => {
                eprintln!("{}", term.message());
                std::process::exit(term.exit_code());
            }
        };
    }

    Ok(())
}

async fn install_tmux(params: &terminal::SSHConnectParams) -> Result<()> {
    let command = "apt-get update && apt-get install -y tmux";

    let spinner = ProgressBar::new_spinner()
        .with_style(
            ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")
                .expect("Failed to create spinner template"),
        )
        .with_message("Installing tmux...");

    spinner.enable_steady_tick(Duration::from_millis(100));

    let mut terminal_client = create_client(params).await?;

    let result =
        execute_command_with_result(&mut terminal_client, command.to_string(), spinner).await;

    if let Err(err) = result {
        println!("Error installing tmux: {}", err);
        std::process::exit(1);
    }

    Ok(())
}

pub async fn check_if_command_exists(
    params: &terminal::SSHConnectParams,
    command: &str,
) -> Result<bool> {
    let mut terminal_client = create_client(params).await?;
    let spinner = create_spinner(true);

    let result =
        execute_command_with_result(&mut terminal_client, format!("which {}", command), spinner)
            .await;

    Ok(result.is_ok())
}

async fn create_client(params: &terminal::SSHConnectParams) -> Result<TerminalClient> {
    let configs = Configs::new()?;
    let token = configs
        .get_railway_auth_token()
        .context("No authentication token found. Please login first with 'railway login'")?;

    let ws_url = format!("wss://{}", configs.get_relay_host_path());
    let terminal_client = create_terminal_client(&ws_url, &token, &params).await?;

    Ok(terminal_client)
}
