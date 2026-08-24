//! Cloud agent lifecycle, shared by `railway ca` and `railway code`.
//!
//! The launcher grew these operations as flags on a command whose job is
//! something else (`--new` creates, `--rm` destroys, `--keep-awake` declines to
//! sleep). Pulling them here gives the flat `railway ca` verbs one
//! implementation to call, and — more importantly — one place where the local
//! agent pointer is kept honest. A delete that forgets to clear the pointer
//! leaves `railway code` waking a corpse.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};

use crate::client::post_graphql;
use crate::config::Configs;
use crate::gql::{mutations, queries};

/// How long to wait for a created or woken agent to reach RUNNING.
///
/// Matches the launcher's budget: a cold create boots a microVM and publishes
/// routes, a wake restores a checkpoint and is much quicker.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Gap between readiness polls. A fresh boot reaches running in single-digit
/// seconds, and every poll-width of delay is pure wait the user sees — so poll
/// fast while a normal boot is still plausible, then back off for the tail.
const POLL_INTERVAL_FAST: std::time::Duration = std::time::Duration::from_millis(400);
const POLL_FAST_WINDOW: std::time::Duration = std::time::Duration::from_secs(20);
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// An agent's lifecycle state, normalised across the four generated enums that
/// describe the same thing.
///
/// Worth the conversion boilerplate: without it every caller matches on a
/// per-operation enum, and the "is this thing usable" question gets answered
/// slightly differently in each place it is asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Sleeping,
    Starting,
    Crashed,
    Failed,
    Deleting,
    /// A state this CLI predates. Treated as terminal — refusing to act on a
    /// state we cannot reason about beats guessing.
    Unknown(String),
}

impl Status {
    /// Lowercase label for tables and messages.
    pub fn label(&self) -> String {
        match self {
            Status::Running => "running".into(),
            Status::Sleeping => "sleeping".into(),
            Status::Starting => "starting".into(),
            Status::Crashed => "crashed".into(),
            Status::Failed => "failed".into(),
            Status::Deleting => "deleting".into(),
            Status::Unknown(s) => s.to_lowercase(),
        }
    }

    /// The agent exists and can be brought back: it is worth listing, waking,
    /// sleeping or connecting to.
    pub fn is_live(&self) -> bool {
        matches!(self, Status::Running | Status::Sleeping | Status::Starting)
    }
}

/// Generate `From` impls for the per-operation status enums, which are
/// structurally identical and separately generated.
macro_rules! status_from {
    ($($path:path),+ $(,)?) => {
        $(
            impl From<$path> for Status {
                fn from(status: $path) -> Self {
                    use $path as S;
                    match status {
                        S::RUNNING => Status::Running,
                        S::SLEEPING => Status::Sleeping,
                        S::STARTING => Status::Starting,
                        S::CRASHED => Status::Crashed,
                        S::FAILED => Status::Failed,
                        S::DELETING => Status::Deleting,
                        S::Other(other) => Status::Unknown(other),
                    }
                }
            }
        )+
    };
}

status_from!(
    queries::cloud_agent::CloudAgentStatus,
    queries::cloud_agents::CloudAgentStatus,
    queries::my_cloud_agents::CloudAgentStatus,
    mutations::cloud_agent_create::CloudAgentStatus,
);

/// One cloud agent, as every `railway ca` verb sees it.
#[derive(Clone, Debug)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub status: Status,
    pub project_id: String,
    pub environment_id: String,
    pub created_at: DateTime<Utc>,
}

/// Read one agent by id, scoped to its environment. `None` means it is gone —
/// deleted, or it belongs to another environment — which is the caller's cue to
/// forget the stored pointer rather than to fail.
pub async fn get(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
    id: &str,
) -> Result<Option<Agent>> {
    let res = post_graphql::<queries::CloudAgent, _>(
        client,
        backboard,
        queries::cloud_agent::Variables {
            id: id.to_owned(),
            environment_id: environment_id.to_owned(),
        },
    )
    .await?;
    Ok(res.cloud_agent.map(|a| Agent {
        id: a.id,
        name: a.name,
        status: a.status.into(),
        project_id: a.project_id,
        environment_id: a.environment_id,
        created_at: a.created_at,
    }))
}

/// Every agent the caller owns, across the whole account, in one request.
///
/// This is what makes the flat verbs work without a linked directory: agents
/// are addressed by name, and the name has to be looked up somewhere.
pub async fn list_mine(client: &reqwest::Client, backboard: &str) -> Result<Vec<Agent>> {
    let res = post_graphql::<queries::MyCloudAgents, _>(
        client,
        backboard,
        queries::my_cloud_agents::Variables {},
    )
    .await?;
    Ok(res
        .my_cloud_agents
        .into_iter()
        .map(|a| Agent {
            id: a.id,
            name: a.name,
            status: a.status.into(),
            project_id: a.project_id,
            environment_id: a.environment_id,
            created_at: a.created_at,
        })
        .collect())
}

/// Agents in one environment. `mine` is load-bearing rather than tidiness:
/// agents authorize per environment, so an unfiltered list includes teammates'
/// — and connecting to one would put this user's credentials on a box someone
/// else is working in.
pub async fn list_in_environment(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
    mine: bool,
) -> Result<Vec<Agent>> {
    let res = post_graphql::<queries::CloudAgents, _>(
        client,
        backboard,
        queries::cloud_agents::Variables {
            environment_id: environment_id.to_owned(),
            mine: Some(mine),
        },
    )
    .await?;
    Ok(res
        .cloud_agents
        .into_iter()
        .map(|a| Agent {
            id: a.id,
            name: a.name,
            status: a.status.into(),
            project_id: a.project_id,
            environment_id: a.environment_id,
            created_at: a.created_at,
        })
        .collect())
}

/// Layer the CLI's default variables under the caller's, for a new agent.
///
/// `SHELL` is a stopgap: the VM's session runner wraps every command in
/// `bash -c`, and bash self-assigns `$SHELL` without exporting it — so the
/// variable reads as set from a shell prompt but is invisible to child
/// processes. Codex desktop's remote bootstrap probes it from a `sh -c`
/// wrapper and refuses to start when it's empty, which a GUI surfaces as a
/// bare "SSH connection failed". A create-time variable lands in the VM's
/// real environment, so it is exported everywhere. Only at create: variables
/// don't reach an agent that already exists. A caller-supplied SHELL wins.
/// Remove once vm-init exports SHELL itself.
pub fn with_default_variables(variables: Option<serde_json::Value>) -> Option<serde_json::Value> {
    let mut map = match variables {
        None => serde_json::Map::new(),
        Some(serde_json::Value::Object(map)) => map,
        // Never produced by our callers (variables come from string maps);
        // reshaping someone else's value is worse than leaving it alone.
        Some(other) => return Some(other),
    };
    map.entry("SHELL")
        .or_insert_with(|| serde_json::Value::String("/bin/bash".to_owned()));
    Some(serde_json::Value::Object(map))
}

/// Create an agent. The VM only — no harness, no credential, no session.
/// Provisioning belongs to whatever opens a session on it, so an agent created
/// here works with any of them.
pub async fn create(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
    name: Option<String>,
    variables: Option<serde_json::Value>,
) -> Result<Agent> {
    let res = post_graphql::<mutations::CloudAgentCreate, _>(
        client,
        backboard,
        mutations::cloud_agent_create::Variables {
            input: mutations::cloud_agent_create::CloudAgentCreateInput {
                environment_id: environment_id.to_owned(),
                name,
                variables: with_default_variables(variables),
            },
        },
    )
    .await?
    .cloud_agent_create;
    Ok(Agent {
        id: res.id,
        name: res.name,
        status: res.status.into(),
        project_id: res.project_id,
        environment_id: res.environment_id,
        created_at: res.created_at,
    })
}

pub async fn wake(client: &reqwest::Client, backboard: &str, id: &str) -> Result<()> {
    post_graphql::<mutations::CloudAgentWake, _>(
        client,
        backboard,
        mutations::cloud_agent_wake::Variables { id: id.to_owned() },
    )
    .await?;
    Ok(())
}

/// Sleep an agent, flushing its filesystem first.
///
/// The flush is paired with the mutation here, rather than left to each caller,
/// because forgetting it loses the user's most recent work silently — see
/// [`crate::commands::code::flush_disk`]. Every path that suspends an agent goes
/// through this function for that reason; there is deliberately no way to reach
/// the bare mutation from outside this module.
pub async fn sleep(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
    id: &str,
) -> Result<()> {
    crate::commands::code::flush_disk(environment_id, id).await;
    sleep_without_flush(client, backboard, id).await
}

async fn sleep_without_flush(client: &reqwest::Client, backboard: &str, id: &str) -> Result<()> {
    post_graphql::<mutations::CloudAgentSleep, _>(
        client,
        backboard,
        mutations::cloud_agent_sleep::Variables { id: id.to_owned() },
    )
    .await?;
    Ok(())
}

pub async fn delete(client: &reqwest::Client, backboard: &str, id: &str) -> Result<()> {
    post_graphql::<mutations::CloudAgentDelete, _>(
        client,
        backboard,
        mutations::cloud_agent_delete::Variables { id: id.to_owned() },
    )
    .await?;
    Ok(())
}

/// Poll until the agent is RUNNING. A terminal state is reported immediately
/// rather than burning the whole timeout on a box that will never come up.
pub async fn wait_until_running(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
    id: &str,
) -> Result<Agent> {
    let started = std::time::Instant::now();
    let deadline = started + READY_TIMEOUT;
    loop {
        let agent = match get(client, backboard, environment_id, id).await? {
            Some(agent) => agent,
            None => bail!("Agent {id} disappeared while starting."),
        };
        match agent.status {
            Status::Running => return Ok(agent),
            Status::Starting | Status::Sleeping => {}
            Status::Crashed => bail!("Agent {} crashed while starting.", agent.name),
            Status::Failed => bail!("Agent {} failed to start.", agent.name),
            Status::Deleting => bail!("Agent {} is being deleted.", agent.name),
            Status::Unknown(ref s) => bail!("Agent {} is in an unknown state ({s}).", agent.name),
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "Agent {} did not reach running within {}s (last state: {}).",
                agent.name,
                READY_TIMEOUT.as_secs(),
                agent.status.label()
            );
        }
        let interval = if started.elapsed() < POLL_FAST_WINDOW {
            POLL_INTERVAL_FAST
        } else {
            POLL_INTERVAL
        };
        tokio::time::sleep(interval).await;
    }
}

/// A durable console session on an agent.
///
/// Sessions outlive the ssh that started them — that is the point of them — so
/// reconnecting means finding the session by name rather than starting a second
/// copy of the work alongside the first.
pub struct ConsoleSession {
    pub name: String,
    pub command: String,
    pub running: bool,
    pub attached: bool,
}

pub async fn list_sessions(
    client: &reqwest::Client,
    backboard: &str,
    agent_id: &str,
) -> Result<Vec<ConsoleSession>> {
    let res = post_graphql::<queries::CloudAgentConsoleSessions, _>(
        client,
        backboard,
        queries::cloud_agent_console_sessions::Variables {
            cloud_agent_id: agent_id.to_owned(),
        },
    )
    .await?;
    Ok(res
        .cloud_agent_console_sessions
        .map(|conn| {
            conn.edges
                .into_iter()
                .map(|edge| ConsoleSession {
                    name: edge.node.name,
                    command: edge.node.command,
                    running: edge.node.run_state.running,
                    attached: edge.node.attached,
                })
                .collect()
        })
        .unwrap_or_default())
}

/// How an agent was picked, so callers can say so.
pub enum Resolution {
    /// The caller named it.
    Named,
    /// It is the agent this machine last used in its environment.
    Remembered,
    /// It is the caller's only live agent in scope.
    Sole,
}

/// Find the agent a command should act on.
///
/// One order for every verb, and no interactive prompt anywhere in it — a
/// lifecycle command that stops to ask which agent it meant is unusable in a
/// script, and the ambiguous case has a better answer anyway: print the
/// candidates and let the next invocation name one.
///
/// Scope is the whole account unless `--project`/`--environment` narrows it,
/// because agents are cross-project and the directory you happen to be in is
/// about deploys.
pub async fn resolve(
    configs: &Configs,
    client: &reqwest::Client,
    selector: Option<&str>,
    environment_id: Option<&str>,
) -> Result<(Agent, Resolution)> {
    match resolve_or_none(configs, client, selector, environment_id).await? {
        Some(found) => Ok(found),
        None => bail!(
            "You have no cloud agents{}. Create one with `railway ca create`.",
            match environment_id {
                Some(_) => " in this environment",
                None => "",
            }
        ),
    }
}

/// [`resolve`], with the empty account handed back instead of an error.
///
/// `None` means exactly "no live agents in scope, and no name was given" —
/// the one case where a caller can reasonably do something other than fail,
/// e.g. a connect command creating the first agent. Every other outcome
/// (a named agent missing, more than one candidate) is still an error here,
/// because acting on a guess would touch the wrong machine.
pub async fn resolve_or_none(
    configs: &Configs,
    client: &reqwest::Client,
    selector: Option<&str>,
    environment_id: Option<&str>,
) -> Result<Option<(Agent, Resolution)>> {
    let backboard = configs.get_backboard();
    let candidates = match environment_id {
        Some(env) => list_in_environment(client, &backboard, env, true).await?,
        None => list_mine(client, &backboard).await?,
    };

    if let Some(selector) = selector {
        return match_selector(candidates, selector).map(|agent| Some((agent, Resolution::Named)));
    }

    let live: Vec<Agent> = candidates
        .into_iter()
        .filter(|a| a.status.is_live())
        .collect();

    // The pointer is what makes bare `railway ca ssh` mean "the agent I was
    // just working in" rather than "whichever one sorts first".
    let mut remembered: Vec<Agent> = live
        .iter()
        .filter(|a| configs.get_code_agent(&a.environment_id).as_deref() == Some(a.id.as_str()))
        .cloned()
        .collect();
    if remembered.len() == 1 {
        return Ok(Some((remembered.remove(0), Resolution::Remembered)));
    }

    match live.len() {
        0 => Ok(None),
        1 => Ok(Some((
            live.into_iter().next().expect("len checked"),
            Resolution::Sole,
        ))),
        _ => bail!(
            "You have {} cloud agents and none is this directory's. Name one:\n{}",
            live.len(),
            describe(&live)
        ),
    }
}

/// Match a user-supplied selector against candidates: an exact id first, then
/// an exact name. Ambiguity is reported rather than broken by picking one —
/// names are not unique across projects, and guessing here would act on the
/// wrong machine.
fn match_selector(candidates: Vec<Agent>, selector: &str) -> Result<Agent> {
    if let Some(agent) = candidates.iter().find(|a| a.id == selector) {
        return Ok(agent.clone());
    }
    let mut by_name: Vec<Agent> = candidates
        .into_iter()
        .filter(|a| a.name == selector)
        .collect();
    match by_name.len() {
        0 => bail!("No cloud agent named {selector:?}. `railway ca list` shows yours."),
        1 => Ok(by_name.remove(0)),
        _ => bail!(
            "{} cloud agents are named {selector:?}. Use an id:\n{}",
            by_name.len(),
            describe(&by_name)
        ),
    }
}

/// Candidate list for an ambiguity error: the id is what disambiguates, so it
/// is what this prints.
fn describe(agents: &[Agent]) -> String {
    agents
        .iter()
        .map(|a| format!("  {} ({}) — {}", a.name, a.status.label(), a.id))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Remember this agent as the environment's, and persist.
pub fn remember(configs: &mut Configs, agent: &Agent) -> Result<()> {
    configs.set_code_agent(&agent.environment_id, &agent.id);
    configs.write()
}

/// Forget an environment's agent pointer, and persist.
///
/// Called on delete whether or not the delete reported success: a mutation that
/// fails on an already-gone agent must not leave the CLI reaching for it
/// forever.
pub fn forget(configs: &mut Configs, environment_id: &str) -> Result<()> {
    configs.remove_code_agent(environment_id);
    configs.write()
}

/// Compact age for list output — the column answers "is this one stale", which
/// needs one unit, not a timestamp.
pub fn humanize_age(created_at: DateTime<Utc>) -> String {
    let seconds = Utc::now().signed_duration_since(created_at).num_seconds();
    if seconds < 0 {
        return "just now".into();
    }
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, name: &str, status: Status) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            status,
            project_id: "project".into(),
            environment_id: "env".into(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn selector_matches_id_before_name() {
        // A name that happens to equal another agent's id must not win over the
        // agent that actually has that id.
        let candidates = vec![
            agent("abc", "first", Status::Running),
            agent("def", "abc", Status::Running),
        ];
        let found = match_selector(candidates, "abc").unwrap();
        assert_eq!(found.name, "first");
    }

    #[test]
    fn selector_matches_name() {
        let candidates = vec![agent("abc", "sunny-cloud", Status::Sleeping)];
        assert_eq!(match_selector(candidates, "sunny-cloud").unwrap().id, "abc");
    }

    #[test]
    fn duplicate_names_are_ambiguous_rather_than_guessed() {
        let candidates = vec![
            agent("abc", "dev", Status::Running),
            agent("def", "dev", Status::Running),
        ];
        let err = match_selector(candidates, "dev").unwrap_err().to_string();
        assert!(err.contains("abc"), "error should list ids: {err}");
        assert!(err.contains("def"), "error should list ids: {err}");
    }

    #[test]
    fn unknown_selector_points_at_list() {
        let err = match_selector(vec![agent("abc", "dev", Status::Running)], "nope")
            .unwrap_err()
            .to_string();
        assert!(err.contains("railway ca list"), "{err}");
    }

    #[test]
    fn terminal_states_are_not_live() {
        assert!(Status::Running.is_live());
        assert!(Status::Sleeping.is_live());
        assert!(Status::Starting.is_live());
        assert!(!Status::Crashed.is_live());
        assert!(!Status::Failed.is_live());
        assert!(!Status::Deleting.is_live());
        assert!(!Status::Unknown("wat".into()).is_live());
    }

    #[test]
    fn shell_is_seeded_when_absent() {
        // No variables at all still yields SHELL — Codex's remote bootstrap
        // dies without it, and this is the only moment it can be set.
        let vars = with_default_variables(None).unwrap();
        assert_eq!(vars["SHELL"], "/bin/bash");

        let vars = with_default_variables(Some(serde_json::json!({ "FOO": "bar" }))).unwrap();
        assert_eq!(vars["SHELL"], "/bin/bash");
        assert_eq!(vars["FOO"], "bar");
    }

    #[test]
    fn a_callers_shell_wins_over_the_default() {
        let vars =
            with_default_variables(Some(serde_json::json!({ "SHELL": "/bin/zsh" }))).unwrap();
        assert_eq!(vars["SHELL"], "/bin/zsh");
    }

    #[test]
    fn age_uses_one_unit() {
        assert_eq!(
            humanize_age(Utc::now() - chrono::Duration::seconds(30)),
            "30s"
        );
        assert_eq!(
            humanize_age(Utc::now() - chrono::Duration::minutes(5)),
            "5m"
        );
        assert_eq!(humanize_age(Utc::now() - chrono::Duration::hours(3)), "3h");
        assert_eq!(humanize_age(Utc::now() - chrono::Duration::days(9)), "9d");
    }
}
