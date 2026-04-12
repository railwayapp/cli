use std::io::Write;

use anyhow::Context;
use crate::{
    controllers::{
        chat::{ChatEvent, ChatRequest, build_chat_client, get_chat_url, stream_chat},
        environment::get_matched_environment,
        project::get_project,
    },
    interact_or,
    util::progress::{create_spinner, fail_spinner, success_spinner},
};

use super::*;

pub struct Args {
    pub message: Option<String>,
    pub json: bool,
    pub thread_id: Option<String>,
    pub service: Option<String>,
    pub environment: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let linked_project = configs.get_linked_project().await?;

    let client = GQLClient::new_authorized(&configs)?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let environment_id = match args.environment.clone() {
        Some(env) => get_matched_environment(&project, env)?.id,
        None => linked_project.environment_id()?.to_string(),
    };

    let service_id = match args.service {
        Some(ref service_arg) => {
            let svc = project
                .services
                .edges
                .iter()
                .find(|s| s.node.name == *service_arg || s.node.id == *service_arg)
                .with_context(|| format!("Service '{service_arg}' not found"))?;
            Some(svc.node.id.clone())
        }
        None => linked_project.service.clone(),
    };

    let chat_client = build_chat_client(&configs)?;
    let url = get_chat_url(&configs);

    if let Some(message) = args.message {
        run_single_shot(&chat_client, &url, &ChatRequest {
            project_id: linked_project.project.clone(),
            environment_id,
            message,
            thread_id: args.thread_id,
            service_id,
        }, args.json).await
    } else {
        run_repl(
            &chat_client,
            &url,
            &linked_project.project,
            &environment_id,
            service_id.as_deref(),
            args.thread_id,
            args.json,
        ).await
    }
}

async fn run_single_shot(
    client: &reqwest::Client,
    url: &str,
    request: &ChatRequest,
    json: bool,
) -> Result<()> {
    let mut spinner: Option<indicatif::ProgressBar> = None;

    stream_chat(client, url, request, |event| {
        if json {
            handle_event_json(&event);
        } else {
            handle_event_human(event, &mut spinner);
        }
    }).await
}

async fn run_repl(
    client: &reqwest::Client,
    url: &str,
    project_id: &str,
    environment_id: &str,
    service_id: Option<&str>,
    initial_thread_id: Option<String>,
    json: bool,
) -> Result<()> {
    interact_or!("Interactive chat requires a terminal. Pass a message as an argument for non-interactive use.");

    println!(
        "{}",
        "Railway Chat (type 'exit' or Ctrl+C to quit)"
            .dimmed()
    );
    println!();

    let mut thread_id = initial_thread_id;

    loop {
        let input = inquire::Text::new("You:")
            .with_render_config(Configs::get_render_config())
            .prompt();

        let message = match input {
            Ok(msg) if msg.trim().eq_ignore_ascii_case("exit") || msg.trim().eq_ignore_ascii_case("quit") => break,
            Ok(msg) if msg.trim().is_empty() => continue,
            Ok(msg) => msg,
            Err(inquire::InquireError::OperationInterrupted) => break,
            Err(e) => return Err(e.into()),
        };

        let request = ChatRequest {
            project_id: project_id.to_string(),
            environment_id: environment_id.to_string(),
            message,
            thread_id: thread_id.clone(),
            service_id: service_id.map(|s| s.to_string()),
        };

        println!();
        let mut spinner: Option<indicatif::ProgressBar> = None;

        stream_chat(client, url, &request, |event| {
            if let ChatEvent::Metadata { thread_id: ref tid, .. } = event {
                thread_id = Some(tid.clone());
            }
            if json {
                handle_event_json(&event);
            } else {
                handle_event_human(event, &mut spinner);
            }
        }).await?;

        println!();
    }

    Ok(())
}

fn handle_event_human(event: ChatEvent, spinner: &mut Option<indicatif::ProgressBar>) {
    match event {
        ChatEvent::Chunk { text } => {
            if let Some(s) = spinner.take() {
                s.finish_and_clear();
            }
            print!("{}", text);
            let _ = std::io::stdout().flush();
        }
        ChatEvent::ToolCallReady { tool_name, .. } => {
            *spinner = Some(create_spinner(format!("Running: {tool_name}")));
        }
        ChatEvent::ToolExecutionComplete { is_error, .. } => {
            if let Some(s) = spinner {
                if is_error {
                    fail_spinner(s, "Tool failed".to_string());
                } else {
                    success_spinner(s, "Done".to_string());
                }
            }
            *spinner = None;
        }
        ChatEvent::Error { message } => {
            eprintln!("{}: {}", "Error".red().bold(), message);
        }
        ChatEvent::WorkflowCompleted { .. } => {
            println!();
        }
        ChatEvent::Metadata { .. } => {
            // Thread ID captured by caller; no output
        }
    }
}

fn handle_event_json(event: &ChatEvent) {
    if let Ok(json) = serde_json::to_string(event) {
        println!("{json}");
    }
}
