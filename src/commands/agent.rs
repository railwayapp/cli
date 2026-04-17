use std::io::Write;

use colored::ColoredString;
use is_terminal::IsTerminal;
use rand::Rng;
use serde::Serialize;

const THINKING_MESSAGES: &[&str] = &[
    "Chugging along...",
    "Full steam ahead...",
    "Leaving the station...",
    "Building up steam...",
    "Coupling the cars...",
    "Switching tracks...",
    "Rolling down the line...",
    "Stoking the engine...",
    "Pulling into the yard...",
    "All aboard...",
];

use crate::{
    controllers::{
        chat::{ChatEvent, ChatRequest, build_chat_client, get_chat_url, stream_chat},
        environment::get_matched_environment,
        project::get_project,
        service::get_or_prompt_service,
    },
    interact_or,
    util::progress::create_spinner,
};

use super::*;

/// Interact with the Railway Agent
#[derive(Parser)]
#[clap(
    about = "Interact with the Railway Agent",
    after_help = "Examples:\n\n\
      railway agent                                             # Interactive mode\n\
      railway agent -p \"what's the status of my deployment?\"    # Single prompt\n\
      railway agent -p \"why is my service crashing?\" --json     # JSON output"
)]
pub struct Args {
    /// Send a single prompt (omit for interactive mode)
    #[clap(short, long, value_name = "MESSAGE")]
    prompt: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,

    /// Continue an existing chat thread
    #[clap(long, value_name = "ID")]
    thread_id: Option<String>,

    /// Service to scope the chat to (name or ID)
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to use (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,
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

    if let Some(message) = args.prompt {
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
        let response = stream_json(client, url, request).await?;
        println!("{}", serde_json::to_string_pretty(&response).unwrap());
    } else {
        stream_human(client, url, request, is_tty).await?;
    }
    Ok(())
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

    println!(
        "{}",
        "Railway Agent (type 'exit' or Ctrl+C to quit)".dimmed()
    );
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

        if json {
            let response = stream_json(client, url, &request).await?;
            if let Some(tid) = response.thread_id.clone() {
                thread_id = Some(tid);
            }
            println!("{}", serde_json::to_string_pretty(&response).unwrap());
        } else if let Some(new_tid) = stream_human(client, url, &request, is_tty).await? {
            thread_id = Some(new_tid);
        }

        println!();
    }

    Ok(())
}

async fn stream_human(
    client: &reqwest::Client,
    url: &str,
    request: &ChatRequest,
    is_tty: bool,
) -> Result<Option<String>> {
    let mut renderer = HumanRenderer::new(is_tty);
    let mut thread_id = None;
    renderer.start_thinking();

    stream_chat(client, url, request, |event| {
        if let ChatEvent::Metadata {
            thread_id: ref tid, ..
        } = event
        {
            thread_id = Some(tid.clone());
        }
        renderer.handle(event);
    })
    .await?;

    Ok(thread_id)
}

async fn stream_json(
    client: &reqwest::Client,
    url: &str,
    request: &ChatRequest,
) -> Result<JsonResponse> {
    let mut response = JsonResponse::default();
    stream_chat(client, url, request, |event| {
        accumulate_json_event(event, &mut response);
    })
    .await?;
    Ok(response)
}

struct HumanRenderer {
    spinner: Option<indicatif::ProgressBar>,
    has_printed_text: bool,
    pending_markdown: String,
    block_start_pos: Option<(u16, u16)>,
    is_tty: bool,
}

impl HumanRenderer {
    fn new(is_tty: bool) -> Self {
        Self {
            spinner: None,
            has_printed_text: false,
            pending_markdown: String::new(),
            block_start_pos: None,
            is_tty,
        }
    }

    fn start_thinking(&mut self) {
        if self.is_tty {
            let msg = THINKING_MESSAGES[rand::thread_rng().gen_range(0..THINKING_MESSAGES.len())];
            println!();
            self.spinner = Some(create_spinner(msg.dimmed().to_string()));
        }
    }

    fn clear_spinner(&mut self) -> bool {
        self.spinner.take().map(|s| s.finish_and_clear()).is_some()
    }

    fn handle(&mut self, event: ChatEvent) {
        match event {
            ChatEvent::Chunk { text } => {
                let cleared = self.clear_spinner();
                if !self.has_printed_text {
                    if !cleared {
                        // Two newlines to guarantee one visible blank line between
                        // this response and whatever came before.
                        print!("\n\n");
                    }
                    if self.is_tty {
                        let _ = std::io::stdout().flush();
                        self.block_start_pos = crossterm::cursor::position().ok();
                    }
                    print!("{} ", "Railway Agent:".purple().bold());
                    self.has_printed_text = true;
                    self.pending_markdown.clear();
                }
                self.pending_markdown.push_str(&text);
                print!("{}", text);
                let _ = std::io::stdout().flush();
            }
            ChatEvent::ToolCallReady { tool_name, .. } => {
                if !self.is_tty {
                    return;
                }
                self.flush_pending();
                self.clear_spinner();
                self.has_printed_text = false;
                self.spinner = Some(create_spinner(format!(
                    "{} {}",
                    "╰─".dimmed(),
                    tool_badge(&format!(" Agent Tool: {tool_name} "))
                )));
            }
            ChatEvent::ToolExecutionComplete { is_error, .. } => {
                self.clear_spinner();
                if self.is_tty {
                    let label = if is_error {
                        " ✗ Tool failed "
                    } else {
                        " ✓ Done "
                    };
                    println!("{}", tool_badge(label));
                }
            }
            ChatEvent::Error { message } => {
                self.clear_spinner();
                eprintln!("{}: {}", "Error".red().bold(), message);
            }
            ChatEvent::Aborted { reason } => {
                self.clear_spinner();
                let msg = reason.unwrap_or_else(|| "Request was aborted".to_string());
                eprintln!("{}: {}", "Aborted".yellow().bold(), msg);
            }
            ChatEvent::WorkflowCompleted { .. } => {
                if !self.flush_pending() {
                    println!();
                }
            }
            ChatEvent::Metadata { .. } => {}
        }
    }

    fn flush_pending(&mut self) -> bool {
        if self.pending_markdown.is_empty() {
            return false;
        }

        if let Some((col, row)) = self.block_start_pos.take() {
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::cursor::MoveTo(col, row),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
            );
        } else {
            println!();
        }

        println!("{}", "Railway Agent:".purple().bold());
        termimad::MadSkin::default().print_text(&self.pending_markdown);

        self.pending_markdown.clear();
        let _ = std::io::stdout().flush();
        true
    }
}

fn tool_badge(text: &str) -> ColoredString {
    text.truecolor(255, 255, 255).on_truecolor(68, 68, 68)
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
