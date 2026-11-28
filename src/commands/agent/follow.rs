//! Following a run that `railway agent` handed to the agent dashboard: the
//! terminal stays on it the way the dashboard's composer opens it, showing what
//! the agent does, taking its questions and approvals, and ending on what it
//! made and what to do next. Ctrl-C detaches; the run keeps going.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::Result;
use colored::Colorize;
use serde_json::{Value, json};

use indicatif::ProgressBar;

use crate::{
    commands::cloud_agent::task::{self, Answer},
    config::Configs,
    controllers::cloud_agent as ca,
    util::progress::create_spinner,
};

/// The `server` the agent's own questions (`ask_requester`) come under, and
/// the one field they are answered with.
const REQUESTER_SERVER: &str = "requester";
const REPLY_FIELD: &str = "reply";

/// A run the API started on the agent dashboard.
pub(super) struct Run<'a> {
    pub client: &'a reqwest::Client,
    pub backboard: &'a str,
    pub project: &'a str,
    pub environment: &'a str,
    pub environment_id: &'a str,
    pub task_id: String,
    pub agent_id: String,
    pub session_id: String,
    pub url: String,
}

/// How following a run ended.
pub(super) enum Outcome {
    Done,
    /// The person stopped following; the run carries on.
    Detached,
    Failed(String),
    Cancelled,
}

/// What a question the run is parked on needs from the person.
#[derive(Debug, PartialEq)]
enum Ask {
    /// Allow or deny a tool call.
    Approval,
    /// The agent's own question, answered in words.
    Question,
    /// A form (variables, another server's question): answered on the
    /// dashboard, so nothing like a secret is typed into a terminal.
    Dashboard,
}

fn ask_of(request: &Value) -> Ask {
    if task::is_approval(request) {
        Ask::Approval
    } else if task::text(request, "kind") == Some("ELICITATION")
        || task::text(request, "kind") == Some("elicitation")
    {
        if task::text(request, "server") == Some(REQUESTER_SERVER) {
            Ask::Question
        } else {
            Ask::Dashboard
        }
    } else {
        Ask::Dashboard
    }
}

pub(super) async fn follow(run: &Run<'_>) -> Result<Outcome> {
    header(run);
    let started = Instant::now();
    // One spinner for the whole run, so the tools it has used are printed once
    // however many questions interrupt it.
    let spinner = create_spinner("Starting".to_string());
    let watched = watch(run, &spinner, started).await;
    spinner.finish_and_clear();
    Ok(match watched? {
        Watched::Done(task) => {
            finish(run, &task, started.elapsed()).await;
            Outcome::Done
        }
        Watched::Detached => Outcome::Detached,
        Watched::Failed(error) => Outcome::Failed(error),
        Watched::Cancelled => Outcome::Cancelled,
    })
}

/// How watching ended, with the finished task to report on.
enum Watched {
    Done(Value),
    Detached,
    Failed(String),
    Cancelled,
}

/// What the person said to one of the run's questions.
enum Reply {
    Send(Answer, Option<Value>),
    /// Left for the dashboard.
    Later,
    /// They backed out of the prompt: stop following.
    Stop,
}

/// Poll the run until it ends or the person stops following, answering what it
/// asks along the way.
async fn watch(run: &Run<'_>, spinner: &ProgressBar, started: Instant) -> Result<Watched> {
    let mut handled: HashSet<String> = HashSet::new();
    let mut tools_shown = 0;
    let mut told_dashboard = false;
    loop {
        let task = tokio::select! {
            task = task::fetch(run.client, run.backboard, &run.agent_id, &run.session_id, &run.task_id) => task?,
            _ = tokio::signal::ctrl_c() => return Ok(Watched::Detached),
        };
        if let Some(tools) = task["progress"]["tools"].as_array() {
            // A new attempt starts its list over.
            if tools.len() < tools_shown {
                tools_shown = 0;
            }
            for tool in tools.iter().skip(tools_shown) {
                spinner.println(format!(
                    "  {} {}",
                    "↳".dimmed(),
                    tool.as_str().unwrap_or("tool").dimmed()
                ));
            }
            tools_shown = tools.len();
        }
        match task::outcome(&task) {
            "COMPLETED" => return Ok(Watched::Done(task)),
            "FAILED" => {
                return Ok(Watched::Failed(
                    task::text(&task, "error")
                        .unwrap_or("no reason given")
                        .to_string(),
                ));
            }
            "CANCELLED" => return Ok(Watched::Cancelled),
            _ => {}
        }

        let open: Vec<Value> = task::pending(&task)
            .into_iter()
            .filter(|request| !handled.contains(request_id(request)))
            .collect();
        for request in open {
            let id = request_id(&request).to_string();
            let what = task::text(&request, "message")
                .or_else(|| task::text(&request, "tool"))
                .unwrap_or("The agent needs your input")
                .to_string();
            let ask = ask_of(&request);
            // The spinner steps aside while a prompt has the terminal.
            let reply = spinner.suspend(|| -> Result<Reply> {
                println!("{} {what}", "?".yellow().bold());
                Ok(match ask {
                    Ask::Approval => match confirm("Allow?")? {
                        Some(true) => Reply::Send(Answer::Allow, None),
                        Some(false) => Reply::Send(Answer::Deny, None),
                        None => Reply::Stop,
                    },
                    Ask::Question => match ask_reply()? {
                        Some(text) => {
                            Reply::Send(Answer::Accept, Some(json!({ REPLY_FIELD: text })))
                        }
                        None => Reply::Stop,
                    },
                    Ask::Dashboard => {
                        if !told_dashboard {
                            println!(
                                "  Answer it on the dashboard ({}); this keeps following the run.",
                                link(&run.url, "open the run")
                            );
                            told_dashboard = true;
                        }
                        Reply::Later
                    }
                })
            })?;
            match reply {
                Reply::Send(action, content) => {
                    task::answer(
                        run.client,
                        run.backboard,
                        &run.task_id,
                        id.clone(),
                        action,
                        content,
                    )
                    .await?;
                }
                Reply::Later => {}
                Reply::Stop => return Ok(Watched::Detached),
            }
            handled.insert(id);
        }

        let pending = task::pending(&task);
        let waiting_on_dashboard = task::outcome(&task) == "AWAITING_INPUT"
            && !pending.is_empty()
            && pending
                .iter()
                .all(|request| ask_of(request) == Ask::Dashboard);
        spinner.set_message(if waiting_on_dashboard {
            format!(
                "Waiting for your answer on the dashboard · {}",
                duration(started.elapsed())
            )
        } else {
            format!(
                "{} · {}",
                task::progress_line(&task),
                duration(started.elapsed())
            )
        });
        tokio::select! {
            _ = tokio::time::sleep(task::POLL_INTERVAL) => {}
            _ = tokio::signal::ctrl_c() => return Ok(Watched::Detached),
        }
    }
}

fn header(run: &Run<'_>) {
    println!();
    println!(
        "  {} {}",
        "◆".purple(),
        "A code change, so it's running on the agent dashboard".bold()
    );
    println!(
        "    {}",
        format!("{} · {}", run.project, run.environment).dimmed()
    );
    println!("    {}", link(&run.url, "Open the run on the dashboard ↗"));
    println!(
        "    {}",
        "Following it here. Ctrl-C stops following; the agent keeps working.".dimmed()
    );
    println!();
}

async fn finish(run: &Run<'_>, task: &Value, elapsed: Duration) {
    println!(
        "{} {}",
        "✓".green().bold(),
        format!("Done in {}", duration(elapsed)).bold()
    );
    if let Some(summary) = task::text(task, "text").filter(|text| !text.trim().is_empty()) {
        println!();
        termimad::MadSkin::default().print_text(summary);
    }
    println!();
    // The machine serves what the agent built; it sleeps a while after the
    // run, so a missing preview is left out rather than reported.
    if let Ok(Some(domain)) = preview(run).await {
        println!("  {}https://{domain}", label("Preview"));
    }
    println!(
        "  {}Review it, open a pull request or ship it: {}",
        label("Next"),
        link(&run.url, "the run on the dashboard ↗")
    );
    println!(
        "  {}{}",
        label(""),
        format!(
            "railway agent --thread-id {} -p \"<what next>\" keeps going here",
            run.session_id
        )
        .dimmed()
    );
}

/// A left-hand label, padded before it is styled so the escape codes don't
/// count toward its width.
fn label(text: &str) -> colored::ColoredString {
    format!("{text:<9}").bold()
}

/// What to say after following a run ends without it finishing.
pub(super) fn detached_hint(run: &Run<'_>) -> String {
    format!(
        "Stopped following. The agent keeps working on the dashboard: {}",
        run.url
    )
}

async fn preview(run: &Run<'_>) -> Result<Option<String>> {
    ca::preview_domain(run.client, run.backboard, run.environment_id, &run.agent_id).await
}

fn request_id(request: &Value) -> &str {
    task::text(request, "requestId").unwrap_or_default()
}

/// Yes or no; `None` when the person backs out.
fn confirm(message: &str) -> Result<Option<bool>> {
    match inquire::Confirm::new(message)
        .with_default(true)
        .with_render_config(Configs::get_render_config())
        .prompt()
    {
        Ok(answer) => Ok(Some(answer)),
        Err(
            inquire::InquireError::OperationInterrupted | inquire::InquireError::OperationCanceled,
        ) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The person's answer to the agent's question; `None` when they back out.
fn ask_reply() -> Result<Option<String>> {
    match inquire::Text::new("Your answer:")
        .with_render_config(Configs::get_render_config())
        .with_validator(|text: &str| {
            Ok(if text.trim().is_empty() {
                inquire::validator::Validation::Invalid("Say something, or Esc to stop".into())
            } else {
                inquire::validator::Validation::Valid
            })
        })
        .prompt()
    {
        Ok(text) => Ok(Some(text)),
        Err(
            inquire::InquireError::OperationInterrupted | inquire::InquireError::OperationCanceled,
        ) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// `label` linked to `url` where the terminal shows links (OSC 8), else the
/// URL itself, which every terminal can at least select.
fn link(url: &str, label: &str) -> String {
    if hyperlinks() {
        format!(
            "\u{1b}]8;;{url}\u{1b}\\{}\u{1b}]8;;\u{1b}\\",
            label.cyan().underline()
        )
    } else {
        url.cyan().to_string()
    }
}

fn hyperlinks() -> bool {
    hyperlinks_in(|name| std::env::var(name).ok())
}

/// Terminals known to render OSC 8 links. Elsewhere the escape could show as
/// noise, so the plain URL is printed instead.
fn hyperlinks_in(env: impl Fn(&str) -> Option<String>) -> bool {
    if env("NO_HYPERLINKS").is_some() || env("CI").is_some() {
        return false;
    }
    let program = env("TERM_PROGRAM").unwrap_or_default();
    matches!(
        program.as_str(),
        "iTerm.app" | "WezTerm" | "vscode" | "ghostty" | "Hyper" | "Tabby" | "rio" | "zed"
    ) || env("WT_SESSION").is_some()
        || env("KITTY_WINDOW_ID").is_some()
        || env("TERM").is_some_and(|term| term.contains("kitty") || term.contains("ghostty"))
        || env("VTE_VERSION")
            .and_then(|version| version.parse::<u32>().ok())
            .is_some_and(|version| version >= 5000)
}

fn duration(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match (seconds / 3600, (seconds % 3600) / 60, seconds % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_are_answered_here_and_forms_on_the_dashboard() {
        assert_eq!(
            ask_of(&json!({ "kind": "APPROVAL", "actions": ["ALLOW", "DENY"] })),
            Ask::Approval
        );
        assert_eq!(
            ask_of(&json!({ "kind": "ELICITATION", "server": "requester", "actions": ["ACCEPT"] })),
            Ask::Question
        );
        assert_eq!(
            ask_of(&json!({ "kind": "ELICITATION", "server": "railway", "actions": ["ACCEPT"] })),
            Ask::Dashboard
        );
    }

    #[test]
    fn links_only_where_the_terminal_renders_them() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        };
        assert!(hyperlinks_in(env(&[("TERM_PROGRAM", "ghostty")])));
        assert!(hyperlinks_in(env(&[("TERM_PROGRAM", "iTerm.app")])));
        assert!(!hyperlinks_in(env(&[("TERM_PROGRAM", "Apple_Terminal")])));
        assert!(!hyperlinks_in(env(&[
            ("TERM_PROGRAM", "iTerm.app"),
            ("NO_HYPERLINKS", "1")
        ])));
        assert!(hyperlinks_in(env(&[("VTE_VERSION", "7000")])));
    }

    #[test]
    fn durations_read_like_a_person_would_say_them() {
        assert_eq!(duration(Duration::from_secs(42)), "42s");
        assert_eq!(duration(Duration::from_secs(252)), "4m 12s");
        assert_eq!(duration(Duration::from_secs(3720)), "1h 2m");
    }
}
