use std::{
    borrow::Cow,
    collections::{HashSet, VecDeque},
    future::IntoFuture,
    io::{Stdout, Write},
    sync::Mutex,
    time::Duration,
};

use anyhow::{Context, Result};
use futures::StreamExt;
use graphql_ws_client::graphql::StreamingOperation;
use serde::Serialize;
use tokio::sync::{Notify, OnceCell};
use tokio::time::Instant;

use crate::{
    controllers::deployment::BuildLogContext,
    gql::subscriptions,
    subscription::connect_graphql,
    util::logs::{LogFormat, LogLike, format_log_string, strip_terminal_controls},
};

const FINAL_LOG_GRACE: Duration = Duration::from_secs(1);
const FINAL_LOG_QUIET: Duration = Duration::from_secs(2);
const FINAL_LOG_TIMEOUT: Duration = Duration::from_secs(15);
const RECENT_LOG_LIMIT: usize = 1_000;

#[derive(Clone, Hash, PartialEq, Eq)]
struct LogKey {
    timestamp: String,
    message: String,
    attributes: Vec<(String, String)>,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct BuildStepKey {
    digest: String,
    message: String,
}

fn attribute<'a>(attributes: &'a [(String, String)], name: &str) -> Option<Cow<'a, str>> {
    let (_, value) = attributes.iter().find(|(key, _)| key == name)?;
    // Attributes can contain either plain strings or JSON-encoded strings.
    Some(match serde_json::from_str::<String>(value) {
        Ok(value) => Cow::Owned(value),
        Err(_) => Cow::Borrowed(value),
    })
}

fn is_error(attributes: &[(String, String)]) -> bool {
    attribute(attributes, "error").is_some_and(|error| !error.is_empty() && error != "null")
        || ["level", "severity", "lvl"].iter().any(|name| {
            attribute(attributes, name).is_some_and(|level| {
                matches!(
                    level.to_ascii_lowercase().as_str(),
                    "error" | "err" | "fatal" | "critical"
                )
            })
        })
}

struct Output<W> {
    writer: W,
    seen: HashSet<LogKey>,
    recent: VecDeque<LogKey>,
    seen_steps: HashSet<BuildStepKey>,
    recent_steps: VecDeque<BuildStepKey>,
    last_message: Option<String>,
    error_seen: bool,
    closed: bool,
}

impl<W> Output<W> {
    fn build_step_message<'a>(
        &mut self,
        message: &'a str,
        attributes: &[(String, String)],
    ) -> Option<Cow<'a, str>> {
        if attribute(attributes, "source").as_deref() != Some("buildkit")
            || attribute(attributes, "type").as_deref() != Some("vertex")
        {
            return Some(Cow::Borrowed(message));
        }

        // A failed vertex often has the same message as its progress updates;
        // the actual error only appears in an attribute. Always surface it.
        if let Some(error) = attribute(attributes, "error")
            .filter(|error| !error.is_empty() && error.as_ref() != "null")
        {
            return Some(Cow::Owned(format!("{message}: {error}")));
        }
        if ["level", "severity", "lvl"].iter().any(|name| {
            attribute(attributes, name).is_some_and(|level| {
                matches!(
                    level.to_ascii_lowercase().as_str(),
                    "warn" | "warning" | "error" | "err" | "fatal" | "critical"
                )
            })
        }) {
            return Some(Cow::Borrowed(message));
        }

        let Some(digest) = attribute(attributes, "digest").filter(|digest| !digest.is_empty())
        else {
            return Some(Cow::Borrowed(message));
        };
        let step = BuildStepKey {
            digest: digest.into_owned(),
            message: message.to_owned(),
        };
        // BuildKit sends queued, started, and completed events for each
        // vertex. They render identically in plain output, including when
        // other steps' updates arrive in between. Do not apply this to the
        // vertex's stdout/stderr events or to JSON output.
        if !self.seen_steps.insert(step.clone()) {
            return None;
        }
        self.recent_steps.push_back(step);
        if self.recent_steps.len() > RECENT_LOG_LIMIT {
            let expired = self.recent_steps.pop_front().unwrap();
            self.seen_steps.remove(&expired);
        }
        Some(Cow::Borrowed(message))
    }
}

/// Keep live output open after FAILED, then replay the subscription's tail on
/// the same socket until error logs settle or finalization times out. Repeated
/// replays catch late entries behind the server's timestamp cursor without an
/// HTTP build-log query.
pub(super) struct BuildLogs<W = Stdout> {
    json: bool,
    output: Mutex<Output<W>>,
    context: OnceCell<BuildLogContext>,
    context_ready: Notify,
    finished: OnceCell<()>,
    replay_requested: Notify,
    replay_finished: Notify,
}

impl BuildLogs {
    pub(super) fn new(json: bool) -> Self {
        Self::with_writer(json, std::io::stdout())
    }

    pub(super) async fn finish(&self) {
        self.finished
            .get_or_init(|| async {
                tokio::time::sleep(FINAL_LOG_GRACE).await;
                self.replay_requested.notify_one();
                let _ =
                    tokio::time::timeout(FINAL_LOG_TIMEOUT, self.replay_finished.notified()).await;
                let mut output = self.output.lock().unwrap();
                output.closed = true;
                let _ = output.writer.flush();
            })
            .await;
    }

    pub(super) fn set_context(&self, context: Option<BuildLogContext>) {
        if let Some(context) = context
            && self.context.set(context).is_ok()
        {
            self.context_ready.notify_one();
        }
    }

    pub(super) async fn stream(&self, ci: bool) -> Result<()> {
        // Wake finish even if the socket closes or initial subscription fails.
        let _finished = scopeguard::guard((), |_| self.replay_finished.notify_one());
        // The existing status subscription supplies the first available snapshot.
        // A deployment can fail before it has one, so finalization must also
        // release this wait without making a metadata query.
        let context = loop {
            if let Some(context) = self.context.get() {
                break context;
            }
            tokio::select! {
                _ = self.context_ready.notified() => {}
                _ = self.replay_requested.notified() => return Ok(()),
            }
        };
        let mut delay_ms = 1_000;
        for attempt in 1..=12 {
            let mut received_logs = false;
            let result = self.stream_once(context, ci, &mut received_logs).await;
            match result {
                Ok(()) => return Ok(()),
                Err(error) if received_logs || attempt == 12 => return Err(error),
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms = ((delay_ms as f64 * 1.5) as u64).min(8_000);
                }
            }
        }
        unreachable!()
    }

    async fn stream_once(
        &self,
        context: &BuildLogContext,
        ci: bool,
        received_logs: &mut bool,
    ) -> Result<()> {
        let (client, actor) = connect_graphql().await?;
        let _actor = scopeguard::guard(tokio::spawn(actor.into_future()), |task| task.abort());
        let subscribe = || {
            client.subscribe(StreamingOperation::<subscriptions::EnvironmentLogs>::new(
                context.stream_variables(None),
            ))
        };
        let mut stream = subscribe().await?;
        let mut replaying = false;
        let mut last_new_log = Instant::now();
        loop {
            let response = tokio::select! {
                _ = self.replay_requested.notified(), if !replaying => {
                    stream.stop().await?;
                    stream = subscribe().await?;
                    replaying = true;
                    continue;
                }
                response = stream.next() => response,
            };
            let Some(response) = response else {
                return Ok(());
            };
            let data = response
                .context("Build log stream error")?
                .data
                .context("Failed to retrieve build logs")?;
            for log in data.environment_logs {
                *received_logs = true;
                let skipped = ci && log.message.starts_with("No changed files matched patterns");
                // Accept unseen older entries, including replayed final errors.
                if self.print(log) {
                    last_new_log = Instant::now();
                }
                if skipped {
                    std::process::exit(0);
                }
            }
            if replaying {
                stream.stop().await?;
                // An empty or stale first replay is not a completion signal:
                // logs can still be ingested behind its timestamp cursor.
                // Require a fresh replay after the error output settles. If
                // there is no error log, finish's timeout bounds the wait.
                if self.output.lock().unwrap().error_seen
                    && last_new_log.elapsed() >= FINAL_LOG_QUIET
                {
                    return Ok(());
                }
                tokio::time::sleep(FINAL_LOG_GRACE).await;
                stream = subscribe().await?;
            }
        }
    }
}

impl<W: Write> BuildLogs<W> {
    fn with_writer(json: bool, writer: W) -> Self {
        Self {
            json,
            output: Mutex::new(Output {
                writer,
                seen: HashSet::new(),
                recent: VecDeque::new(),
                seen_steps: HashSet::new(),
                recent_steps: VecDeque::new(),
                last_message: None,
                error_seen: false,
                closed: false,
            }),
            context: OnceCell::new(),
            context_ready: Notify::new(),
            finished: OnceCell::new(),
            replay_requested: Notify::new(),
            replay_finished: Notify::new(),
        }
    }

    /// Return whether the event is new, independently of display deduplication.
    pub(super) fn print<T: LogLike + Serialize>(&self, log: T) -> bool {
        let mut attributes: Vec<_> = log
            .attributes()
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        attributes.sort_unstable();
        let key = LogKey {
            timestamp: log.timestamp().to_owned(),
            message: log.message().to_owned(),
            attributes,
        };

        // Hold the lock through writing so replayed entries cannot print twice
        // or interleave a multiline build error.
        let mut output = self.output.lock().unwrap();
        if output.closed || !output.seen.insert(key.clone()) {
            return false;
        }
        output.error_seen |= is_error(&key.attributes);
        output.recent.push_back(key.clone());
        if output.recent.len() > RECENT_LOG_LIMIT {
            let expired = output.recent.pop_front().unwrap();
            output.seen.remove(&expired);
        }

        let line = if self.json {
            format_log_string(log, true, LogFormat::LevelOnly)
        } else {
            let Some(message) = output.build_step_message(log.message(), &key.attributes) else {
                return true;
            };
            strip_terminal_controls(&message)
        };
        // Compare rendered text so differing timestamps or attributes cannot
        // repeat the same adjacent message in human-readable output.
        if !self.json && output.last_message.as_deref() == Some(line.as_str()) {
            return true;
        }
        writeln!(output.writer, "{line}").expect("failed to write build log");
        if !self.json {
            output.last_message = Some(line);
        }
        true
    }
}
