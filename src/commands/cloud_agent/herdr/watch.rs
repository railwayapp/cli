//! Keeps herdr's machines in step with the cloud agents behind them as they
//! change, so a sleep from anywhere disables the machine before herdr's next
//! reconnect can park it in Attention, and a wake re-enables it once the
//! relay answers. The signal is backboard's `cloudAgentInvalidation`
//! subscription, one per environment holding one of the user's agents, on the
//! unpublished internal graph the web and mobile apps use. No polling: a
//! refused subscription is retried with backoff and said so. One watcher per
//! herdr session, started by `install` and the plugin's startup hook, told to
//! re-list environments by SIGUSR1 (`nudge`), gone when the session's socket is.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;
use futures::StreamExt;
use graphql_client::{GraphQLQuery, QueryBody};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::herdr_cli::Herdr;
use super::state::State;
use super::sync;
use crate::client::GQLClient;
use crate::config::Configs;
use crate::controllers::cloud_agent as ca;
use crate::subscription::subscribe_graphql_internal;

const DEBOUNCE: Duration = Duration::from_secs(2);
const LIVENESS: Duration = Duration::from_secs(30);
const RESUBSCRIBE_MIN: Duration = Duration::from_secs(5);
const RESUBSCRIBE_MAX: Duration = Duration::from_secs(120);

#[derive(Parser)]
pub struct Args {
    /// Stay attached to this terminal and print every event; detached, only
    /// subscriptions, failures and applied changes are logged
    #[clap(long)]
    foreground: bool,
}

struct CloudAgentInvalidation;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Variables {
    environment_id: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ResponseData {
    cloud_agent_invalidation: Invalidation,
}

#[derive(Deserialize, Debug)]
struct Invalidation {
    #[allow(dead_code)]
    id: String,
    agent: Option<Snapshot>,
}

#[derive(Deserialize, Debug)]
struct Snapshot {
    id: String,
    status: String,
}

impl GraphQLQuery for CloudAgentInvalidation {
    type Variables = Variables;
    type ResponseData = ResponseData;

    fn build_query(variables: Variables) -> QueryBody<Variables> {
        QueryBody {
            variables,
            query: "subscription CloudAgentInvalidation($environmentId: String!) { cloudAgentInvalidation(environmentId: $environmentId) { id agent { id status } } }",
            operation_name: "CloudAgentInvalidation",
        }
    }
}

enum Signal {
    Changed(String),
}

pub async fn command(args: Args) -> Result<()> {
    let socket = PathBuf::from(
        std::env::var_os("HERDR_SOCKET_PATH")
            .context("HERDR_SOCKET_PATH is not set; run this from inside herdr")?,
    );
    let pidfile = pidfile_path()?;
    if let Some(pid) = running_pid(&pidfile) {
        bail!("a watcher for this herdr session is already running (pid {pid})");
    }
    std::fs::write(&pidfile, std::process::id().to_string())?;
    let result = run(&socket, args.foreground).await;
    let _ = std::fs::remove_file(&pidfile);
    result
}

async fn run(socket: &Path, verbose: bool) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<Signal>(64);
    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut watched: BTreeSet<String> = BTreeSet::new();
    let mut liveness = tokio::time::interval(LIVENESS);
    let mut nudged = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
        .context("Installing the SIGUSR1 handler")?;
    let mut relist = true;

    loop {
        if relist {
            relist = false;
            match watched_environments().await {
                Ok(envs) if envs != watched => {
                    tasks.shutdown().await;
                    for env in &envs {
                        tasks.spawn(subscribe(env.clone(), tx.clone(), verbose));
                    }
                    say(verbose, &format!("watching {} environment(s)", envs.len()));
                    watched = envs;
                }
                Ok(_) => {}
                Err(e) => say(true, &format!("could not list agents: {e:#}")),
            }
        }
        tokio::select! {
            _ = liveness.tick() => {
                if !server_alive(socket) {
                    say(true, "herdr session gone; exiting");
                    return Ok(());
                }
                if watched.is_empty() {
                    relist = true;
                }
            }
            _ = nudged.recv() => {
                say(verbose, "nudged: re-listing environments");
                relist = true;
            }
            Some(Signal::Changed(what)) = rx.recv() => {
                say(verbose, &what);
                tokio::time::sleep(DEBOUNCE).await;
                while rx.try_recv().is_ok() {}
                resync(verbose, &what).await;
            }
        }
    }
}

/// Every environment holding one of the user's agents: a machine added later
/// in any of them is covered without re-listing.
async fn watched_environments() -> Result<BTreeSet<String>> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let agents = ca::list_mine(&client, &configs.get_backboard()).await?;
    Ok(agents.into_iter().map(|a| a.environment_id).collect())
}

async fn subscribe(environment_id: String, tx: mpsc::Sender<Signal>, verbose: bool) {
    let mut backoff = RESUBSCRIBE_MIN;
    loop {
        let stream = subscribe_graphql_internal::<CloudAgentInvalidation>(Variables {
            environment_id: environment_id.clone(),
        })
        .await;
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                say(
                    true,
                    &format!(
                        "subscribe {environment_id}: {e:#}; retrying in {}s",
                        backoff.as_secs()
                    ),
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RESUBSCRIBE_MAX);
                continue;
            }
        };
        backoff = RESUBSCRIBE_MIN;
        say(true, &format!("subscribed {environment_id}"));
        while let Some(item) = stream.next().await {
            let what = match item {
                Ok(response) => match response.data {
                    Some(data) => describe(&environment_id, data.cloud_agent_invalidation.agent),
                    None => format!("{environment_id}: invalidation without data"),
                },
                Err(e) => {
                    say(verbose, &format!("stream {environment_id}: {e}"));
                    break;
                }
            };
            if tx.send(Signal::Changed(what)).await.is_err() {
                return;
            }
        }
        tokio::time::sleep(RESUBSCRIBE_MIN).await;
    }
}

fn describe(environment_id: &str, agent: Option<Snapshot>) -> String {
    match agent {
        Some(a) => format!("agent {} is now {}", a.id, a.status.to_lowercase()),
        None => format!("environment {environment_id}: agents changed"),
    }
}

async fn resync(verbose: bool, cause: &str) {
    let run = async {
        let configs = Configs::new()?;
        let client = GQLClient::new_authorized(&configs)?;
        sync::resync(&client, &configs.get_backboard(), &Herdr::from_env()).await
    };
    match run.await {
        Ok(applied) if applied.is_empty() => say(verbose, "synced, nothing to change"),
        Ok(applied) => say(true, &format!("{cause}: {}", applied.join(", "))),
        Err(e) => say(true, &format!("sync failed after \"{cause}\": {e:#}")),
    }
}

/// A unix socket file can outlive its server; only a connection proves one.
fn server_alive(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

fn say(verbose: bool, line: &str) {
    if verbose {
        println!(
            "{} {line}",
            chrono::Local::now().format("%H:%M:%S").to_string().dimmed()
        );
    }
}

fn pidfile_path() -> Result<PathBuf> {
    let state = State::path()?;
    let stem = state
        .file_stem()
        .map(|s| s.to_string_lossy().replace("state", "watch"))
        .unwrap_or_else(|| "watch".into());
    Ok(state.with_file_name(format!("{stem}.pid")))
}

/// The recorded pid, only while that pid is still one of our watchers: pids
/// are reused after a crash or reboot, and a signal to a stranger is fatal.
fn running_pid(pidfile: &Path) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pidfile).ok()?.trim().parse().ok()?;
    let out = Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let command = String::from_utf8_lossy(&out.stdout);
    is_watcher_command(&command).then_some(pid)
}

fn is_watcher_command(command: &str) -> bool {
    let mut words = command.split_whitespace();
    words
        .next()
        .is_some_and(|exe| exe.ends_with("railway") || exe.contains("railway"))
        && command.contains(" ca herdr watch")
}

/// Stop this session's watcher, if one of ours is running.
pub fn stop() -> Option<u32> {
    let pidfile = pidfile_path().ok()?;
    let pid = running_pid(&pidfile)?;
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = std::fs::remove_file(&pidfile);
    Some(pid)
}

/// Tell this session's watcher that the set of environments may have changed.
pub fn nudge() {
    let Ok(pidfile) = pidfile_path() else { return };
    if let Some(pid) = running_pid(&pidfile) {
        let _ = Command::new("kill")
            .args(["-USR1", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The watcher's pid for this session, if one is running.
pub fn running() -> Option<u32> {
    pidfile_path().ok().and_then(|p| running_pid(&p))
}

/// Start this session's watcher in the background unless one is up. Quiet on
/// every failure: without a watcher the plugin degrades to the manual sync key.
pub fn spawn_detached() {
    let Ok(pidfile) = pidfile_path() else { return };
    if running_pid(&pidfile).is_some() || std::env::var_os("HERDR_SOCKET_PATH").is_none() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Ok(out) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(pidfile.with_extension("log"))
    else {
        return;
    };
    let Ok(err) = out.try_clone() else { return };
    let mut cmd = Command::new(exe);
    cmd.args(["ca", "herdr", "watch", "--foreground"])
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let _ = cmd.spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_is_the_one_the_apps_send() {
        let body = CloudAgentInvalidation::build_query(Variables {
            environment_id: "env-1".into(),
        });
        assert_eq!(body.operation_name, "CloudAgentInvalidation");
        assert!(
            body.query
                .contains("cloudAgentInvalidation(environmentId: $environmentId)")
        );
        assert_eq!(
            serde_json::to_value(&body.variables).unwrap(),
            serde_json::json!({ "environmentId": "env-1" })
        );
        let data: ResponseData = serde_json::from_value(serde_json::json!({
            "cloudAgentInvalidation": { "id": "x", "agent": { "id": "a1", "status": "SLEEPING" } }
        }))
        .unwrap();
        assert_eq!(
            describe("e", data.cloud_agent_invalidation.agent),
            "agent a1 is now sleeping"
        );
    }

    #[test]
    fn only_a_live_watcher_process_counts_as_running() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("watch.pid");
        std::fs::write(&pidfile, "999999").unwrap();
        assert_eq!(running_pid(&pidfile), None);
        // This test binary is alive but is not `railway ca herdr watch`:
        // a reused pid must never be mistaken for our watcher.
        std::fs::write(&pidfile, std::process::id().to_string()).unwrap();
        assert_eq!(running_pid(&pidfile), None);
        assert!(is_watcher_command(
            "/opt/homebrew/bin/railway ca herdr watch --foreground"
        ));
        assert!(!is_watcher_command("/usr/bin/sleep 30"));
        assert!(!is_watcher_command("railway ca herdr sync"));
    }
}
