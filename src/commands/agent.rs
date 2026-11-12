//! `railway agent`: ask the Railway Agent something, from the terminal.
//!
//! Each message is a task run on a cloud agent (see `railway ca task`). Every
//! conversation in an environment shares one agent, named `railway-agent`:
//! the first conversation creates it, which is billed, so the command says so
//! and asks first. A thread is one session on that agent, and its session ID
//! is the thread ID, which stays the same for as long as the thread lives.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use is_terminal::IsTerminal;
use serde_json::{Value, json};

use crate::{
    commands::{
        cloud_agent::task::{self, Answer, Interrupted},
        sandbox::resolve_project_and_env,
    },
    controllers::{cloud_agent as ca, project::get_project},
    gql::mutations::cloud_agent_task_dispatch::CloudAgentTaskDispatchInput,
    interact_or,
    util::prompt::prompt_confirm_with_default,
};

use super::*;

/// The cloud agent every `railway agent` thread in an environment runs on.
const AGENT_NAME: &str = "railway-agent";

/// What the user accepts before the first thread in an environment creates
/// that agent.
const NEW_AGENT_NOTICE: &str = "Railway Agent runs on a cloud agent. This will start a cloud agent VM named railway-agent in this environment and use Railway Agent credits. The VM is billed while it is active and sleeps after five minutes idle. Later sessions reuse it.";

/// How many extra polls to give an approval to register before handing the
/// task back as still waiting.
const ANSWER_SETTLE_POLLS: usize = 20;

/// How many earlier turns a resumed thread shows.
const RESUME_TURNS: usize = 3;

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

    /// Continue an existing thread (see --list)
    #[clap(long, value_name = "ID")]
    thread_id: Option<String>,

    /// Service to scope the chat to (name or ID)
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to use (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID (defaults to the linked project)
    #[clap(long)]
    project: Option<String>,

    /// List existing agent threads for the current environment
    #[clap(long, conflicts_with_all = ["prompt", "thread_id", "service"])]
    list: bool,

    /// Accept that the first thread in an environment starts a billed cloud
    /// agent VM, without being asked
    #[clap(long, short = 'y', visible_alias = "yes")]
    accept: bool,
}

pub async fn command(args: Args) -> Result<()> {
    if let Some(prompt) = &args.prompt {
        if prompt.trim().is_empty() {
            bail!("The prompt is empty. Say what you want to ask.");
        }
    }

    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (project_id, environment_id) = resolve_project_and_env(
        &mut configs,
        &client,
        args.project.clone(),
        args.environment.clone(),
    )
    .await?;
    let backboard = configs.get_backboard();

    let agent = shared_agent(&client, &backboard, &environment_id).await?;

    if args.list {
        let records = match &agent {
            Some(agent) => {
                task::recent_tasks(&client, &backboard, &environment_id, Some(&agent.id)).await?
            }
            None => Vec::new(),
        };
        return render_threads(&threads(&records), args.json);
    }

    // The agent can see the whole environment, so a service only narrows what
    // the conversation is about: use the one named or linked, never ask.
    let project = get_project(&client, &configs, project_id.clone()).await?;
    let named = |id: &str| {
        project
            .services
            .edges
            .iter()
            .find(|edge| edge.node.id == id)
            .map(|edge| edge.node.name.clone())
    };
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
        None => configs
            .get_linked_project()
            .await
            .ok()
            .filter(|linked| linked.project == project_id)
            .and_then(|linked| linked.service)
            .map(|id| {
                let name = named(&id);
                (id, name)
            }),
    };

    // A thread to continue is found before anything is asked or sent, so a
    // mistyped ID is an error here rather than a surprise later.
    let mut history = Vec::new();
    let (agent_id, session_id) = match args.thread_id.as_deref() {
        Some(thread) => {
            let records = task::recent_tasks(&client, &backboard, &environment_id, None).await?;
            let (agent_id, session_id) = locate_thread(&records, thread).with_context(|| {
                format!(
                    "No thread {thread} in this environment. `railway agent --list` shows yours."
                )
            })?;
            history = records
                .into_iter()
                .filter(|record| task::text(record, "sessionId") == Some(&session_id))
                .collect();
            history.reverse();
            (Some(agent_id), Some(session_id))
        }
        None => (agent.map(|agent| agent.id), None),
    };

    let mut session = Session {
        client: &client,
        backboard,
        project_id,
        environment_id,
        service,
        agent_id,
        session_id,
        json: args.json,
        // Only a person at a terminal can answer the agent mid-task.
        interactive: args.prompt.is_none(),
    };

    match args.prompt {
        Some(message) => {
            if session.agent_id.is_none() {
                task::confirm_new_agent(args.accept, NEW_AGENT_NOTICE)?;
            }
            let turn = match session.turn(message).await {
                Ok(turn) => turn,
                Err(error) if error.is::<Interrupted>() => {
                    eprintln!("{}", session.interrupted_hint());
                    std::process::exit(130);
                }
                Err(error) => return Err(error),
            };
            session.render(&turn)?;
            let thread = session.session_id.as_deref().unwrap_or_default();
            match task::outcome(&turn) {
                "FAILED" => bail!(
                    "The agent failed: {}",
                    task::text(&turn, "error").unwrap_or("no reason given")
                ),
                "CANCELLED" => bail!("The request was cancelled."),
                "AWAITING_INPUT" => bail!(
                    "The agent is waiting on an answer. Reply with `railway agent --thread-id {thread} -p \"<answer>\"`."
                ),
                _ => Ok(()),
            }
        }
        None => session.repl(args.accept, &history).await,
    }
}

/// The environment's shared agent, if it has been created. Two first runs at
/// once can leave more than one; the oldest wins so every caller agrees.
async fn shared_agent(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
) -> Result<Option<ca::Agent>> {
    Ok(
        ca::list_in_environment(client, backboard, environment_id, true)
            .await?
            .into_iter()
            .filter(|agent| agent.name == AGENT_NAME)
            .min_by_key(|agent| agent.created_at),
    )
}

/// The agent and session a thread ID names. A task ID is accepted too, and
/// names the thread that task belongs to.
fn locate_thread(records: &[Value], thread: &str) -> Option<(String, String)> {
    records
        .iter()
        .find(|record| {
            task::text(record, "sessionId") == Some(thread)
                || task::text(record, "id") == Some(thread)
        })
        .and_then(|record| {
            Some((
                task::text(record, "cloudAgentId")?.to_string(),
                task::text(record, "sessionId")?.to_string(),
            ))
        })
}

/// One row of `--list`: a session and what happened on it.
#[derive(Debug, PartialEq)]
struct Thread {
    id: String,
    turns: usize,
    first_prompt: String,
    status: String,
    last_active: String,
}

/// Group task records (newest first) into threads, most recently active first.
fn threads(records: &[Value]) -> Vec<Thread> {
    let mut out: Vec<Thread> = Vec::new();
    for record in records {
        let Some(session) = task::text(record, "sessionId") else {
            continue;
        };
        let prompt = task::text(record, "promptPreview").unwrap_or_default();
        match out.iter_mut().find(|thread| thread.id == session) {
            // Records arrive newest first, so each later one is an earlier
            // turn: it is the thread's opening prompt until an older shows up.
            Some(thread) => {
                thread.turns += 1;
                thread.first_prompt = prompt.to_string();
            }
            None => out.push(Thread {
                id: session.to_string(),
                turns: 1,
                first_prompt: prompt.to_string(),
                status: task::outcome(record).to_string(),
                last_active: task::text(record, "createdAt")
                    .unwrap_or_default()
                    .to_string(),
            }),
        }
    }
    out
}

fn render_threads(threads: &[Thread], as_json: bool) -> Result<()> {
    if as_json {
        let out: Vec<Value> = threads
            .iter()
            .map(|thread| {
                json!({
                    "threadId": thread.id,
                    "turns": thread.turns,
                    "status": thread.status,
                    "lastActiveAt": thread.last_active,
                    "firstPrompt": thread.first_prompt,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if threads.is_empty() {
        println!("No agent threads in this environment. Start one with `railway agent`.");
        return Ok(());
    }
    println!(
        "{:<18}  {:<5}  {:<8}  {}",
        "THREAD".dimmed(),
        "TURNS".dimmed(),
        "ACTIVE".dimmed(),
        "STARTED WITH".dimmed()
    );
    for thread in threads {
        let active = DateTime::parse_from_rfc3339(&thread.last_active)
            .map(|at| ca::humanize_age(at.with_timezone(&Utc)))
            .unwrap_or_default();
        println!(
            "{:<18}  {:<5}  {:<8}  {}",
            thread.id,
            thread.turns,
            active,
            task::truncate(&thread.first_prompt, 60)
        );
    }
    println!(
        "\n{}",
        "Continue one with `railway agent --thread-id <THREAD>`.".dimmed()
    );
    Ok(())
}

struct Session<'a> {
    client: &'a reqwest::Client,
    backboard: String,
    project_id: String,
    environment_id: String,
    /// The service the conversation is scoped to: its ID and, when known, name.
    service: Option<(String, Option<String>)>,
    /// The shared agent, once it exists. `None` means the next message
    /// creates it.
    agent_id: Option<String>,
    /// The thread. `None` means the next message starts one.
    session_id: Option<String>,
    json: bool,
    interactive: bool,
}

impl Session<'_> {
    /// Send one message and wait for the agent to answer, ask, or fail.
    async fn turn(&mut self, message: String) -> Result<Value> {
        let input = CloudAgentTaskDispatchInput {
            cloud_agent_id: self.agent_id.clone(),
            session_id: self.session_id.clone(),
            // Only the message that creates the agent names it.
            agent_name: self.agent_id.is_none().then(|| AGENT_NAME.to_string()),
            interactive: Some(self.interactive),
            context: service_context(self.service.as_ref()),
            ..task::blank_input(
                self.project_id.clone(),
                self.environment_id.clone(),
                message,
            )
        };
        let handle = task::dispatch(self.client, &self.backboard, input)
            .await
            .map_err(|error| {
                if format!("{error:#}").contains("Cloud agent not found") {
                    anyhow::anyhow!(
                        "The agent behind this thread was deleted. Start a new thread with `railway agent`."
                    )
                } else {
                    task::explain(error, None)
                }
            })?;
        self.agent_id = Some(handle.agent_id.clone());
        self.session_id = Some(handle.session_id.clone());

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
            let approvals: Vec<&Value> = open.iter().filter(|r| task::is_approval(r)).collect();
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
                serde_json::to_string_pretty(&json_response(turn, self.session_id.as_deref()))?
            );
            return Ok(());
        }
        // The header is for a person reading a terminal. Piped, the output is
        // the answer and nothing else.
        let to_terminal = std::io::stdout().is_terminal();
        let header = || {
            if to_terminal {
                println!("{}", "Railway Agent:".purple().bold());
            }
        };
        match task::outcome(turn) {
            "FAILED" | "CANCELLED" => {}
            "AWAITING_INPUT" => {
                header();
                for request in task::pending(turn) {
                    println!(
                        "{}",
                        task::text(&request, "message").unwrap_or("(the agent asked a question)")
                    );
                }
            }
            _ => {
                header();
                let answer = task::text(turn, "text").unwrap_or("(no response)");
                if to_terminal {
                    termimad::MadSkin::default().print_text(answer);
                } else {
                    println!("{answer}");
                }
            }
        }
        Ok(())
    }

    fn interrupted_hint(&self) -> String {
        match &self.session_id {
            Some(thread) => format!(
                "Stopped waiting. The agent is still working. Pick the thread back up with: railway agent --thread-id {thread}"
            ),
            None => "Stopped waiting.".to_string(),
        }
    }

    async fn repl(&mut self, accepted: bool, history: &[Value]) -> Result<()> {
        interact_or!(
            "Interactive mode requires a terminal. Use `railway agent -p \"your message\"` for non-interactive use."
        );

        if self.agent_id.is_none() {
            task::confirm_new_agent(accepted, NEW_AGENT_NOTICE)?;
        }
        if let Some(thread) = &self.session_id {
            show_resumed(thread, history);
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
            // A failed or abandoned turn ends the turn, not the conversation.
            match self.turn(message).await {
                Ok(turn) => {
                    self.render(&turn)?;
                    match task::outcome(&turn) {
                        "FAILED" => eprintln!(
                            "{}: {}",
                            "Error".red().bold(),
                            task::text(&turn, "error").unwrap_or("the agent failed")
                        ),
                        "CANCELLED" => eprintln!("{}", "The request was cancelled.".yellow()),
                        _ => {}
                    }
                }
                Err(error) if error.is::<Interrupted>() => eprintln!(
                    "{}",
                    "Stopped waiting. The agent is still working on that.".yellow()
                ),
                Err(error) => eprintln!("{}: {error:#}", "Error".red().bold()),
            }
            println!();
        }

        if let Some(thread) = &self.session_id {
            println!(
                "{}",
                format!("Continue later with `railway agent --thread-id {thread}`.").dimmed()
            );
        }
        Ok(())
    }
}

/// Show where a resumed thread left off: its last few turns, oldest first.
fn show_resumed(thread: &str, history: &[Value]) {
    println!("{}", format!("── resuming thread {thread} ──").dimmed());
    let skin = termimad::MadSkin::default();
    for record in history
        .iter()
        .skip(history.len().saturating_sub(RESUME_TURNS))
    {
        if let Some(prompt) = task::text(record, "promptPreview") {
            println!("{}\n{prompt}", "You:".bold());
        }
        if let Some(answer) = task::text(record, "text").filter(|text| !text.is_empty()) {
            println!("{}", "Railway Agent:".purple().bold());
            skin.print_text(answer);
        }
    }
    println!("{}", "── end of history ──".dimmed());
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

/// The `--json` shape for one turn. `threadId` is what `--thread-id` takes to
/// continue.
fn json_response(turn: &Value, thread: Option<&str>) -> Value {
    let status = task::outcome(turn);
    let mut out = json!({
        "threadId": thread,
        "taskId": task::text(turn, "taskId"),
        "status": status,
        "response": task::text(turn, "text").unwrap_or_default(),
        "toolCalls": turn["progress"]["tools"]
            .as_array()
            .map(|tools| tools.iter().map(|tool| json!({ "toolName": tool })).collect::<Vec<_>>())
            .unwrap_or_default(),
    });
    if let Some(error) = task::text(turn, "error").filter(|_| status == "FAILED") {
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
            "taskId": "t1",
            "text": "all good",
            "progress": { "steps": 2, "tools": ["get-logs"] }
        });
        let out = json_response(&turn, Some("task-abc"));
        assert_eq!(out["threadId"], "task-abc");
        assert_eq!(out["taskId"], "t1");
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

        let cancelled = json_response(
            &json!({ "status": "FAILED", "error": "The task was cancelled" }),
            None,
        );
        assert_eq!(cancelled["status"], "CANCELLED");
        assert!(cancelled.get("error").is_none());

        let asking = json_response(
            &json!({
                "status": "AWAITING_INPUT",
                "pendingInteractions": [{ "requestId": "r1", "message": "Which service?" }]
            }),
            Some("task-abc"),
        );
        assert_eq!(asking["questions"][0]["requestId"], "r1");
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

    fn record(id: &str, session: &str, prompt: &str, at: &str) -> Value {
        json!({
            "id": id,
            "sessionId": session,
            "cloudAgentId": "agent-1",
            "promptPreview": prompt,
            "status": "COMPLETED",
            "createdAt": at,
        })
    }

    #[test]
    fn tasks_group_into_threads_by_session() {
        // Newest first, as the API returns them.
        let records = vec![
            record("t4", "task-b", "and then?", "2026-01-01T00:04:00Z"),
            record("t3", "task-a", "follow up", "2026-01-01T00:03:00Z"),
            record("t2", "task-b", "second thread", "2026-01-01T00:02:00Z"),
            record("t1", "task-a", "first thread", "2026-01-01T00:01:00Z"),
            json!({ "id": "t0", "status": "FAILED" }),
        ];
        let threads = threads(&records);
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].id, "task-b");
        assert_eq!(threads[0].turns, 2);
        assert_eq!(threads[0].first_prompt, "second thread");
        assert_eq!(threads[0].last_active, "2026-01-01T00:04:00Z");
        assert_eq!(threads[1].first_prompt, "first thread");
    }

    #[test]
    fn a_thread_is_found_by_session_or_task_id() {
        let records = vec![record("t1", "task-a", "hi", "2026-01-01T00:01:00Z")];
        let found = Some(("agent-1".to_string(), "task-a".to_string()));
        assert_eq!(locate_thread(&records, "task-a"), found);
        assert_eq!(locate_thread(&records, "t1"), found);
        assert_eq!(locate_thread(&records, "nope"), None);
    }
}
