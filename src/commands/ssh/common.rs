use anyhow::{anyhow, Result};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;
use tokio::time::Duration;

use crate::config::Configs;
use crate::consts::TICK_STRING;
use crate::controllers::{
    environment::get_matched_environment,
    project::get_project,
    service::get_or_prompt_service,
    terminal::{SSHConnectParams, TerminalClient},
};

use super::Args;

pub async fn get_ssh_connect_params(
    args: Args,
    configs: &Configs,
    client: &Client,
) -> Result<SSHConnectParams> {
    let has_project = args.project.is_some();
    let has_service = args.service.is_some();
    let has_environment = args.environment.is_some();

    let provided_args_count = [has_project, has_service, has_environment]
        .iter()
        .filter(|&&x| x)
        .count();

    if provided_args_count > 0 && provided_args_count < 3 {
        if !has_project {
            return Err(anyhow!(
                "Must provide project when setting service or environment"
            ));
        }
        if !has_environment {
            return Err(anyhow!(
                "Must provide environment when setting project or service"
            ));
        }
        if !has_service {
            return Err(anyhow!(
                "Must provide service when setting project or environment"
            ));
        }
    }

    if provided_args_count == 3 {
        let project_id = args.project.unwrap();
        let project = get_project(client, configs, project_id.clone()).await?;

        let environment = args.environment.unwrap();
        let environment_id = get_matched_environment(&project, environment)?.id;

        let service_id = args.service.unwrap();

        return Ok(SSHConnectParams {
            project_id,
            environment_id,
            service_id,
            deployment_instance_id: args.deployment_instance,
        });
    }

    let linked_project = configs.get_linked_project().await?;
    let project_id = linked_project.project.clone();
    let project = get_project(client, configs, project_id.clone()).await?;

    let environment = linked_project.environment.clone();
    let environment_id = get_matched_environment(&project, environment)?.id;

    let service_id = get_or_prompt_service(linked_project.clone(), project, None)
        .await?
        .ok_or_else(|| anyhow!("No service found. Please specify a service to connect to via the `--service` flag."))?;

    Ok(SSHConnectParams {
        project_id,
        environment_id,
        service_id,
        deployment_instance_id: args.deployment_instance,
    })
}

pub fn create_spinner(running_command: bool) -> ProgressBar {
    let message = if running_command {
        "Connecting to execute command..."
    } else {
        "Connecting to service..."
    };

    let spinner = ProgressBar::new_spinner()
        .with_style(
            ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")
                .expect("Failed to create spinner template"),
        )
        .with_message(message);

    spinner.enable_steady_tick(Duration::from_millis(100));
    spinner
}

pub async fn create_terminal_client(
    ws_url: &str,
    token: &str,
    params: &SSHConnectParams,
) -> Result<TerminalClient> {
    let client = TerminalClient::new(ws_url, token, params).await?;
    Ok(client)
}

pub async fn initialize_shell(
    client: &mut TerminalClient,
    shell: Option<String>,
    spinner: ProgressBar,
) -> Result<()> {
    client.init_shell(shell).await?;

    client.wait_for_shell_ready(5).await?;

    spinner.finish_with_message("Connected to interactive shell");

    crossterm::terminal::enable_raw_mode()?;

    if let Ok((cols, rows)) = crossterm::terminal::size() {
        client.send_window_size(cols, rows).await?;
    }

    Ok(())
}

pub async fn execute_command(
    client: &mut TerminalClient,
    command_args: Vec<String>,
    spinner: ProgressBar,
) -> Result<()> {
    if command_args.is_empty() {
        return Err(anyhow!("No command specified"));
    }

    let full_command = command_args.join(" ");
    let wrapped_command = "sh";
    let wrapped_args = vec!["-c".to_string(), full_command];

    client.send_command(wrapped_command, wrapped_args).await?;

    spinner.finish_and_clear();

    match client.handle_server_messages().await {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
