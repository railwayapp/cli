use anyhow::{Context, Result};
use clap::Parser;

use crate::client::GQLClient;
use crate::config::Configs;

pub const SSH_CONNECTION_TIMEOUT_SECS: u64 = 30;
pub const SSH_MESSAGE_TIMEOUT_SECS: u64 = 10;
pub const SSH_MAX_RECONNECT_ATTEMPTS: usize = 3;
pub const SSH_RECONNECT_DELAY_SECS: u64 = 5;
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

    let spinner = create_spinner(running_command);

    let ws_url = format!("wss://{}", configs.get_relay_host_path());
    let mut terminal_client = establish_connection(&ws_url, &token, &params).await?;

    if running_command {
        // Run single command
        execute_command(&mut terminal_client, args.command, spinner).await
    } else {
        // Initialize interactive shell (default to bash)
        initialize_shell(&mut terminal_client, Some("bash".to_string()), spinner).await?;
        // Run the platform-specific event loop (unix/windows implements terminals differently)
        run_interactive_session(&mut terminal_client).await
    }
}
