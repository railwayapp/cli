use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::time::Duration;

use crate::client::GQLClient;
use crate::config::Configs;
use crate::consts::TICK_STRING;
use crate::controllers::terminal::{SSHConnectParams, TerminalClient};

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
    #[arg(long = "tmux")]
    tmux: bool,

    /// Command to execute instead of starting an interactive shell
    #[clap(trailing_var_arg = true)]
    command: Vec<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let params = get_ssh_connect_params(args.clone(), &configs, &client).await?;

    let token = configs
        .get_railway_auth_token()
        .context("No authentication token found. Please login first with 'railway login'")?;

    // Determine if we're running a command or interactive shell
    let running_command = !args.command.is_empty();

    if args.tmux {
        run_tmux_session(&configs, &params).await?;
        return Ok(());
    }

    let spinner = create_spinner(running_command);
    let ws_url = format!("wss://{}", configs.get_relay_host_path());
    let mut terminal_client =
        crate::commands::ssh::common::create_terminal_client(&ws_url, &token, &params).await?;

    if running_command {
        // Run single command
        execute_command(&mut terminal_client, args.command.join(" "), spinner).await
    } else {
        // Initialize interactive shell (default to bash)
        initialize_shell(&mut terminal_client, Some("bash".to_string()), spinner).await?;

        // Run the platform-specific event loop (unix/windows implements terminals differently)
        run_interactive_session(terminal_client).await
    }
}

async fn run_tmux_session(configs: &Configs, params: &SSHConnectParams) -> Result<()> {
    let tmux_exists = check_if_command_exists(&configs, params, "tmux").await?;

    if !tmux_exists {
        install_tmux(&configs, params).await?;
    }

    let token = configs
        .get_railway_auth_token()
        .context("No authentication token found. Please login first with 'railway login'")?;

    let ws_url = format!("wss://{}", configs.get_relay_host_path());
    let mut terminal_client = create_terminal_client(&ws_url, &token, &params).await?;
    let spinner = create_spinner(true);

    initialize_shell(&mut terminal_client, Some("bash".to_string()), spinner).await?;

    terminal_client
        .send_data("exec tmux new-session -A -s railway\n")
        .await?;

    terminal_client.send_window_size(cols, rows)

    // Run the platform-specific event loop (unix/windows implements terminals differently)
    let result = run_interactive_session(terminal_client).await;

    if let Err(err) = result {
        println!("Error running tmux session: {}", err);
        std::process::exit(1);
    }

    Ok(())
}

pub async fn check_if_command_exists(
    configs: &Configs,
    params: &SSHConnectParams,
    command: &str,
) -> Result<bool> {
    let token = configs
        .get_railway_auth_token()
        .context("No authentication token found. Please login first with 'railway login'")?;

    let ws_url = format!("wss://{}", configs.get_relay_host_path());
    let mut terminal_client = create_terminal_client(&ws_url, &token, &params).await?;
    let spinner = create_spinner(true);

    let result =
        execute_command_with_result(&mut terminal_client, format!("which {}", command), spinner)
            .await;

    Ok(result.is_ok())
}

async fn install_tmux(configs: &Configs, params: &SSHConnectParams) -> Result<()> {
    let token = configs
        .get_railway_auth_token()
        .context("No authentication token found. Please login first with 'railway login'")?;

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

    let ws_url = format!("wss://{}", configs.get_relay_host_path());
    let mut terminal_client =
        crate::commands::ssh::common::create_terminal_client(&ws_url, &token, &params).await?;

    let result =
        execute_command_with_result(&mut terminal_client, command.to_string(), spinner).await;

    if let Err(err) = result {
        println!("Error installing tmux: {}", err);
        std::process::exit(1);
    }

    Ok(())
}
