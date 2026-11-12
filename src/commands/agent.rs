//! `railway agent`: ask the Railway Agent something, from the terminal.
//!
//! Each message is a task run on a cloud agent (see `railway ca task`). A
//! thread is the agent and session those tasks share: the first message
//! starts a cloud agent VM, which is billed, so the command says so and asks
//! first. Later messages continue that agent. A thread is named by the ID of
//! any task in it.

use std::collections::HashSet;

use is_terminal::IsTerminal;
use serde_json::{Value, json};

use crate::{
    commands::cloud_agent::task::{self, Answer},
    controllers::{environment::get_matched_environment, project::get_project},
    interact_or,
    util::prompt::prompt_confirm_with_default,
};

use super::*;

/// What the user accepts before a new thread starts a cloud agent.
const NEW_SESSION_NOTICE: &str = "Railway Agent runs on a cloud agent. This will start a new cloud agent VM for this session and use Railway Agent credits. The VM is billed while it is active and sleeps after five minutes idle.";

/// How many extra polls to give an approval to register before handing the
/// task back as still waiting.
const ANSWER_SETTLE_POLLS: usize = 20;

/// Interact with the Railway Agent
#[derive(Parser)]
#[clap(
    about = "Interact with the Railway Agent",
    after_help = "Examples:\n\n\
      railway agent                                             # Interactive mode\n\
      railway agent -p \"what's the status of my deployment?\"    # Single prompt\n\
      railway agent -p \"why is my service crashing?\" --json     # JSON output\n\
      railway agent -p \"summarize today's errors\" --accept      # Skip the cloud agent prompt\n\
      railway agent --list                                      # List existing threads\n\
      railway agent --thread-id <ID>                            # Continue a thread"
)]
pub struct Args {
    /// Send a single prompt (omit for interactive mode)
    #[clap(short, long, value_name = "MESSAGE")]
    prompt: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,

    /// Continue an existing thread: the ID of any task in it
    #[clap(long, value_name = "ID")]
    thread_id: Option<String>,

    /// Service to scope the chat to (name or ID)
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to use (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// List existing agent threads for the current environment
    #[clap(long, conflicts_with_all = ["prompt", "thread_id", "service"])]
    list: bool,

    /// Accept that a new thread starts a billed cloud agent VM, without
    /// being asked
    #[clap(long)]
    accept: bool,
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
    let backboard = configs.get_backboard();

    if args.list {
        return task::show_tasks(
            &client,
            &backboard,
            environment_id,
            None,
            None,
            20,
            args.json,
        )
        .await;
    }

    // The agent can see the whole environment, so a service only narrows what
    // the conversation is about: use the one named or linked, never ask.
    let service = match args.service.as_deref() {
        Some(selector) => Some(
            project
                .services
                .edges
                .iter()
                .find(|edge| edge.node.name == selector || edge.node.id == selector)
                .map(|edge| (edge.node.id.clone(), Some(edge.node.name.clone())))
                .with_context(|| format!("Service {selector} not found in this project"))?,
        ),
        None => linked_project.service.clone().map(|id| {
            let name = project
                .services
                .edges
                .iter()
                .find(|edge| edge.node.id == id)
                .map(|edge| edge.node.name.clone());
            (id, name)
        }),
    };

    let mut session = Session {
        client: &client,
        backboard,
        project_id,
        environment_id,
        service,
        thread: args.thread_id.clone(),
        json: args.json,
        // Only a person at a terminal can answer the agent mid-task.
        interactive: args.prompt.is_none(),
    };

    match args.prompt {
        Some(message) => {
            if session.thread.is_none() {
                task::confirm_new_agent(args.accept, NEW_SESSION_NOTICE)?;
            }
            let turn = session.turn(message).await?;
            session.render(&turn)?;
            match task::text(&turn, "status") {
                Some("FAILED") => bail!(
                    "The agent failed: {}",
                    task::text(&turn, "error").unwrap_or("no reason given")
                ),
                Some("AWAITING_INPUT") => bail!(
                    "The agent is waiting on an answer. Reply with `railway agent --thread-id {} -p \"<answer>\"`.",
                    session.thread.as_deref().unwrap_or_default()
                ),
                _ => Ok(()),
            }
        }
        None => session.repl(args.accept).await,
    }
}

struct Session<'a> {
    client: &'a reqwest::Client,
    backboard: String,
    project_id: String,
    environment_id: String,
    /// The service the conversation is scoped to: its ID and, when known, name.
    service: Option<(String, Option<String>)>,
    /// The latest task in the thread. The next message replies to it, which
    /// continues its agent and session.
    thread: Option<String>,
    json: bool,
    interactive: bool,
}

impl Session<'_> {
    /// Send one message and wait for the agent to answer, ask, or fail.
    async fn turn(&mut self, message: String) -> Result<Value> {
        let input = cloud_agent_task_dispatch_input(self, message);
        let handle = task::dispatch(self.client, &self.backboard, input).await?;
        self.thread = Some(handle.task_id.clone());

        let mut answered: HashSet<String> = HashSet::new();
        let mut settling = 0;
        loop {
            let turn = task::wait(
                self.client,
                &self.backboard,
                &handle.agent_id,
                &handle.session_id,
                &handle.task_id,
                !self.json,
            )
            .await?;
            if task::text(&turn, "status") != Some("AWAITING_INPUT") {
                return Ok(turn);
            }

            let open: Vec<Value> = task::pending(&turn)
                .into_iter()
                .filter(|request| !answered.contains(request_id(request)))
                .collect();
            if open.is_empty() {
                // An approval just sent has not registered yet.
                settling += 1;
                if settling > ANSWER_SETTLE_POLLS {
                    return Ok(turn);
                }
                tokio::time::sleep(task::POLL_INTERVAL).await;
                continue;
            }
            settling = 0;

            // An approval is a yes or no the person can give right here. A
            // question goes back to the caller, whose next message answers it.
            let approvals: Vec<&Value> = open.iter().filter(|r| is_approval(r)).collect();
            if !self.interactive || approvals.is_empty() {
                return Ok(turn);
            }
            for request in approvals {
                let what = task::text(request, "message")
                    .or_else(|| task::text(request, "tool"))
                    .unwrap_or("an action");
                println!("{} {what}", "The agent wants approval:".yellow().bold());
                let allow = prompt_confirm_with_default("Allow?", true)?;
                let id = request_id(request).to_string();
                task::answer(
                    self.client,
                    &self.backboard,
                    &handle.task_id,
                    id.clone(),
                    if allow { Answer::Allow } else { Answer::Deny },
                    None,
                )
                .await?;
                answered.insert(id);
            }
        }
    }

    fn render(&self, turn: &Value) -> Result<()> {
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json_response(turn, self.thread.as_deref()))?
            );
            return Ok(());
        }
        match task::text(turn, "status") {
            Some("FAILED") => {}
            Some("AWAITING_INPUT") => {
                println!("{}", "Railway Agent:".purple().bold());
                for request in task::pending(turn) {
                    println!(
                        "{}",
                        task::text(&request, "message").unwrap_or("(the agent asked a question)")
                    );
                }
            }
            _ => {
                println!("{}", "Railway Agent:".purple().bold());
                let answer = task::text(turn, "text").unwrap_or("(no response)");
                if std::io::stdout().is_terminal() {
                    termimad::MadSkin::default().print_text(answer);
                } else {
                    println!("{answer}");
                }
            }
        }
        Ok(())
    }

    async fn repl(&mut self, accepted: bool) -> Result<()> {
        interact_or!(
            "Interactive mode requires a terminal. Use `railway agent -p \"your message\"` for non-interactive use."
        );

        match self.thread.clone() {
            Some(thread) => self.show_resumed(&thread).await,
            None => task::confirm_new_agent(accepted, NEW_SESSION_NOTICE)?,
        }

        println!(
            "{}",
            "Railway Agent (type 'exit' or Ctrl+C to quit)".dimmed()
        );
        println!();

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

            println!();
            // A failed turn ends the turn, not the conversation.
            match self.turn(message).await {
                Ok(turn) => {
                    self.render(&turn)?;
                    if task::text(&turn, "status") == Some("FAILED") {
                        eprintln!(
                            "{}: {}",
                            "Error".red().bold(),
                            task::text(&turn, "error").unwrap_or("the agent failed")
                        );
                    }
                }
                Err(error) => eprintln!("{}: {error:#}", "Error".red().bold()),
            }
            println!();
        }

        if let Some(thread) = &self.thread {
            println!(
                "{}",
                format!("Continue later with `railway agent --thread-id {thread}`.").dimmed()
            );
        }
        Ok(())
    }

    /// Show where a resumed thread left off. Best effort: a thread that cannot
    /// be read can still be continued.
    async fn show_resumed(&self, thread: &str) {
        println!("{}", format!("── resuming thread {thread} ──").dimmed());
        let Ok(record) =
            task::find_task(self.client, &self.backboard, &self.environment_id, thread).await
        else {
            return;
        };
        if let Some(prompt) = task::text(&record, "promptPreview") {
            println!("{}\n{prompt}", "You:".bold());
        }
        if let Some(answer) = task::text(&record, "text") {
            println!("{}", "Railway Agent:".purple().bold());
            termimad::MadSkin::default().print_text(answer);
        }
        println!("{}", "── end of history ──".dimmed());
    }
}

fn cloud_agent_task_dispatch_input(
    session: &Session<'_>,
    message: String,
) -> crate::gql::mutations::cloud_agent_task_dispatch::CloudAgentTaskDispatchInput {
    crate::gql::mutations::cloud_agent_task_dispatch::CloudAgentTaskDispatchInput {
        reply_to_task_id: session.thread.clone(),
        interactive: Some(session.interactive),
        context: service_context(session.service.as_ref()),
        ..task::blank_input(
            session.project_id.clone(),
            session.environment_id.clone(),
            message,
        )
    }
}

/// `--service` as background for the agent: what the conversation is about,
/// not an instruction.
fn service_context(service: Option<&(String, Option<String>)>) -> Option<Value> {
    service.map(|(id, name)| {
        let named = match name {
            Some(name) => format!("\"{name}\" (ID {id})"),
            None => format!("with ID {id}"),
        };
        json!([{
            "type": "service",
            "serviceId": id,
            "text": format!("The user scoped this conversation to the Railway service {named}."),
        }])
    })
}

fn request_id(request: &Value) -> &str {
    task::text(request, "requestId").unwrap_or_default()
}

fn is_approval(request: &Value) -> bool {
    request["actions"]
        .as_array()
        .is_some_and(|actions| actions.iter().any(|action| action == "ALLOW"))
}

/// The `--json` shape for one turn. `threadId` is what `--thread-id` takes to
/// continue.
fn json_response(turn: &Value, thread: Option<&str>) -> Value {
    let status = task::text(turn, "status").unwrap_or("UNKNOWN");
    let mut out = json!({
        "threadId": thread,
        "status": status,
        "response": task::text(turn, "text").unwrap_or_default(),
        "toolCalls": turn["progress"]["tools"]
            .as_array()
            .map(|tools| tools.iter().map(|tool| json!({ "toolName": tool })).collect::<Vec<_>>())
            .unwrap_or_default(),
    });
    if let Some(error) = task::text(turn, "error") {
        out["error"] = json!(error);
    }
    if status == "AWAITING_INPUT" {
        out["questions"] = json!(task::pending(turn));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_response_keeps_the_old_keys() {
        let turn = json!({
            "status": "COMPLETED",
            "text": "all good",
            "progress": { "steps": 2, "tools": ["get-logs"] }
        });
        let out = json_response(&turn, Some("t1"));
        assert_eq!(out["threadId"], "t1");
        assert_eq!(out["response"], "all good");
        assert_eq!(out["toolCalls"][0]["toolName"], "get-logs");
        assert!(out.get("error").is_none());
        assert!(out.get("questions").is_none());
    }

    #[test]
    fn json_response_carries_failures_and_questions() {
        let failed = json_response(&json!({ "status": "FAILED", "error": "boom" }), None);
        assert_eq!(failed["error"], "boom");
        assert_eq!(failed["response"], "");

        let asking = json_response(
            &json!({
                "status": "AWAITING_INPUT",
                "pendingInteractions": [{ "requestId": "r1", "message": "Which service?" }]
            }),
            Some("t1"),
        );
        assert_eq!(asking["questions"][0]["requestId"], "r1");
    }

    #[test]
    fn approvals_are_told_apart_from_questions() {
        assert!(is_approval(&json!({ "actions": ["ALLOW", "DENY"] })));
        assert!(!is_approval(&json!({ "actions": ["ACCEPT", "DECLINE"] })));
        assert!(!is_approval(&json!({})));
    }

    #[test]
    fn service_scope_is_sent_as_context() {
        assert!(service_context(None).is_none());
        let service = ("svc_1".to_string(), Some("api".to_string()));
        let context = service_context(Some(&service)).unwrap();
        assert_eq!(context[0]["type"], "service");
        assert_eq!(context[0]["serviceId"], "svc_1");
        assert!(context[0]["text"].as_str().unwrap().contains("\"api\""));
    }
}
