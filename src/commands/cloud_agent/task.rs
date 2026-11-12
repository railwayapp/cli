//! `railway ca task`: hand a piece of work to a cloud agent and get the result
//! back, without connecting to it.
//!
//! A task is one request and its result. `run` dispatches it, and unless told
//! otherwise waits for the outcome; the task itself lives on Railway, so
//! leaving the wait (Ctrl-C, a closed terminal) never stops the work.
//!
//! A task with no target starts a new cloud agent VM, which is billed. `run`
//! says so and asks first; `--accept` accepts ahead of time for scripts.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use chrono::{DateTime, Utc};
use clap::{Parser, ValueEnum};
use colored::Colorize;
use is_terminal::IsTerminal;
use serde_json::Value;

use crate::client::{GQLClient, post_graphql};
use crate::commands::sandbox::resolve_project_and_env;
use crate::config::Configs;
use crate::controllers::cloud_agent as ca;
use crate::gql::mutations::{
    self, cloud_agent_task_cancel, cloud_agent_task_dispatch, cloud_agent_task_respond,
};
use crate::gql::queries::{self, cloud_agent_task, cloud_agent_task_models, cloud_agent_tasks};
use crate::util::progress::create_spinner;
use crate::util::prompt::prompt_confirm_with_default;

/// What the user accepts before `run` starts a new cloud agent.
const NEW_AGENT_NOTICE: &str = "This will start a new cloud agent VM in this environment and use Railway Agent credits to run the task. The VM is billed while it is active and sleeps after five minutes idle.";

/// The same, for a conversation key: only its first task starts an agent, and
/// whether this is the first cannot be told from here.
const CONVERSATION_NOTICE: &str = "If this is the first task on this conversation, it will start a new cloud agent VM in this environment. Either way it uses Railway Agent credits. The VM is billed while it is active and sleeps after five minutes idle.";

/// How the API reports a cancelled task until it has a status of its own.
const CANCELLED_ERROR: &str = "The task was cancelled";

/// How often a waiting command asks for the task's state.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// How many pages of an environment's tasks `find_task` reads before giving up.
const FIND_TASK_PAGES: usize = 10;

/// Run and manage cloud agent tasks
#[derive(Parser)]
#[clap(after_help = r#"Examples:
  railway ca task run "Fix the failing test in api/ and open a PR" --repo acme/api
  railway ca task run "Now add a changelog entry" --conversation release-42
  railway ca task run "Summarize yesterday's errors" --accept --json
  railway ca task status <TASK_ID> --wait
  railway ca task list --status running
  railway ca task respond <TASK_ID> allow
  railway ca task cancel <TASK_ID>"#)]
pub struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Parser)]
enum Command {
    /// Hand a task to a cloud agent
    Run(Box<RunArgs>),
    /// Show a task's state and result
    #[clap(visible_alias = "check")]
    Status(StatusArgs),
    /// List tasks in an environment, newest first
    #[clap(visible_alias = "ls")]
    List(ListArgs),
    /// Answer a question or approval a task is waiting on
    Respond(RespondArgs),
    /// Cancel a task in flight
    Cancel(CancelArgs),
    /// List the models a task can run on
    Models(ModelsArgs),
}

/// Where to look. Tasks belong to an environment, so every verb needs one.
#[derive(Parser)]
struct TargetArgs {
    /// Environment name or ID (defaults to the linked environment)
    #[clap(long, short)]
    environment: Option<String>,

    /// Project ID (defaults to the linked project)
    #[clap(long, short)]
    project: Option<String>,
}

#[derive(Parser)]
struct RunArgs {
    /// What the agent should do
    prompt: Option<String>,

    /// Run on this existing agent (name or ID) instead of starting a new one.
    /// Each run is a fresh session: use --reply-to to keep an earlier task's
    /// context
    #[clap(long, value_name = "AGENT", conflicts_with_all = ["reply_to", "conversation"])]
    agent: Option<String>,

    /// Continue this session on --agent
    #[clap(long, value_name = "ID", requires = "agent")]
    session: Option<String>,

    /// Continue the agent and session an earlier task ran on
    #[clap(long, value_name = "TASK_ID", conflicts_with = "conversation")]
    reply_to: Option<String>,

    /// Your own key for a conversation. The first task with a key starts an
    /// agent; later tasks with the same key continue it
    #[clap(long, value_name = "KEY")]
    conversation: Option<String>,

    /// Name for a newly created agent
    #[clap(long, value_name = "NAME")]
    name: Option<String>,

    /// GitHub owner/repo to clone into a new agent
    #[clap(long, value_name = "OWNER/REPO")]
    repo: Option<String>,

    /// Branch of --repo to check out
    #[clap(long, requires = "repo")]
    branch: Option<String>,

    /// Service whose source a new agent starts from (ID)
    #[clap(long, value_name = "SERVICE_ID")]
    service: Option<String>,

    /// Model to run on (see `railway ca task models`). Fixed when a
    /// conversation starts
    #[clap(long)]
    model: Option<String>,

    /// Reasoning effort, one of the model's supported values
    #[clap(long)]
    effort: Option<String>,

    /// Image the agent sees with the prompt: a file path or an https URL.
    /// Repeat for up to four
    #[clap(long = "image", value_name = "PATH_OR_URL")]
    images: Vec<String>,

    /// File holding a JSON Schema; the result then carries matching
    /// structured output
    #[clap(long, value_name = "FILE")]
    output_schema: Option<std::path::PathBuf>,

    /// Resending a request with the same key returns the original task
    /// instead of starting a second one
    #[clap(long, value_name = "KEY")]
    idempotency_key: Option<String>,

    /// Return as soon as the task is dispatched instead of waiting for it
    #[clap(long)]
    no_wait: bool,

    /// Let the agent ask you a question and wait for the answer, instead of
    /// deciding on its own
    #[clap(long)]
    interactive: bool,

    /// Accept that this may start a new, billed cloud agent VM, without
    /// being asked
    #[clap(long, short = 'y', visible_alias = "yes")]
    accept: bool,

    /// Output as JSON
    #[clap(long)]
    json: bool,

    #[clap(flatten)]
    target: TargetArgs,
}

#[derive(Parser)]
struct StatusArgs {
    /// Task ID
    task: String,

    /// Wait until the task finishes or needs an answer
    #[clap(long)]
    wait: bool,

    /// Output as JSON
    #[clap(long)]
    json: bool,

    #[clap(flatten)]
    target: TargetArgs,
}

#[derive(Clone, Copy, ValueEnum)]
enum StatusFilter {
    Pending,
    Running,
    AwaitingInput,
    Completed,
    Failed,
}

#[derive(Parser)]
struct ListArgs {
    /// Only tasks in this state
    #[clap(long, value_enum)]
    status: Option<StatusFilter>,

    /// Only tasks that ran on this agent (name or ID)
    #[clap(long, value_name = "AGENT")]
    agent: Option<String>,

    /// How many tasks to show (1-100)
    #[clap(long, default_value_t = 20, value_parser = clap::value_parser!(i64).range(1..=100))]
    limit: i64,

    /// Output as JSON
    #[clap(long)]
    json: bool,

    #[clap(flatten)]
    target: TargetArgs,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Answer {
    /// Approve a tool call or permission request
    Allow,
    /// Refuse a tool call or permission request
    Deny,
    /// Submit a form, with values from --field
    Accept,
    /// Decline to fill in a form
    Decline,
    /// Dismiss the request
    Cancel,
}

#[derive(Parser)]
struct RespondArgs {
    /// Task ID
    task: String,

    /// Your answer
    #[clap(value_enum)]
    answer: Answer,

    /// Which request to answer. Needed only when the task has more than one
    /// pending
    #[clap(long, value_name = "ID")]
    request: Option<String>,

    /// Form value for `accept`, as name=value. Repeat for each field
    #[clap(long = "field", value_name = "NAME=VALUE")]
    fields: Vec<String>,

    /// Output as JSON
    #[clap(long)]
    json: bool,

    #[clap(flatten)]
    target: TargetArgs,
}

#[derive(Parser)]
struct CancelArgs {
    /// Task ID
    task: String,

    /// Output as JSON
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct ModelsArgs {
    /// Output as JSON
    #[clap(long)]
    json: bool,

    #[clap(flatten)]
    target: TargetArgs,
}

/// The telemetry name for a verb, so the dispatcher can time it.
pub fn kind(args: &Args) -> &'static str {
    match args.command {
        Command::Run(_) => "task_run",
        Command::Status(_) => "task_status",
        Command::List(_) => "task_list",
        Command::Respond(_) => "task_respond",
        Command::Cancel(_) => "task_cancel",
        Command::Models(_) => "task_models",
    }
}

pub async fn command(args: Args) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    match args.command {
        Command::Run(a) => run(&mut configs, &client, *a).await,
        Command::Status(a) => status(&mut configs, &client, a).await,
        Command::List(a) => list(&mut configs, &client, a).await,
        Command::Respond(a) => respond(&mut configs, &client, a).await,
        Command::Cancel(a) => cancel(&configs, &client, a).await,
        Command::Models(a) => models(&mut configs, &client, a).await,
    }
}

/// Ask before starting a new, billed cloud agent. `accepted` is the caller's
/// pre-acceptance. With nobody to ask, refuse rather than assume.
pub(crate) fn confirm_new_agent(accepted: bool, notice: &str) -> Result<()> {
    if accepted {
        return Ok(());
    }
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        bail!("{notice} Pass --accept to proceed.");
    }
    let width = crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
    println!("{}", textwrap::fill(notice, width.clamp(40, 100)));
    if !prompt_confirm_with_default("Continue?", true)? {
        bail!("Cancelled. No agent was started.");
    }
    Ok(())
}

async fn run(configs: &mut Configs, client: &reqwest::Client, args: RunArgs) -> Result<()> {
    // Everything that can be wrong with the request itself is checked before
    // anything is asked or sent.
    let prompt = required_prompt(args.prompt.as_deref(), args.target.project.as_deref())?;
    let images = args
        .images
        .iter()
        .map(|image| image_input(image))
        .collect::<Result<Vec<_>>>()?;
    if images.len() > 4 {
        bail!("A task takes at most four images.");
    }
    let output_schema = match &args.output_schema {
        Some(path) => {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("Could not read {}", path.display()))?;
            Some(
                serde_json::from_str::<Value>(&raw)
                    .with_context(|| format!("{} is not valid JSON", path.display()))?,
            )
        }
        None => None,
    };

    let (project_id, environment_id) = resolve_project_and_env(
        configs,
        client,
        args.target.project.clone(),
        args.target.environment.clone(),
    )
    .await?;
    let backboard = configs.get_backboard();

    let cloud_agent_id = match args.agent.as_deref() {
        Some(selector) => Some(
            ca::resolve(configs, client, Some(selector), Some(&environment_id))
                .await?
                .0
                .id,
        ),
        None => None,
    };

    // A task with no target is certain to start an agent. A conversation key
    // starts one only on its first use.
    if args.conversation.is_some() {
        confirm_new_agent(args.accept, CONVERSATION_NOTICE)?;
    } else if cloud_agent_id.is_none() && args.reply_to.is_none() {
        confirm_new_agent(args.accept, NEW_AGENT_NOTICE)?;
    }
    let source = (args.repo.is_some() || args.service.is_some()).then(|| {
        cloud_agent_task_dispatch::CloudAgentSourceInput {
            repo: args.repo.clone(),
            branch: args.branch.clone(),
            service_id: args.service.clone(),
        }
    });

    let input = cloud_agent_task_dispatch::CloudAgentTaskDispatchInput {
        cloud_agent_id,
        session_id: args.session.clone(),
        reply_to_task_id: args.reply_to.clone(),
        conversation_key: args.conversation.clone(),
        agent_name: args.name.clone(),
        source,
        model: args.model.clone(),
        reasoning_effort: args.effort.clone(),
        images: (!images.is_empty()).then_some(images),
        output_schema,
        idempotency_key: args.idempotency_key.clone(),
        interactive: args.interactive.then_some(true),
        ..blank_input(project_id, environment_id, prompt)
    };

    let handle = dispatch(client, &backboard, input)
        .await
        .map_err(|error| explain(error, args.reply_to.as_deref()))?;

    if args.no_wait {
        if args.json {
            println!("{}", serde_json::to_string_pretty(&handle.raw)?);
        } else {
            println!("Task {} dispatched.", handle.task_id.cyan());
            println!(
                "{}",
                format!(
                    "Check on it with `railway ca task status {}`.",
                    handle.task_id
                )
                .dimmed()
            );
        }
        return Ok(());
    }

    if !args.json {
        eprintln!(
            "{}",
            format!(
                "Task {} is running. Ctrl-C stops waiting, not the task.",
                handle.task_id
            )
            .dimmed()
        );
    }
    let result = wait(
        client,
        &backboard,
        &handle.agent_id,
        &handle.session_id,
        &handle.task_id,
        !args.json,
    )
    .await;
    finish(&or_exit_on_interrupt(result)?, args.json)
}

/// The prompt, or why there isn't a usable one. `-p` is `--prompt` on
/// `railway agent` and `--project` here, so a prompt that landed in
/// `--project` gets pointed out rather than reported as a missing argument.
fn required_prompt(prompt: Option<&str>, project: Option<&str>) -> Result<String> {
    match prompt.map(str::trim) {
        Some(prompt) if !prompt.is_empty() => Ok(prompt.to_string()),
        Some(_) => bail!("The prompt is empty. Say what the agent should do."),
        None => match project {
            Some(project) if project.contains(char::is_whitespace) => bail!(
                "On `railway ca task`, -p is --project. Pass the prompt as an argument: railway ca task run \"{project}\""
            ),
            _ => bail!("A prompt is required: railway ca task run \"<what the agent should do>\""),
        },
    }
}

/// Rewrite the API's wording for the errors a person can act on.
pub(crate) fn explain(error: anyhow::Error, replied_to: Option<&str>) -> anyhow::Error {
    let message = format!("{error:#}");
    if message.contains("Reply task not found") {
        return anyhow::anyhow!(
            "No task {} in this environment. Pass --environment if it ran somewhere else.",
            replied_to.unwrap_or("with that ID")
        );
    }
    if message.contains("Cloud agent not found") {
        return anyhow::anyhow!(
            "The cloud agent this would run on no longer exists. Start a new one by leaving out the target."
        );
    }
    if message.contains("cloudAgentTaskModels") {
        return anyhow::anyhow!(
            "{}",
            message.replace(
                "list the available ones with cloudAgentTaskModels",
                "list the available ones with `railway ca task models`"
            )
        );
    }
    if message.contains("reasoningEffort must be") {
        return anyhow::anyhow!("{}", message.replace("reasoningEffort", "--effort"));
    }
    error
}

/// Ctrl-C during a wait leaves the task running. Say so, with the way back,
/// and exit the way an interrupted command does.
fn or_exit_on_interrupt(result: Result<Value>) -> Result<Value> {
    match result {
        Err(error) => match error.downcast_ref::<Interrupted>() {
            Some(interrupted) => {
                eprintln!(
                    "Stopped waiting. The task is still running: railway ca task status {} --wait",
                    interrupted.task_id
                );
                std::process::exit(130);
            }
            None => Err(error),
        },
        ok => ok,
    }
}

/// A wait ended by Ctrl-C. The task it was waiting on is still running.
#[derive(Debug)]
pub(crate) struct Interrupted {
    pub task_id: String,
}

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Stopped waiting on task {}", self.task_id)
    }
}

impl std::error::Error for Interrupted {}

/// Where a dispatched task landed.
pub(crate) struct Handle {
    pub task_id: String,
    pub agent_id: String,
    pub session_id: String,
    /// The API's handle as returned, for `--json`.
    raw: Value,
}

/// A dispatch input with only the required fields set, to build on with
/// struct update syntax.
pub(crate) fn blank_input(
    project_id: String,
    environment_id: String,
    prompt: String,
) -> cloud_agent_task_dispatch::CloudAgentTaskDispatchInput {
    cloud_agent_task_dispatch::CloudAgentTaskDispatchInput {
        project_id,
        environment_id,
        prompt,
        agent_name: None,
        cloud_agent_id: None,
        context: None,
        conversation_key: None,
        external_ref: None,
        idempotency_key: None,
        images: None,
        interactive: None,
        metadata: None,
        mode: None,
        model: None,
        output_schema: None,
        reasoning_effort: None,
        reply_to_task_id: None,
        session_id: None,
        source: None,
        webhook_progress: None,
        webhook_secret: None,
        webhook_url: None,
    }
}

pub(crate) async fn dispatch(
    client: &reqwest::Client,
    backboard: &str,
    input: cloud_agent_task_dispatch::CloudAgentTaskDispatchInput,
) -> Result<Handle> {
    let handle = post_graphql::<mutations::CloudAgentTaskDispatch, _>(
        client,
        backboard,
        cloud_agent_task_dispatch::Variables { input },
    )
    .await?
    .cloud_agent_task_dispatch;
    Ok(Handle {
        raw: serde_json::to_value(&handle)?,
        task_id: handle.task_id,
        agent_id: handle.cloud_agent_id,
        session_id: handle.session_id,
    })
}

async fn status(configs: &mut Configs, client: &reqwest::Client, args: StatusArgs) -> Result<()> {
    let environment_id = environment(configs, client, &args.target).await?;
    let backboard = configs.get_backboard();
    let record = find_task(client, &backboard, &environment_id, &args.task).await?;
    // A finished task's result is kept on its record, which stays readable
    // after the agent that ran it is gone.
    if finished(&record) {
        return finish(&record, args.json);
    }
    let (agent_id, session_id) = placement(&record)?;

    let result = if args.wait {
        or_exit_on_interrupt(
            wait(
                client,
                &backboard,
                &agent_id,
                &session_id,
                &args.task,
                !args.json,
            )
            .await,
        )?
    } else {
        fetch(client, &backboard, &agent_id, &session_id, &args.task).await?
    };
    finish(&result, args.json)
}

async fn list(configs: &mut Configs, client: &reqwest::Client, args: ListArgs) -> Result<()> {
    let environment_id = environment(configs, client, &args.target).await?;
    let backboard = configs.get_backboard();
    let cloud_agent_id = match args.agent.as_deref() {
        Some(selector) => Some(
            ca::resolve(configs, client, Some(selector), Some(&environment_id))
                .await?
                .0
                .id,
        ),
        None => None,
    };
    let status = args.status.map(|s| {
        use cloud_agent_tasks::CloudAgentTaskStatus as S;
        match s {
            StatusFilter::Pending => S::PENDING,
            StatusFilter::Running => S::RUNNING,
            StatusFilter::AwaitingInput => S::AWAITING_INPUT,
            StatusFilter::Completed => S::COMPLETED,
            StatusFilter::Failed => S::FAILED,
        }
    });

    show_tasks(
        client,
        &backboard,
        environment_id,
        status,
        cloud_agent_id,
        args.limit,
        args.json,
    )
    .await
}

/// Print one page of an environment's tasks, newest first.
pub(crate) async fn show_tasks(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: String,
    status: Option<cloud_agent_tasks::CloudAgentTaskStatus>,
    cloud_agent_id: Option<String>,
    limit: i64,
    as_json: bool,
) -> Result<()> {
    let page = post_graphql::<queries::CloudAgentTasks, _>(
        client,
        backboard,
        cloud_agent_tasks::Variables {
            environment_id,
            status,
            cloud_agent_id,
            limit: Some(limit),
            cursor: None,
        },
    )
    .await?
    .cloud_agent_tasks;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&page.tasks)?);
        return Ok(());
    }
    if page.tasks.is_empty() {
        println!("No tasks in this environment.");
        return Ok(());
    }

    println!(
        "{:<36}  {:<14}  {:<8}  {}",
        "ID".dimmed(),
        "STATUS".dimmed(),
        "AGE".dimmed(),
        "PROMPT".dimmed()
    );
    for task in serde_json::to_value(&page.tasks)?
        .as_array()
        .into_iter()
        .flatten()
    {
        let status = outcome(task);
        let age = text(task, "createdAt")
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
            .map(|at| ca::humanize_age(at.with_timezone(&Utc)))
            .unwrap_or_default();
        println!(
            "{:<36}  {:<14}  {:<8}  {}",
            text(task, "id").unwrap_or_default(),
            colorize_status(status),
            age,
            truncate(text(task, "promptPreview").unwrap_or_default(), 60)
        );
    }
    if page.next_cursor.is_some() {
        println!(
            "\n{}",
            "More tasks exist. Raise --limit to see them.".dimmed()
        );
    }
    Ok(())
}

async fn respond(configs: &mut Configs, client: &reqwest::Client, args: RespondArgs) -> Result<()> {
    let backboard = configs.get_backboard();

    let request_id = match args.request.clone() {
        Some(id) => id,
        None => {
            let environment_id = environment(configs, client, &args.target).await?;
            let record = find_task(client, &backboard, &environment_id, &args.task).await?;
            let (agent_id, session_id) = placement(&record)?;
            let task = fetch(client, &backboard, &agent_id, &session_id, &args.task).await?;
            let pending = pending(&task);
            match pending.as_slice() {
                [] => bail!("Task {} is not waiting on an answer.", args.task),
                [only] => text(only, "requestId").unwrap_or_default().to_string(),
                many => {
                    print_interactions(many);
                    bail!("This task has more than one request pending. Name one with --request.")
                }
            }
        }
    };

    let content = if args.fields.is_empty() {
        None
    } else {
        if !matches!(args.answer, Answer::Accept) {
            bail!("--field only applies to `accept`.");
        }
        let mut values = serde_json::Map::new();
        for field in &args.fields {
            let (name, value) = field
                .split_once('=')
                .with_context(|| format!("--field {field} is not NAME=VALUE"))?;
            values.insert(name.to_string(), Value::String(value.to_string()));
        }
        Some(Value::Object(values))
    };

    let result = answer(
        client,
        &backboard,
        &args.task,
        request_id,
        args.answer,
        content,
    )
    .await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "Answer sent ({}).",
            text(&result, "outcome").unwrap_or("ok")
        );
        println!(
            "{}",
            format!(
                "Follow the task with `railway ca task status {} --wait`.",
                args.task
            )
            .dimmed()
        );
    }
    Ok(())
}

/// Answer one request a task is waiting on.
pub(crate) async fn answer(
    client: &reqwest::Client,
    backboard: &str,
    task_id: &str,
    request_id: String,
    answer: Answer,
    content: Option<Value>,
) -> Result<Value> {
    use cloud_agent_task_respond::CloudAgentTaskInteractionAction as A;
    let action = match answer {
        Answer::Allow => A::ALLOW,
        Answer::Deny => A::DENY,
        Answer::Accept => A::ACCEPT,
        Answer::Decline => A::DECLINE,
        Answer::Cancel => A::CANCEL,
    };
    let result = post_graphql::<mutations::CloudAgentTaskRespond, _>(
        client,
        backboard,
        cloud_agent_task_respond::Variables {
            input: cloud_agent_task_respond::CloudAgentTaskRespondInput {
                task_id: task_id.to_string(),
                request_id,
                action,
                content,
                answered_by: None,
            },
        },
    )
    .await?
    .cloud_agent_task_respond;
    Ok(serde_json::to_value(result)?)
}

async fn cancel(configs: &Configs, client: &reqwest::Client, args: CancelArgs) -> Result<()> {
    let result = post_graphql::<mutations::CloudAgentTaskCancel, _>(
        client,
        configs.get_backboard(),
        cloud_agent_task_cancel::Variables {
            task_id: args.task.clone(),
        },
    )
    .await
    .map_err(|error| {
        // The API answers the same for a task that does not exist and one
        // that is not the caller's.
        if format!("{error:#}").contains("do not have access") {
            anyhow::anyhow!("No task {} that you can cancel.", args.task)
        } else {
            error.into()
        }
    })?
    .cloud_agent_task_cancel;
    let result = serde_json::to_value(result)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if finished(&result) {
        // The status is the one the task had when cancel was requested.
        println!(
            "Task {} had already finished. Nothing was cancelled.",
            args.task.cyan()
        );
    } else {
        println!("Cancel requested for task {}.", args.task.cyan());
    }
    Ok(())
}

async fn models(configs: &mut Configs, client: &reqwest::Client, args: ModelsArgs) -> Result<()> {
    let (project_id, environment_id) = resolve_project_and_env(
        configs,
        client,
        args.target.project.clone(),
        args.target.environment.clone(),
    )
    .await?;
    let found = post_graphql::<queries::CloudAgentTaskModels, _>(
        client,
        configs.get_backboard(),
        cloud_agent_task_models::Variables {
            project_id,
            environment_id,
        },
    )
    .await?
    .cloud_agent_task_models;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&found)?);
        return Ok(());
    }
    println!(
        "{:<32}  {:<12}  {}",
        "MODEL".dimmed(),
        "PROVIDER".dimmed(),
        "EFFORTS".dimmed()
    );
    for model in &found.models {
        println!(
            "{:<32}  {:<12}  {}{}",
            model.id,
            model.provider,
            model.reasoning_efforts.join(", "),
            if model.is_default {
                "  (default)".dimmed().to_string()
            } else {
                String::new()
            }
        );
    }
    Ok(())
}

async fn environment(
    configs: &mut Configs,
    client: &reqwest::Client,
    target: &TargetArgs,
) -> Result<String> {
    Ok(resolve_project_and_env(
        configs,
        client,
        target.project.clone(),
        target.environment.clone(),
    )
    .await?
    .1)
}

/// A task's record, found by ID in its environment. Reading a live task needs
/// the agent and session it runs on, which a task ID alone does not carry; the
/// record has both, plus the kept result of a finished task.
pub(crate) async fn find_task(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
    task_id: &str,
) -> Result<Value> {
    let mut cursor = None;
    for _ in 0..FIND_TASK_PAGES {
        let page = post_graphql::<queries::CloudAgentTasks, _>(
            client,
            backboard,
            cloud_agent_tasks::Variables {
                environment_id: environment_id.to_string(),
                status: None,
                cloud_agent_id: None,
                limit: Some(100),
                cursor,
            },
        )
        .await?
        .cloud_agent_tasks;

        if let Some(task) = page.tasks.iter().find(|task| task.id == task_id) {
            let mut record = serde_json::to_value(task)?;
            record["taskId"] = Value::String(task_id.to_string());
            return Ok(record);
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    bail!("No task {task_id} in this environment. Pass --environment if it ran somewhere else.")
}

/// An environment's task records, newest first, optionally only one agent's.
/// Bounded: a very long history is cut off at the oldest page read.
pub(crate) async fn recent_tasks(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
    cloud_agent_id: Option<&str>,
) -> Result<Vec<Value>> {
    let mut records = Vec::new();
    let mut cursor = None;
    for _ in 0..FIND_TASK_PAGES {
        let page = post_graphql::<queries::CloudAgentTasks, _>(
            client,
            backboard,
            cloud_agent_tasks::Variables {
                environment_id: environment_id.to_string(),
                status: None,
                cloud_agent_id: cloud_agent_id.map(str::to_string),
                limit: Some(100),
                cursor,
            },
        )
        .await?
        .cloud_agent_tasks;
        if let Value::Array(tasks) = serde_json::to_value(&page.tasks)? {
            records.extend(tasks);
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    Ok(records)
}

/// The agent and session a task record points at.
fn placement(record: &Value) -> Result<(String, String)> {
    match (text(record, "cloudAgentId"), text(record, "sessionId")) {
        (Some(agent), Some(session)) => Ok((agent.to_string(), session.to_string())),
        _ => bail!(
            "Task {} has not been given an agent yet. Try again shortly.",
            text(record, "taskId").unwrap_or_default()
        ),
    }
}

fn finished(task: &Value) -> bool {
    matches!(
        text(task, "status"),
        Some("COMPLETED" | "FAILED" | "CANCELLED")
    )
}

/// A task's status, with a cancelled task told apart from a failed one.
pub(crate) fn outcome(task: &Value) -> &str {
    match text(task, "status") {
        Some("FAILED") if text(task, "error") == Some(CANCELLED_ERROR) => "CANCELLED",
        Some(status) => status,
        None => "UNKNOWN",
    }
}

pub(crate) async fn fetch(
    client: &reqwest::Client,
    backboard: &str,
    agent_id: &str,
    session_id: &str,
    task_id: &str,
) -> Result<Value> {
    let task = post_graphql::<queries::CloudAgentTask, _>(
        client,
        backboard,
        cloud_agent_task::Variables {
            cloud_agent_id: agent_id.to_string(),
            session_id: session_id.to_string(),
            task_id: Some(task_id.to_string()),
        },
    )
    .await?
    .cloud_agent_task;
    Ok(serde_json::to_value(task)?)
}

/// Poll until the task settles: finished, failed, or waiting on an answer
/// only a person can give.
pub(crate) async fn wait(
    client: &reqwest::Client,
    backboard: &str,
    agent_id: &str,
    session_id: &str,
    task_id: &str,
    show_progress: bool,
) -> Result<Value> {
    let spinner = (show_progress && std::io::stderr().is_terminal())
        .then(|| create_spinner("Starting".to_string()));
    let started = std::time::Instant::now();
    // The API drops a task's progress once it finishes, so the last progress
    // seen while it ran is kept and handed back with the result.
    let mut last_progress: Option<Value> = None;
    let mut tools_shown = 0;
    let outcome = loop {
        let task = tokio::select! {
            task = fetch(client, backboard, agent_id, session_id, task_id) => task,
            _ = tokio::signal::ctrl_c() => Err(Interrupted { task_id: task_id.to_string() }.into()),
        };
        let task = match task {
            Ok(task) => task,
            Err(error) => break Err(error),
        };
        if !task["progress"].is_null() {
            // Each tool the agent reaches for gets a line of its own, so a
            // long wait shows what it has been doing.
            if let Some(spinner) = &spinner {
                let tools = task["progress"]["tools"].as_array();
                for tool in tools.into_iter().flatten().skip(tools_shown) {
                    spinner.println(format!(
                        "  {} {}",
                        "↳".dimmed(),
                        tool.as_str().unwrap_or("tool").dimmed()
                    ));
                }
                tools_shown = tools.map_or(tools_shown, Vec::len);
            }
            last_progress = Some(task["progress"].clone());
        }
        if settled(&task) {
            break Ok(task);
        }
        if let Some(spinner) = &spinner {
            spinner.set_message(format!(
                "{} · {}s",
                progress_line(&task),
                started.elapsed().as_secs()
            ));
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
            _ = tokio::signal::ctrl_c() => break Err(Interrupted { task_id: task_id.to_string() }.into()),
        }
    };
    if let Some(spinner) = &spinner {
        spinner.finish_and_clear();
    }
    let mut task = outcome?;
    if task["progress"].is_null() {
        if let Some(progress) = last_progress {
            task["progress"] = progress;
        }
    }
    Ok(task)
}

fn settled(task: &Value) -> bool {
    match text(task, "status") {
        Some("COMPLETED" | "FAILED" | "CANCELLED") => true,
        // Waiting with nothing left to answer is the moment between an answer
        // being sent and the agent picking it up, not a place to stop.
        Some("AWAITING_INPUT") => !pending(task).is_empty(),
        _ => false,
    }
}

fn progress_line(task: &Value) -> String {
    let state = match text(task, "status") {
        Some("PENDING") => "Starting",
        _ => "Working",
    };
    let progress = &task["progress"];
    let steps = progress["steps"].as_i64().unwrap_or(0);
    let tool = progress["tools"]
        .as_array()
        .and_then(|tools| tools.last())
        .and_then(Value::as_str);
    let counted = if steps == 1 { "step" } else { "steps" };
    match (steps, tool) {
        (0, _) => state.to_string(),
        (steps, Some(tool)) => format!("{state} · {steps} {counted} · {tool}"),
        (steps, None) => format!("{state} · {steps} {counted}"),
    }
}

/// Print a settled (or snapshotted) task and turn a failure into a non-zero
/// exit.
fn finish(task: &Value, as_json: bool) -> Result<()> {
    let status = outcome(task);
    let task_id = text(task, "taskId").unwrap_or_default();

    if as_json {
        println!("{}", serde_json::to_string_pretty(task)?);
    } else {
        match status {
            "COMPLETED" => {
                if let Some(result) = text(task, "text").filter(|text| !text.is_empty()) {
                    println!("{result}");
                }
                if let Some(structured) = task.get("structuredOutput").filter(|v| !v.is_null()) {
                    println!("{}", serde_json::to_string_pretty(structured)?);
                }
                for attachment in task["attachments"].as_array().into_iter().flatten() {
                    println!(
                        "{}",
                        format!(
                            "Attachment: {} {}",
                            text(attachment, "filename").unwrap_or_default(),
                            text(attachment, "url").unwrap_or_default()
                        )
                        .dimmed()
                    );
                }
            }
            "AWAITING_INPUT" => {
                println!("Task {} is waiting on an answer:", task_id.cyan());
                let requests = pending(task);
                print_interactions(&requests);
                let hint = if requests.iter().any(is_approval) {
                    format!("Answer with `railway ca task respond {task_id} <allow|deny>`.")
                } else {
                    format!(
                        "Answer with `railway ca task run \"<your answer>\" --reply-to {task_id}`."
                    )
                };
                println!("{}", hint.dimmed());
            }
            "FAILED" | "CANCELLED" => {}
            other => {
                println!("Task {} is {}.", task_id.cyan(), colorize_status(other));
                let line = progress_line(task);
                if line.contains('·') {
                    println!("{}", line.dimmed());
                }
            }
        }
    }

    match status {
        "FAILED" => bail!(
            "Task {task_id} failed: {}",
            text(task, "error").unwrap_or("no reason given")
        ),
        "CANCELLED" => bail!("Task {task_id} was cancelled."),
        _ => Ok(()),
    }
}

/// Whether a pending request is a yes-or-no approval rather than a question.
pub(crate) fn is_approval(request: &Value) -> bool {
    request["actions"]
        .as_array()
        .is_some_and(|actions| actions.iter().any(|action| action == "ALLOW"))
}

pub(crate) fn pending(task: &Value) -> Vec<Value> {
    task["pendingInteractions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn print_interactions(interactions: &[Value]) {
    for interaction in interactions {
        let what = text(interaction, "message")
            .or_else(|| text(interaction, "tool"))
            .unwrap_or("(no description)");
        println!("  {what}");
        // A request ID only matters when there is a choice of which to answer.
        if interactions.len() > 1 {
            println!(
                "    {}",
                format!(
                    "--request {}",
                    text(interaction, "requestId").unwrap_or_default()
                )
                .dimmed()
            );
        }
    }
}

/// One `--image` value as the API wants it: a URL is passed through for
/// Railway to fetch, a path is read and sent inline.
fn image_input(image: &str) -> Result<cloud_agent_task_dispatch::CloudAgentTaskImageInput> {
    if image.starts_with("https://") || image.starts_with("http://") {
        return Ok(cloud_agent_task_dispatch::CloudAgentTaskImageInput {
            url: Some(image.to_string()),
            data: None,
            filename: None,
            media_type: None,
        });
    }
    let path = std::path::Path::new(image);
    let bytes = std::fs::read(path).with_context(|| format!("Could not read image {image}"))?;
    let media_type = match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => bail!("{image} is not a PNG, JPEG, GIF or WebP image."),
    };
    Ok(cloud_agent_task_dispatch::CloudAgentTaskImageInput {
        url: None,
        data: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
        filename: path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string),
        media_type: Some(media_type.to_string()),
    })
}

pub(crate) fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

pub(crate) fn colorize_status(status: &str) -> colored::ColoredString {
    let label = status.to_ascii_lowercase().replace('_', " ");
    match status {
        "COMPLETED" => label.green(),
        "FAILED" => label.red(),
        "AWAITING_INPUT" | "CANCELLED" => label.yellow(),
        "RUNNING" => label.cyan(),
        _ => label.dimmed(),
    }
}

pub(crate) fn truncate(value: &str, width: usize) -> String {
    let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= width {
        return single_line;
    }
    let mut out: String = single_line.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn settles_on_terminal_and_waiting_states() {
        for status in ["COMPLETED", "FAILED", "CANCELLED"] {
            assert!(settled(&json!({ "status": status })), "{status}");
        }
        let asking = json!({
            "status": "AWAITING_INPUT",
            "pendingInteractions": [{ "requestId": "r1" }]
        });
        assert!(settled(&asking));
        let answered = json!({ "status": "AWAITING_INPUT", "pendingInteractions": [] });
        assert!(!settled(&answered));
        for status in ["PENDING", "RUNNING"] {
            assert!(!settled(&json!({ "status": status })), "{status}");
        }
    }

    #[test]
    fn progress_line_names_the_latest_tool() {
        let task = json!({
            "status": "RUNNING",
            "progress": { "steps": 3, "tools": ["read", "bash"] }
        });
        assert_eq!(progress_line(&task), "Working · 3 steps · bash");
        let one = json!({ "status": "RUNNING", "progress": { "steps": 1, "tools": [] } });
        assert_eq!(progress_line(&one), "Working · 1 step");
        assert_eq!(progress_line(&json!({ "status": "PENDING" })), "Starting");
    }

    #[test]
    fn failed_task_is_an_error() {
        let task = json!({ "status": "FAILED", "taskId": "t1", "error": "boom" });
        let error = finish(&task, true).unwrap_err().to_string();
        assert_eq!(error, "Task t1 failed: boom");
        assert!(finish(&json!({ "status": "COMPLETED", "taskId": "t1" }), true).is_ok());
    }

    #[test]
    fn record_placement_needs_agent_and_session() {
        let placed = json!({ "taskId": "t1", "cloudAgentId": "a", "sessionId": "s" });
        assert_eq!(placement(&placed).unwrap(), ("a".into(), "s".into()));
        assert!(placement(&json!({ "taskId": "t1", "cloudAgentId": null })).is_err());
        assert!(finished(&json!({ "status": "FAILED" })));
        assert!(!finished(&json!({ "status": "AWAITING_INPUT" })));
    }

    #[test]
    fn cancelled_is_told_apart_from_failed() {
        let cancelled = json!({ "status": "FAILED", "taskId": "t1", "error": CANCELLED_ERROR });
        assert_eq!(outcome(&cancelled), "CANCELLED");
        assert_eq!(
            finish(&cancelled, true).unwrap_err().to_string(),
            "Task t1 was cancelled."
        );
        assert_eq!(outcome(&json!({ "status": "CANCELLED" })), "CANCELLED");
        assert_eq!(
            outcome(&json!({ "status": "FAILED", "error": "boom" })),
            "FAILED"
        );
        assert!(finished(&json!({ "status": "CANCELLED" })));
    }

    #[test]
    fn a_prompt_in_the_project_flag_is_pointed_out() {
        assert_eq!(required_prompt(Some(" fix it "), None).unwrap(), "fix it");
        assert!(
            required_prompt(Some("   "), None)
                .unwrap_err()
                .to_string()
                .contains("empty")
        );
        let hint = required_prompt(None, Some("fix the bug"))
            .unwrap_err()
            .to_string();
        assert!(hint.contains("-p is --project"), "{hint}");
        let plain = required_prompt(None, Some("0b1c")).unwrap_err().to_string();
        assert!(plain.contains("A prompt is required"), "{plain}");
    }

    #[test]
    fn api_wording_is_rewritten_for_the_cli() {
        let model = explain(
            anyhow::anyhow!("Unknown model x; list the available ones with cloudAgentTaskModels"),
            None,
        );
        assert!(model.to_string().contains("`railway ca task models`"));
        let effort = explain(anyhow::anyhow!("reasoningEffort must be one of low"), None);
        assert_eq!(effort.to_string(), "--effort must be one of low");
        let reply = explain(anyhow::anyhow!("Reply task not found"), Some("t9"));
        assert!(
            reply
                .to_string()
                .starts_with("No task t9 in this environment")
        );
        assert_eq!(explain(anyhow::anyhow!("other"), None).to_string(), "other");
    }

    #[test]
    fn approvals_are_told_apart_from_questions() {
        assert!(is_approval(&json!({ "actions": ["ALLOW", "DENY"] })));
        assert!(!is_approval(&json!({ "actions": ["ACCEPT", "DECLINE"] })));
        assert!(!is_approval(&json!({})));
    }

    #[test]
    fn pre_accepted_run_skips_the_prompt() {
        assert!(confirm_new_agent(true, NEW_AGENT_NOTICE).is_ok());
    }

    #[test]
    fn image_urls_pass_through_and_unknown_files_are_refused() {
        let url = image_input("https://example.com/a.png").unwrap();
        assert_eq!(url.url.as_deref(), Some("https://example.com/a.png"));
        assert!(url.data.is_none());
        assert!(image_input("notes.txt").is_err());
    }

    #[test]
    fn truncate_keeps_one_line() {
        assert_eq!(truncate("a\n b", 10), "a b");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}
