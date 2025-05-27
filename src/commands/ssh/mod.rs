use anyhow::{Context, Result};
use clap::Parser;
use indicatif::ProgressBar;

use crate::{
    client::GQLClient,
    config::Configs,
    controllers::terminal::{self, TerminalClient},
    util::progress::{create_spinner, fail_spinner},
};

pub const SSH_CONNECTION_TIMEOUT_SECS: u64 = 30;
pub const SSH_MESSAGE_TIMEOUT_SECS: u64 = 10;
pub const SSH_CONNECT_DELAY_SECS: u64 = 5;
pub const SSH_MAX_EMPTY_MESSAGES: usize = 100;

pub const SSH_MAX_CONNECT_ATTEMPTS: u32 = 3;
pub const SSH_MAX_CONNECT_ATTEMPTS_PERSISTENT: u32 = 20;

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
    session: bool,

    /// Command to execute instead of starting an interactive shell
    #[clap(trailing_var_arg = true)]
    command: Vec<String>,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let params = get_ssh_connect_params(args.clone(), &configs, &client).await?;

    if args.session {
        run_persistent_session(&params).await?;
        return Ok(());
    }

    // Determine if we're running a command or interactive shell
    let running_command = !args.command.is_empty();

    let mut spinner = create_spinner("Connecting to service...".to_string());
    let mut terminal_client = create_client(&params, &mut spinner, None).await?;

    if running_command {
        // Run single command
        execute_command(&mut terminal_client, args.command.join(" "), spinner).await?;
    } else {
        // Initialize interactive shell (default to bash)
        initialize_shell(&mut terminal_client, Some("bash".to_string()), &mut spinner).await?;

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

async fn run_persistent_session(params: &terminal::SSHConnectParams) -> Result<()> {
    ensure_tmux_is_installed(params).await?;

    loop {
        let mut spinner = create_spinner("Connecting to service...".to_string());

        let mut terminal_client = match create_client(
            params,
            &mut spinner,
            Some(SSH_MAX_CONNECT_ATTEMPTS_PERSISTENT),
        )
        .await
        {
            Ok(tc) => tc,
            Err(e) => {
                fail_spinner(&mut spinner, format!("{}", e));
                std::process::exit(1);
            }
        };

        // Start tmux session
        initialize_shell(&mut terminal_client, Some("bash".to_string()), &mut spinner).await?;

        terminal_client
            .send_data("exec tmux new-session -A -s railway \\; set -g mouse on \n")
            .await?;

        // Resend the window size after starting a tmux session
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
            SessionTermination::SendError(e)
            | SessionTermination::StdinError(e)
            | SessionTermination::ServerError(e) => {
                println!("Session error: {}. Reconnecting...", e);
                continue;
            }
        };
    }

    reset_terminal(false)?;

    Ok(())
}

/// Installs tmux with apt-get if not already installed
async fn ensure_tmux_is_installed(params: &terminal::SSHConnectParams) -> Result<()> {
    let command = "which tmux || (apt-get update && apt-get install -y tmux)";

    let mut spinner = create_spinner("Installing tmux...".to_string());
    let mut terminal_client = create_client(params, &mut spinner, None).await?;

    let result =
        execute_command_with_result(&mut terminal_client, command.to_string(), &mut spinner).await;

    if let Err(err) = result {
        fail_spinner(&mut spinner, format!("Error installing tmux: {}", err));
        std::process::exit(1);
    }

    Ok(())
}

async fn create_client(
    params: &terminal::SSHConnectParams,
    spinner: &mut ProgressBar,
    max_attempts: Option<u32>,
) -> Result<TerminalClient> {
    let configs = Configs::new()?;
    let token = configs
        .get_railway_auth_token()
        .context("No authentication token found. Please login first with 'railway login'")?;

    let ws_url = format!("wss://{}", configs.get_relay_host_path());
    let terminal_client =
        create_terminal_client(&ws_url, &token, params, spinner, max_attempts).await?;

    Ok(terminal_client)
}
