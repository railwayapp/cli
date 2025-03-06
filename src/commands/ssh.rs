use anyhow::{bail, Context, Result};
use is_terminal::IsTerminal;
use reqwest::Client;
use tokio::io::AsyncReadExt;
use tokio::select;

use super::{
    queries::deployments::{DeploymentListInput, DeploymentStatus, DeploymentStatusInput},
    *,
};
use crate::{
    consts::TICK_STRING,
    controllers::{
        environment::get_matched_environment, project::get_project, service::get_or_prompt_service,
        terminal::TerminalClient,
    },
    util::prompt::{prompt_select, PromptService},
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

#[derive(Clone, Debug)]
struct SSHConnectParams {
    project_id: String,
    environment_id: String,
    service_id: String,
    deployment_instance_id: Option<String>,
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

    let mut client = TerminalClient::new(
        &ws_url,
        &token,
        &params.project_id,
        &params.service_id,
        params.deployment_instance_id.as_deref(),
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
