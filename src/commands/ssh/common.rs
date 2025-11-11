use std::io::Cursor;

use anyhow::bail;
use anyhow::{Context, Result, anyhow};
use indicatif::ProgressBar;
use reqwest::Client;
use std::io::Write;

use crate::commands::queries::RailwayProject;
use crate::controllers::{
    environment::get_matched_environment,
    project::get_project,
    service::get_or_prompt_service,
    terminal::{SSHConnectParams, TerminalClient},
};
use crate::util::progress::success_spinner;
use crate::{commands::ssh::AuthKind, config::Configs};

use super::Args;

#[derive(Debug)]
pub enum SessionTermination {
    /// Session has been successfully closed
    Complete,

    /// Error reading from stdin
    StdinError(String),

    /// Error sending data to the server
    SendError(String),

    /// Server error occurred
    ServerError(String),

    /// Connection to the server was closed unexpectedly
    ConnectionReset,
}

impl SessionTermination {
    pub fn exit_code(&self) -> i32 {
        match self {
            SessionTermination::Complete => 0,
            SessionTermination::StdinError(_) => 2,
            SessionTermination::SendError(_) => 3,
            SessionTermination::ServerError(_) => 4,
            SessionTermination::ConnectionReset => 5,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            SessionTermination::Complete => "",
            SessionTermination::StdinError(msg) => msg,
            SessionTermination::SendError(msg) => msg,
            SessionTermination::ServerError(msg) => msg,
            SessionTermination::ConnectionReset => {
                "Connection to the server was closed unexpectedly"
            }
        }
    }
}

pub fn parse_server_error(error: String) -> SessionTermination {
    if error.contains("Connection reset without closing handshake")
        || error.contains("WebSocket closed unexpectedly")
    {
        SessionTermination::ConnectionReset
    } else {
        SessionTermination::ServerError(error)
    }
}

pub async fn find_service_by_name(
    client: &Client,
    configs: &Configs,
    project: &RailwayProject,
    service_id_or_name: &str,
) -> Result<String> {
    let project = get_project(&client, &configs, project.id.clone()).await?;

    let services = project.services.edges.iter().collect::<Vec<_>>();

    let service = services
        .iter()
        // Match service on lowercase name or id
        .find(|service| {
            service.node.name.to_lowercase() == service_id_or_name.to_lowercase()
                || service.node.id == service_id_or_name
        })
        .with_context(|| format!("Service '{service_id_or_name}' not found"))?
        .node
        .id
        .to_owned();

    return Ok(service);
}

pub async fn get_ssh_connect_params(
    args: Args,
    configs: &Configs,
    client: &Client,
) -> Result<SSHConnectParams> {
    let has_project = args.project.is_some();
    let has_service = args.service.is_some();
    let has_environment = args.environment.is_some();

    let linked_project = configs.get_linked_project().await?;
    let project_id;
    if has_project {
        project_id = args.project.unwrap();
    } else {
        project_id = linked_project.project.clone();
    }
    let project = get_project(client, configs, project_id.clone()).await?;

    let environment;
    if has_environment {
        environment = args.environment.unwrap();
    } else {
        environment = linked_project.environment.clone();
    }
    let environment_id = get_matched_environment(&project, environment)?.id;

    let service_id;
    if has_service {
        let service_id_or_name = args.service.unwrap();
        service_id = find_service_by_name(&client, &configs, &project, &service_id_or_name).await?
    } else {
        service_id = get_or_prompt_service(linked_project.clone(), project, None)
            .await?
            .ok_or_else(|| anyhow!("No service found. Please specify a service to connect to via the `--service` flag."))?;
    }

    Ok(SSHConnectParams {
        project_id,
        environment_id,
        service_id,
        deployment_instance_id: args.deployment_instance,
    })
}

pub async fn create_terminal_client(
    ws_url: &str,
    token: AuthKind,
    params: &SSHConnectParams,
    spinner: &mut ProgressBar,
    max_attempts: Option<u32>,
) -> Result<TerminalClient> {
    let client = TerminalClient::new(ws_url, token, params, spinner, max_attempts).await?;
    Ok(client)
}

pub async fn initialize_shell(
    client: &mut TerminalClient,
    shell: Option<String>,
    spinner: &mut ProgressBar,
) -> Result<()> {
    client.init_shell(shell).await?;

    client.wait_for_shell_ready(5).await?;

    success_spinner(spinner, "Connected to interactive shell".to_string());

    crossterm::terminal::enable_raw_mode()?;

    send_window_size(client).await?;

    Ok(())
}

pub async fn send_window_size(client: &mut TerminalClient) -> Result<()> {
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        client.send_window_size(cols, rows).await?;
    }

    Ok(())
}

pub async fn execute_command(
    client: &mut TerminalClient,
    command: String,
    spinner: ProgressBar,
) -> Result<()> {
    let (wrapped_command, wrapped_args) = get_terminal_command(command)?;
    client.send_command(&wrapped_command, wrapped_args).await?;
    spinner.finish_and_clear();

    match client.handle_server_messages().await {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

pub async fn execute_command_with_result(
    client: &mut TerminalClient,
    command: String,
    spinner: &mut ProgressBar,
) -> Result<String> {
    let (wrapped_command, wrapped_args) = get_terminal_command(command)?;
    client.send_command(&wrapped_command, wrapped_args).await?;

    let mut buffer = Cursor::new(Vec::new());
    match client
        .handle_server_messages_with_writer(&mut buffer, false)
        .await
    {
        Ok(_) => {
            spinner.finish_and_clear();
            let output = String::from_utf8(buffer.into_inner())?;
            Ok(output)
        }
        Err(e) => {
            spinner.finish_and_clear();
            Err(e)
        }
    }
}

fn get_terminal_command(command: String) -> Result<(String, Vec<String>)> {
    if command.is_empty() {
        return Err(anyhow!("No command specified"));
    }

    let wrapped_command = "sh";
    let wrapped_args = vec!["-c".to_string(), command];

    Ok((wrapped_command.to_string(), wrapped_args))
}

/// Reset the terminal state, clear the screen, and make the cursor visible
pub fn reset_terminal(clear_screen: bool) -> anyhow::Result<()> {
    let _ = crossterm::terminal::disable_raw_mode();

    if clear_screen {
        // Clear screen, move cursor to home position, and reset all attributes
        print!("\x1b[2J\x1b[H\x1b[0m");
    } else {
        // Just reset attributes
        print!("\x1b[0m");
    }

    // Ensure cursor is visible
    print!("\x1b[?25h");

    std::io::stdout().flush()?;

    Ok(())
}
