use std::io::Write;

use is_terminal::IsTerminal;
use serde::Serialize;

use crate::{
    controllers::{
        chat::{ChatEvent, ChatRequest, build_chat_client, get_chat_url, stream_chat},
        environment::get_matched_environment,
        project::get_project,
        service::get_or_prompt_service,
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

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
    response: String,
    tool_calls: Vec<JsonToolCall>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonToolCall {
    tool_name: String,
    args: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    is_error: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let linked_project = configs.get_linked_project().await?;
    let project_id = linked_project.project.clone();

    let client = GQLClient::new_authorized(&configs)?;
    let project = get_project(&client, &configs, project_id.clone()).await?;

    let environment_id = match args.environment.clone() {
        Some(env) => get_matched_environment(&project, env)?.id,
        None => linked_project.environment_id()?.to_string(),
    };

    let service_id = get_or_prompt_service(Some(linked_project), project, args.service).await?;

    let chat_client = build_chat_client(&configs)?;
    let url = get_chat_url(&configs);
    let is_tty = std::io::stdout().is_terminal();

    if let Some(message) = args.message {
        run_single_shot(
            &chat_client,
            &url,
            &ChatRequest {
                project_id,
                environment_id,
                message,
                thread_id: args.thread_id,
                service_id,
            },
            args.json,
            is_tty,
        )
        .await
    } else {
        run_repl(
            &chat_client,
            &url,
            &project_id,
            &environment_id,
            service_id.as_deref(),
            args.thread_id,
            args.json,
            is_tty,
        )
        .await
    }
}

async fn run_single_shot(
    client: &reqwest::Client,
    url: &str,
    request: &ChatRequest,
    json: bool,
    is_tty: bool,
) -> Result<()> {
    if json {
        let mut response = JsonResponse::default();

        stream_chat(client, url, request, |event| {
            accumulate_json_event(event, &mut response);
        })
        .await?;

        println!("{}", serde_json::to_string_pretty(&response).unwrap());
        Ok(())
    } else {
        let mut spinner: Option<indicatif::ProgressBar> = None;
        let mut has_printed_text = false;

        // Show a thinking spinner while waiting for the first event
        if is_tty {
            spinner = Some(create_spinner("Thinking...".dimmed().to_string()));
        }

        stream_chat(client, url, request, |event| {
            handle_event_human(event, &mut spinner, &mut has_printed_text, is_tty);
        })
        .await
    }
}

async fn run_repl(
    client: &reqwest::Client,
    url: &str,
    project_id: &str,
    environment_id: &str,
    service_id: Option<&str>,
    initial_thread_id: Option<String>,
    json: bool,
    is_tty: bool,
) -> Result<()> {
    interact_or!(
        "Interactive mode requires a terminal. Use `railway -p \"your message\"` for non-interactive use."
    );

    println!("{}", "Railway AI (type 'exit' or Ctrl+C to quit)".dimmed());
    println!();

    let mut thread_id = initial_thread_id;

    loop {
        let input = inquire::Text::new("You:")
            .with_render_config(Configs::get_render_config())
            .prompt();

        let message = match input {
            Ok(msg)
                if msg.trim().eq_ignore_ascii_case("exit")
                    || msg.trim().eq_ignore_ascii_case("quit") =>
            {
                break;
            }
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

        if json {
            let mut response = JsonResponse::default();

            stream_chat(client, url, &request, |event| {
                if let ChatEvent::Metadata {
                    thread_id: ref tid, ..
                } = event
                {
                    thread_id = Some(tid.clone());
                }
                accumulate_json_event(event, &mut response);
            })
            .await?;

            println!("{}", serde_json::to_string_pretty(&response).unwrap());
        } else {
            let mut spinner: Option<indicatif::ProgressBar> = None;
            let mut has_printed_text = false;

            if is_tty {
                spinner = Some(create_spinner("Thinking...".dimmed().to_string()));
            }

            stream_chat(client, url, &request, |event| {
                if let ChatEvent::Metadata {
                    thread_id: ref tid, ..
                } = event
                {
                    thread_id = Some(tid.clone());
                }
                handle_event_human(event, &mut spinner, &mut has_printed_text, is_tty);
            })
            .await?;
        }

        println!();
    }

    Ok(())
}

fn handle_event_human(
    event: ChatEvent,
    spinner: &mut Option<indicatif::ProgressBar>,
    has_printed_text: &mut bool,
    is_tty: bool,
) {
    match event {
        ChatEvent::Chunk { text } => {
            if let Some(s) = spinner.take() {
                s.finish_and_clear();
            }
            if !*has_printed_text {
                println!();
                print!("{} ", "Railway AI:".purple().bold());
                *has_printed_text = true;
            }
            print!("{}", text);
            let _ = std::io::stdout().flush();
        }
        ChatEvent::ToolCallReady { tool_name, .. } => {
            if is_tty {
                // Clear any existing spinner before starting a new one
                if let Some(s) = spinner.take() {
                    s.finish_and_clear();
                }
                *spinner = Some(create_spinner(
                    format!("Running: {tool_name}").dimmed().to_string(),
                ));
            }
        }
        ChatEvent::ToolExecutionComplete { is_error, .. } => {
            if let Some(s) = spinner {
                if is_error {
                    fail_spinner(s, "Tool failed".to_string());
                } else {
                    success_spinner(s, "Done".dimmed().to_string());
                }
            }
            *spinner = None;
        }
        ChatEvent::Error { message } => {
            if let Some(s) = spinner.take() {
                s.finish_and_clear();
            }
            eprintln!("{}: {}", "Error".red().bold(), message);
        }
        ChatEvent::Aborted { reason } => {
            if let Some(s) = spinner.take() {
                s.finish_and_clear();
            }
            let msg = reason.unwrap_or_else(|| "Request was aborted".to_string());
            eprintln!("{}: {}", "Aborted".yellow().bold(), msg);
        }
        ChatEvent::WorkflowCompleted { .. } => {
            println!();
        }
        ChatEvent::Metadata { .. } => {
            // Thread ID captured by caller; no output
        }
    }
}

fn accumulate_json_event(event: ChatEvent, response: &mut JsonResponse) {
    match event {
        ChatEvent::Metadata { thread_id, .. } => {
            response.thread_id = Some(thread_id);
        }
        ChatEvent::Chunk { text } => {
            response.response.push_str(&text);
        }
        ChatEvent::ToolCallReady {
            tool_name, args, ..
        } => {
            response.tool_calls.push(JsonToolCall {
                tool_name,
                args,
                result: None,
                is_error: false,
            });
        }
        ChatEvent::ToolExecutionComplete {
            result, is_error, ..
        } => {
            if let Some(last) = response.tool_calls.last_mut() {
                last.result = Some(result);
                last.is_error = is_error;
            }
        }
        ChatEvent::Error { message } => {
            response.response.push_str(&format!("\nError: {message}"));
        }
        ChatEvent::Aborted { reason } => {
            let msg = reason.unwrap_or_else(|| "Request was aborted".to_string());
            response.response.push_str(&format!("\nAborted: {msg}"));
        }
        ChatEvent::WorkflowCompleted { .. } => {}
    }
}
