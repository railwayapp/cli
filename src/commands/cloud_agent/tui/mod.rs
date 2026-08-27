//! The `railway ca` TUI: terminal lifecycle, the async event loop, and the
//! data it runs on.
//!
//! Structure comes from one `UserProjects` call before the first frame — that
//! query already returns every workspace, project and environment, so the tree
//! costs nothing to draw. Agents are the expensive part (`cloudAgents` takes a
//! single environment) and load in the background when an environment is
//! expanded.
//!
//! A launch runs *inside* the TUI: the pipeline in [`crate::commands::code`]
//! reports its steps through a [`Progress`] sink that forwards them to the
//! loading screen, and the finished session opens in the right-hand pane rather
//! than taking the whole terminal. The one thing that cannot happen here is an
//! interactive Claude mint — it wants a browser and a pasted token — so the loop
//! hands the terminal back for that and the caller re-enters with the same
//! request.

pub mod app;
pub mod session;
pub mod settings;
pub mod theme;
mod ui;
pub mod wizard;

use std::io::{Write, stdout};
use std::panic;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use app::{
    Agent, AgentOp, ConsoleSession, EnvNode, HeldConnect, Load, LoadSessions, ProjectNode,
    SshKeyOffer, SshKeyState, WorkspaceNode,
};
pub use app::{App, Effect, LaunchRequest, Screen, Target};

use crate::client::post_graphql;
use crate::commands::code::{self, LaunchArgs, Prepared, Progress};
use crate::config::Configs;
use crate::gql::{mutations, queries};

/// Create the project first-run setup offers to make, in the workspace the
/// target step's "+ Create a project" row was chosen under.
async fn create_default_project(
    client: &reqwest::Client,
    backboard: &str,
    workspace_id: String,
) -> Result<wizard::ProjectOption> {
    use crate::gql::mutations;

    let created = post_graphql::<mutations::ProjectCreate, _>(
        client,
        backboard.to_string(),
        mutations::project_create::Variables {
            name: Some("Cloud Agents".to_string()),
            description: Some("Home for Railway cloud agents".to_string()),
            workspace_id: Some(workspace_id),
        },
    )
    .await?
    .project_create;

    let environment = created
        .environments
        .edges
        .first()
        .ok_or_else(|| anyhow::anyhow!("the new project has no environment"))?;
    Ok(wizard::ProjectOption {
        project_id: created.id,
        project_name: created.name,
        environment_id: environment.node.id.clone(),
        environment_name: environment.node.name.clone(),
    })
}

/// Write what first-run setup collected.
fn save_setup(
    app: &App,
    outcome: &wizard::Outcome,
) -> Result<crate::commands::cloud_agent::prefs::AgentPrefs> {
    use crate::commands::cloud_agent::prefs::{AgentPrefs, DefaultProject, SkillsPrefs};

    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let prefs = AgentPrefs {
        version: crate::commands::cloud_agent::prefs::CURRENT_VERSION,
        agent: Some(outcome.agent.clone()),
        skills: SkillsPrefs {
            enabled: outcome.skills,
            source: outcome.skills_source.clone(),
            exclude: Vec::new(),
        },
        // The wizard doesn't ask about MCP import; keep what the file has (or
        // the on-by-default for a first run).
        mcp: AgentPrefs::load_in(&home)
            .map(|p| p.mcp)
            .unwrap_or_default(),
        default_project: outcome.project.as_ref().map(|p| DefaultProject {
            project_id: p.project_id.clone(),
            project_name: p.project_name.clone(),
            environment_id: p.environment_id.clone(),
            environment_name: p.environment_name.clone(),
        }),
        theme: Some(outcome.theme.clone()),
    };
    let _ = app;
    prefs.save_in(&home)?;
    Ok(prefs)
}

/// Write a change made on the ⌥s settings card.
///
/// Merged over the file rather than built fresh: the card saves on every
/// change, and the skills exclude list — which no card edits — must survive
/// a stroll through the settings untouched.
fn save_settings(
    outcome: &wizard::Outcome,
) -> Result<crate::commands::cloud_agent::prefs::AgentPrefs> {
    use crate::commands::cloud_agent::prefs::{AgentPrefs, DefaultProject};

    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let mut prefs = AgentPrefs::load_in(&home).unwrap_or_default();
    prefs.version = crate::commands::cloud_agent::prefs::CURRENT_VERSION;
    prefs.agent = Some(outcome.agent.clone());
    prefs.skills.enabled = outcome.skills;
    prefs.skills.source = outcome.skills_source.clone();
    prefs.default_project = outcome.project.as_ref().map(|p| DefaultProject {
        project_id: p.project_id.clone(),
        project_name: p.project_name.clone(),
        environment_id: p.environment_id.clone(),
        environment_name: p.environment_name.clone(),
    });
    prefs.theme = Some(outcome.theme.clone());
    prefs.save_in(&home)?;
    Ok(prefs)
}

/// Persist a settings-card change and bring the session along with it.
///
/// Quiet on success — the card itself shows the new value, and a status line
/// per keypress while cycling a theme would be noise. Failure says so: a save
/// that silently didn't happen is the worst thing a settings card can do.
fn apply_settings(app: &mut App, outcome: &wizard::Outcome) {
    match save_settings(outcome) {
        // There are preferences now, whatever there was before.
        Ok(_) => app.configured = true,
        Err(err) => app.toast_error(format!("Couldn't save your settings: {err:#}")),
    }
    app.set_harness(Some(&outcome.agent));
    app.set_theme(Some(&outcome.theme));
    app.skills_enabled = outcome.skills;
    match &outcome.project {
        Some(project) => {
            app.default_project = Some(project.project_id.clone());
            app.target = Some(Target {
                project_id: project.project_id.clone(),
                project_name: project.project_name.clone(),
                environment_id: project.environment_id.clone(),
                environment_name: project.environment_name.clone(),
            });
        }
        // "Decide later": the default is gone, but the target stays aimed for
        // this run — clearing the default is not pointing the prompt away.
        None => app.default_project = None,
    }
}

/// Shorten a string for a toast, keeping the front — the host and the start of
/// the path are what identify a link.
fn elide(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_string();
    }
    chars[..width.saturating_sub(1)].iter().collect::<String>() + "…"
}

/// Remember the default project on its own, leaving every other preference
/// alone — this is one answer changing, not the setup flow running again.
fn save_default_project(target: &Target) -> Result<()> {
    use crate::commands::cloud_agent::prefs::{AgentPrefs, DefaultProject};

    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let mut prefs = AgentPrefs::load_in(&home).unwrap_or_default();
    prefs.default_project = Some(DefaultProject {
        project_id: target.project_id.clone(),
        project_name: target.project_name.clone(),
        environment_id: target.environment_id.clone(),
        environment_name: target.environment_name.clone(),
    });
    prefs.save_in(&home)
}

/// The `ssh` command that reaches one session from any terminal.
///
/// The same shape the dashboard hands out: the relay target is a username, the
/// session is named through `SetEnv`, and the port only appears when the relay
/// is not on 22.
fn ssh_command_for(environment_id: &str, agent_id: &str, session_name: &str) -> String {
    let (host, port) = Configs::get_ssh_relay();
    let port = match port {
        Some(port) if port != 22 => format!("-p {port} "),
        _ => String::new(),
    };
    format!(
        "ssh {port}-o SetEnv=RAILWAY_DURABLE_SESSION_NAME={session_name} agent:{environment_id}:{agent_id}@{host}"
    )
}

/// How often the loading spinner advances. Fast enough to read as motion,
/// slow enough that a launch does not redraw the screen hundreds of times.
const SPINNER_TICK: std::time::Duration = std::time::Duration::from_millis(110);

/// The frame budget while messages are still queued. Session output wakes the
/// loop once per read — a reattach replaying a long session's recording is
/// thousands of wakes in a burst, and painting a full frame for each turned
/// the replay into a visible slideshow. Worse than cosmetic: every frame takes
/// the emulator lock the pty reader needs, and a starved reader stops draining
/// ssh — the far side backs up until the keepalive replies stop arriving and
/// ssh kills the connection mid-replay. While the queue is non-empty, frames
/// are capped at ~30/s; the moment it drains the next frame is immediate, so
/// interactive latency is untouched.
const FLOOD_FRAME: std::time::Duration = std::time::Duration::from_millis(33);

/// Why the TUI gave the terminal back.
pub enum Outcome {
    /// A Claude credential has to be minted, which needs the real terminal.
    /// The caller mints and re-enters with the same request.
    NeedsCredential(LaunchRequest),
    /// Give the whole terminal to one session, then come back.
    FullScreen(FullScreenRequest),
    Quit,
}

/// Everything needed to reattach to a session outside the TUI.
pub struct FullScreenRequest {
    pub ssh_target: String,
    pub identity: Option<std::path::PathBuf>,
    pub relay_opts: Vec<String>,
    pub session_name: String,
    pub agent_name: String,
}

/// Everything the loop reacts to besides keystrokes.
enum Message {
    AgentsLoaded {
        path: (usize, usize, usize),
        result: Result<Vec<Agent>, String>,
    },
    /// The whole account's agents in one request, keyed by environment — or
    /// why that wasn't possible, in which case startup degrades to the
    /// per-environment path.
    MyAgentsLoaded {
        result: Result<Vec<(String, Agent)>, String>,
        /// When the request went out, which decides what this snapshot is
        /// allowed to overwrite: anything the tree has heard since is fresher
        /// news. See [`App::my_agents_loaded`].
        asked_at: std::time::Instant,
    },
    /// A background fetch was refused for rate limiting. The rest of its batch
    /// is abandoned; see [`spawn_sweep`].
    RateLimited {
        retry_after_secs: Option<u64>,
    },
    SessionsLoaded {
        path: (usize, usize, usize, usize),
        /// Applied by id, not by the index the request was issued with: the
        /// environment can be refetched while sessions are in flight, and a
        /// new agent shifting the list would attach these to the wrong row.
        agent_id: String,
        result: Result<Vec<ConsoleSession>, String>,
    },
    LaunchStep(String),
    LaunchReady(Box<Prepared>, Box<LaunchRequest>),
    LaunchFailed(String),
    /// A lifecycle mutation was accepted, or failed. Sleep and wake are not
    /// finished at this point — see [`App::agent_op_finished`].
    AgentOpDone {
        agent_id: String,
        environment_id: String,
        op: AgentOp,
        error: Option<String>,
    },
    ReattachReady {
        agent_id: String,
        agent_name: String,
        session_name: String,
        info: Box<code::ConnectInfo>,
    },
    /// A foreground reattach's ssh info didn't come back. Carries the name so
    /// the row's spinner comes off before the failure is announced.
    ReattachFailed {
        session_name: String,
        error: String,
    },
    /// A background auto-connect resolved its ssh info; the pane opens
    /// quietly — no focus steal, no screen change.
    AutoReattachReady {
        agent_id: String,
        agent_name: String,
        session_name: String,
        info: Box<code::ConnectInfo>,
    },
    /// A background auto-connect failed. Quiet too: the spinner comes off and
    /// the reason rides the status line, not a toast.
    AutoConnectFailed {
        session_name: String,
        error: String,
    },
    SessionKilled {
        agent_id: String,
        session_name: String,
        error: Option<String>,
    },
    ProjectCreated(Result<wizard::ProjectOption, String>),
    /// The gate's key registration finished. On success the held connect
    /// resumes as the effect it was before the gate held it.
    SshKeyRegistered {
        result: Result<(), String>,
        then: Option<HeldConnect>,
    },
    /// The under-frame Claude mint finished: the launch resumes on success,
    /// and steps out for the manual-paste fallback on failure.
    ClaudeMintDone {
        ok: bool,
        req: Box<LaunchRequest>,
    },
    /// Ask again for one agent's sessions.
    RefreshAgentSessions(String),
    /// The session produced output, so the screen needs redrawing.
    SessionOutput,
}

/// Forwards launch-pipeline steps into the loading screen.
struct ChannelProgress(mpsc::UnboundedSender<Message>);

impl Progress for ChannelProgress {
    fn step(&self, text: &str) {
        let _ = self.0.send(Message::LaunchStep(text.to_string()));
    }
    fn note(&self, text: &str) {
        let _ = self.0.send(Message::LaunchStep(text.to_string()));
    }
    fn finish(&self) {}
}

/// A launch pipeline already running before the TUI took the screen, plus the
/// request that started it. `railway code` starts the pipeline beside the
/// tree load instead of after it — the tree is for drawing the manage screen,
/// not something the launch consumes — and the loop adopts this on frame one
/// exactly where it would otherwise have dispatched the autostart.
pub(crate) struct InflightLaunch {
    pub(crate) req: LaunchRequest,
    rx: mpsc::UnboundedReceiver<Message>,
}

impl InflightLaunch {
    /// Wait (bounded) for the running pipeline to settle, for a caller that is
    /// about to abort with an error: the pipeline may already have CREATED an
    /// agent, and exiting without saying so would orphan a billing VM with no
    /// user-visible record. Returns the line to print when there is one.
    pub(crate) async fn settle_for_abort(mut self, limit: std::time::Duration) -> Option<String> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Ok(Some(Message::LaunchReady(prepared, _))) => {
                    return Some(format!(
                        "A launch was already in flight and created agent {} — `railway ca ssh {}` reattaches it, `railway ca delete {}` removes it.",
                        prepared.agent_name, prepared.agent_name, prepared.agent_name
                    ));
                }
                // Failed before creating anything worth reporting.
                Ok(Some(Message::LaunchFailed(_))) => return None,
                Ok(Some(_)) => continue,
                // Pipeline gone or still running at the deadline: point at the
                // list rather than guessing.
                Ok(None) => return None,
                Err(_) => {
                    return Some(
                        "A launch was still in flight — check `railway ca list` for an agent it may have created.".to_string(),
                    );
                }
            }
        }
    }
}

/// Run the prepare pipeline for `req`, streaming progress and the outcome into
/// `sink` as loop messages. Shared by [`start_launch`] (sink = the loop's own
/// channel) and [`begin_launch_early`] (sink = a buffer the loop adopts later).
fn spawn_prepare(req: LaunchRequest, sink: mpsc::UnboundedSender<Message>) {
    tokio::spawn(async move {
        let req = req;
        let args = launch_args_for(&req);
        let progress = ChannelProgress(sink.clone());
        let message = match code::prepare(&args, &progress, code::SessionStyle::Pane).await {
            Ok(prepared) => Message::LaunchReady(Box::new(prepared), Box::new(req)),
            Err(err) => Message::LaunchFailed(format!("{err:#}")),
        };
        let _ = sink.send(message);
    });
}

/// Start `req`'s pipeline now, before the TUI exists, buffering its messages
/// until the loop adopts them. ONLY for launches every dispatch gate would
/// wave through — the caller must have verified the SSH key is registered and
/// no interactive Claude mint is needed, because a pipeline started here has
/// no frame to raise those questions in and would surface them as failures.
pub(crate) fn begin_launch_early(req: LaunchRequest) -> InflightLaunch {
    let (tx, rx) = mpsc::unbounded_channel();
    spawn_prepare(req.clone(), tx);
    InflightLaunch { req, rx }
}

/// Build the tree from the workspace listing. Deleted projects are dropped, and
/// so are environments the caller cannot access — an agent can't be listed or
/// created in either, so showing them would only offer dead ends.
pub async fn load_tree(client: &reqwest::Client, configs: &Configs) -> Result<Vec<WorkspaceNode>> {
    let workspaces = crate::workspace::workspaces_with_client(client, configs).await?;
    Ok(workspaces
        .into_iter()
        .map(|ws| WorkspaceNode {
            id: ws.id().to_string(),
            name: ws.name().to_string(),
            expanded: false,
            projects: ws
                .projects()
                .into_iter()
                .filter(|p| p.deleted_at().is_none())
                .map(|p| ProjectNode {
                    id: p.id().to_string(),
                    name: p.name().to_string(),
                    expanded: false,
                    envs: p
                        .environments()
                        .into_iter()
                        .filter(|e| e.can_access)
                        .map(|e| EnvNode {
                            id: e.id,
                            name: e.name,
                            expanded: false,
                            agents: Load::NotLoaded,
                        })
                        .collect(),
                })
                .filter(|p| !p.envs.is_empty())
                .collect(),
        })
        .filter(|ws| !ws.projects.is_empty())
        .collect())
}

/// The caller's own agents in one environment.
///
/// `mine` matches what the launcher does: agents hold the credentials of the
/// member they act as, so connecting to a teammate's would write this user's
/// credential onto a box someone else is working in.
async fn fetch_agents(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
) -> Result<Vec<Agent>> {
    let res = post_graphql::<queries::CloudAgents, _>(
        client,
        backboard,
        queries::cloud_agents::Variables {
            environment_id: environment_id.to_owned(),
            mine: Some(true),
        },
    )
    .await?;
    Ok(res
        .cloud_agents
        .into_iter()
        .map(|a| Agent {
            id: a.id,
            name: a.name,
            status: format!("{:?}", a.status).to_lowercase(),
            sessions: LoadSessions::NotLoaded,
            expanded: false,
        })
        .collect())
}

/// Every agent the caller owns, keyed by environment, in one request.
///
/// `myCloudAgents` answers for the whole account what the startup sweep used
/// to ask environment by environment. A backboard that predates the field
/// reports it as unqueryable, which is the caller's cue to fall back to that
/// sweep.
async fn fetch_my_agents(
    client: &reqwest::Client,
    backboard: &str,
) -> Result<Vec<(String, Agent)>> {
    let res = post_graphql::<queries::MyCloudAgents, _>(
        client,
        backboard,
        queries::my_cloud_agents::Variables {},
    )
    .await?;
    Ok(res
        .my_cloud_agents
        .into_iter()
        .map(|a| {
            (
                a.environment_id,
                Agent {
                    id: a.id,
                    name: a.name,
                    status: format!("{:?}", a.status).to_lowercase(),
                    sessions: LoadSessions::NotLoaded,
                    expanded: false,
                },
            )
        })
        .collect())
}

/// Ask for the whole account's agents in the background: at startup, on ⌥r, and
/// on every automatic refresh.
fn spawn_my_agents_fetch(
    tx: &mpsc::UnboundedSender<Message>,
    client: &reqwest::Client,
    backboard: &str,
) {
    let tx = tx.clone();
    let client = client.clone();
    let backboard = backboard.to_string();
    // Stamped before the request goes out, not when the reply is applied: the
    // snapshot describes the account as it was at this moment, and that is what
    // it may not overwrite anything newer than.
    let asked_at = std::time::Instant::now();
    tokio::spawn(async move {
        match fetch_my_agents(&client, &backboard).await {
            Ok(agents) => {
                let _ = tx.send(Message::MyAgentsLoaded {
                    result: Ok(agents),
                    asked_at,
                });
            }
            // A rate limit is worth its own message — the toast with the
            // Retry-After — rather than being folded into the fallback, which
            // would immediately spend more requests against a refusal.
            Err(err) => match rate_limit_from(&err) {
                Some(retry_after_secs) => {
                    let _ = tx.send(Message::RateLimited { retry_after_secs });
                }
                None => {
                    let _ = tx.send(Message::MyAgentsLoaded {
                        result: Err(err.to_string()),
                        asked_at,
                    });
                }
            },
        }
    });
}

/// Ask the platform for everything again.
///
/// One request for the whole account (see [`fetch_my_agents`]), and the sessions
/// of the agents someone is looking at once it lands — not a sweep. Every
/// refresh in the TUI comes through here: ⌥r, the automatic tick, the re-entry
/// after the terminal was handed back, and `shift+r` on an account that is
/// already fully loaded.
///
/// Coalesced on [`App::refreshing`], so holding the chord or having three
/// actions finish at once cannot stack account-wide queries.
fn start_refresh(
    app: &mut App,
    tx: &mpsc::UnboundedSender<Message>,
    client: &reqwest::Client,
    backboard: &str,
) {
    if app.refreshing {
        return;
    }
    app.refresh_started();
    // Without `myCloudAgents` there is no account-wide question to ask, so the
    // refresh asks per environment — but only about the ones that already have
    // an answer or are open, never the whole account. Finding agents in
    // environments that have never loaded stays `shift+r`, a deliberate act,
    // because that is the one that costs a request each.
    if app.account_query_unavailable {
        let effects = app.environments_to_refresh();
        // Each environment answers with its own `AgentsLoaded`, so there is no
        // one reply to close the refresh out on: it is done being started, and
        // the sweep's own limiter bounds what is in flight from here.
        app.refresh_finished();
        if !effects.is_empty() {
            spawn_sweep(effects, tx, client, backboard, Default::default());
        }
        return;
    }
    spawn_my_agents_fetch(tx, client, backboard);
}

/// The reattachable shell and exec sessions on one agent's VM.
///
/// These are the platform's own record of what is running in there, so they
/// survive our disconnects — and each other's. Attaching is by name, which the
/// relay resolves.
async fn fetch_sessions(
    client: &reqwest::Client,
    backboard: &str,
    cloud_agent_id: &str,
) -> Result<Vec<ConsoleSession>> {
    let res = post_graphql::<queries::CloudAgentConsoleSessions, _>(
        client,
        backboard,
        queries::cloud_agent_console_sessions::Variables {
            cloud_agent_id: cloud_agent_id.to_owned(),
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
                    kind: format!("{:?}", edge.node.kind),
                    command: Some(edge.node.command),
                    running: edge.node.run_state.running,
                    attached: edge.node.attached,
                    created_at: edge.node.created_at,
                })
                .collect()
        })
        .unwrap_or_default())
}

pub async fn run(
    app: &mut App,
    client: reqwest::Client,
    backboard: String,
    pending: Option<LaunchRequest>,
) -> Result<Outcome> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));
    let mut terminal = setup_terminal()?;
    let _cleanup = scopeguard::guard((), |_| restore_terminal());

    let mut events = EventStream::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    // A launch the caller had to step outside for (a Claude mint) resumes here.
    if let Some(req) = pending {
        start_launch(app, req, &tx);
    }

    // Ask for every agent the caller owns, in one request. If the platform
    // predates `myCloudAgents`, the reply says so and startup degrades to
    // loading just the environments a keypress would immediately need.
    //
    // Every entry, not just the first: this is also how a re-entry catches up.
    // Coming back from a full-screen session — or from the Claude mint — used to
    // land on the snapshot taken before leaving, sometimes an hour old, because
    // the guard here skipped the request whenever the tree had already settled
    // and the settle would have discarded the answer anyway. A reply that may
    // overwrite what it is newer than is not discarded, so the request is worth
    // spending on every entry.
    let stop_fetching: StopFlag = Default::default();
    start_refresh(app, &tx, &client, &backboard);

    // When the previous frame was painted, for the flood cap below.
    let mut last_frame = std::time::Instant::now() - FLOOD_FRAME;

    loop {
        // Coalesce frames under load: with more messages already waiting,
        // painting now just repeats a screen that is about to change again.
        // See [`FLOOD_FRAME`]. Everything the skipped frame would have shown
        // is still in the emulator; the frame after the burst shows it all.
        if rx.is_empty() || last_frame.elapsed() >= FLOOD_FRAME {
            let mut rects = app.panes;
            let mut copied: Option<String> = None;
            terminal.draw(|f| {
                let (r, text) = ui::render_with_layout(app, f);
                rects = r;
                copied = text;
            })?;
            last_frame = std::time::Instant::now();
            app.panes = rects;
            if app.pending_copy.take().is_some() {
                finish_copy(app, copied);
            }
            sync_session_size(app, &terminal);
        }

        // A pipeline `railway code` already started beside the tree load:
        // adopt it — the same screen moves as a dispatched launch, minus the
        // gates, which were verified before it was allowed to start (see
        // `begin_launch_early`).
        if let Some(inflight) = app.autostart_inflight.take() {
            let InflightLaunch { req, mut rx } = inflight;
            app.screen = Screen::Manage;
            if let Some(Effect::LoadAgents {
                environment_id,
                path,
            }) = app.reveal_environment(&req.environment_id)
            {
                spawn_env_agents_fetch(environment_id, path, &tx, &client, &backboard);
            }
            app.start_loading(&req);
            let tx = tx.clone();
            tokio::spawn(async move {
                while let Some(message) = rx.recv().await {
                    if tx.send(message).is_err() {
                        break;
                    }
                }
            });
            continue;
        }

        // A launch the caller arrived with, started once the first frame is on
        // screen so the terminal is already in the state the loading pane will
        // draw into. Routed through the effect handling rather than straight to
        // `start_launch`, so it meets the ssh-key gate and the Claude mint the
        // same way a launch someone pressed a key for does.
        if let Some(req) = app.autostart.take() {
            dispatch_launch(app, req, &tx, &client, &backboard);
            continue;
        }

        let effect = tokio::select! {
            // Background work first: draining it keeps the tree and the session
            // honest even while keys arrive faster than frames.
            Some(message) = rx.recv() => handle_message(app, message, &tx, &client, &backboard, &stop_fetching),
            // Animate the loading screen. Only armed while it is showing, so an
            // idle TUI still blocks rather than spinning on a timer.
            // The wizard and settings borrow the same tick for their
            // "creating…" spinners, and the tree's connecting rows too.
            _ = tokio::time::sleep(SPINNER_TICK), if app.loading.active
                || !app.connecting.is_empty()
                || app.wizard.as_ref().is_some_and(|w| w.busy.is_some())
                || app.settings.as_ref().is_some_and(|s| s.busy.is_some()) => {
                app.tick();
                None
            }
            // A toast fades on its own, so the loop has to wake for it — an
            // idle TUI blocks on the keyboard and would otherwise leave it on
            // screen until the next keypress.
            _ = tokio::time::sleep(app.toast_remaining()), if app.toast.is_some() => {
                app.expire_toast();
                None
            }
            // An agent that has been told to wake is asked about again until it
            // says it is running. The platform accepts the wake long before the
            // VM is up, and one refetch just puts "sleeping" back on the row.
            _ = tokio::time::sleep(app::WATCH_TICK), if app.watching_agents() => {
                app.watch_tick()
            }
            // Everything else that changes an agent happens outside this
            // process: another terminal, the dashboard, a teammate. One
            // account-wide request every [`app::AUTO_REFRESH_EVERY`] is what
            // keeps the tree from being a snapshot of when it opened. Armed only
            // when a refresh would be right — see [`App::auto_refresh_in`] — so
            // an idle TUI still blocks on the keyboard, and the remainder is
            // recomputed each pass so an early wake just re-arms.
            _ = tokio::time::sleep(app.auto_refresh_in().unwrap_or(std::time::Duration::MAX)),
                if app.auto_refresh_in().is_some() => {
                if app.auto_refresh_due() {
                    start_refresh(app, &tx, &client, &backboard);
                }
                None
            }
            // A reattach that stays silent gets its "no response" notice drawn
            // once the stall clock runs out; nothing else would redraw, since
            // a silent pane by definition sends no output to wake the loop.
            _ = tokio::time::sleep(app.stall_check_remaining().unwrap_or(std::time::Duration::MAX)),
                if app.stall_check_remaining().is_some() => None,
            // An ended pane whose finished/dropped call is still waiting on
            // ssh's exit status: the EOF that woke the loop can beat waitpid,
            // and the reader sends no further wake — poll until the status
            // lands and the reap can decide.
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)),
                if app.awaiting_exit_status() => app.reap_ended_sessions(),
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => app.on_key(key),
                Some(Ok(Event::Paste(text))) => app.on_paste(text),
                Some(Ok(Event::Mouse(mouse))) => {
                    let action = match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => Some(app::MouseAction::Down),
                        MouseEventKind::Drag(MouseButton::Left) => Some(app::MouseAction::Drag),
                        MouseEventKind::Up(MouseButton::Left) => Some(app::MouseAction::Up),
                        MouseEventKind::ScrollUp => Some(app::MouseAction::ScrollUp),
                        MouseEventKind::ScrollDown => Some(app::MouseAction::ScrollDown),
                        _ => None,
                    };
                    // The mouse can ask for work too — clicking a collapsed row
                    // needs its children fetched.
                    let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
                    action.and_then(|action| {
                        app.on_mouse_shifted(action, mouse.column, mouse.row, shift)
                    })
                }
                Some(Ok(Event::Resize(..))) => { terminal.clear()?; None }
                // The stream ending means stdin closed — treat it as a quit
                // rather than spinning on an exhausted stream.
                None => Some(Effect::Quit),
                _ => None,
            },
        };

        // Background auto-connect: every listed session on a running agent
        // gets a pane without being asked for one, so the tree comes up
        // green instead of waiting to be clicked through. Checked each pass —
        // candidates only exist once loads land — and each session is tried
        // once per run, so a failure or a deliberate close stays closed.
        for connect in app.take_auto_connects() {
            spawn_auto_connect(connect, &tx);
        }

        match effect {
            None => {}
            Some(Effect::Quit) => {
                // Panes detach; agents stay running. Sleeping is a deliberate
                // act (`s` on the tree, `railway ca sleep`) — an automatic
                // sleep here killed every session's process while the
                // platform kept listing the sessions as running, and the next
                // reattach landed on a dead name and a blank pane.
                while let Some(mut session) = app.take_session(0) {
                    session.detach();
                }
                return Ok(Outcome::Quit);
            }
            Some(Effect::FullScreen {
                agent_id,
                session_name,
                agent_name,
            }) => {
                // The pane's ssh has to go first: two clients attached to one
                // durable session would fight over its screen.
                let Some(index) = app.sessions.iter().position(|s| s.agent_id == agent_id) else {
                    continue;
                };
                let Some(session) = app.detach_session(index) else {
                    continue;
                };
                return Ok(Outcome::FullScreen(FullScreenRequest {
                    ssh_target: session.ssh_target.clone(),
                    identity: session.identity.clone(),
                    relay_opts: session.relay_opts.clone(),
                    session_name,
                    agent_name,
                }));
            }
            Some(Effect::Reattach {
                agent_id,
                agent_name,
                environment_id,
                session_name,
            }) => {
                // Same SSH gate as a launch: reattaching is a fresh ssh, and
                // a key deregistered since the session opened would otherwise
                // land on the relay's interactive signup screen.
                if app.hold_for_ssh_key(HeldConnect::Reattach {
                    agent_id: agent_id.clone(),
                    agent_name: agent_name.clone(),
                    environment_id: environment_id.clone(),
                    session_name: session_name.clone(),
                }) {
                    continue;
                }
                // The spinner goes on now — connect_info takes a beat, and a
                // row that does nothing for it reads as a dead key.
                app.connecting.insert(session_name.clone());
                let tx = tx.clone();
                tokio::spawn(async move {
                    let message = match code::connect_info(&environment_id, &agent_id).await {
                        Ok(info) => Message::ReattachReady {
                            agent_id,
                            agent_name,
                            session_name,
                            info: Box::new(info),
                        },
                        Err(err) => {
                            let error = format!("{err:#}");
                            super::telemetry::track_session_event(
                                "reattach_connect_failed",
                                Some(error.as_str()),
                            )
                            .await;
                            Message::ReattachFailed {
                                session_name,
                                error,
                            }
                        }
                    };
                    let _ = tx.send(message);
                });
            }
            Some(Effect::StepOutForMint(req)) => {
                return Ok(Outcome::NeedsCredential(req));
            }
            Some(Effect::RegisterSshKey { offer, then }) => {
                let tx = tx.clone();
                let client = client.clone();
                tokio::spawn(async move {
                    let result = register_gate_key(&client, &offer).await;
                    if let Err(message) = &result {
                        crate::commands::ssh::tel::report_failure_for(
                            "cloud_agent_launch",
                            "ssh_key_register",
                            message,
                        )
                        .await;
                    }
                    let _ = tx.send(Message::SshKeyRegistered { result, then });
                });
            }
            Some(Effect::CreateDefaultProject(workspace_id)) => {
                let tx = tx.clone();
                let client = client.clone();
                let backboard = backboard.clone();
                tokio::spawn(async move {
                    let result = create_default_project(&client, &backboard, workspace_id)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(Message::ProjectCreated(result));
                });
            }
            Some(Effect::SaveSetup(outcome)) => {
                match save_setup(app, &outcome) {
                    Ok(prefs) => {
                        tokio::spawn(async move {
                            super::telemetry::track_setup_saved("wizard", &prefs).await;
                        });
                        // Setup is where the account gets ready to connect, so
                        // an unregistered key is offered here too — not just
                        // at the first launch that would trip over it.
                        app.offer_ssh_key_setup();
                        app.status = "Saved — Setup again to change it".into();
                    }
                    Err(err) => {
                        let message = format!("{err:#}");
                        let telemetry_message = message.clone();
                        tokio::spawn(async move {
                            super::telemetry::track_setup_failed("wizard", &telemetry_message)
                                .await;
                        });
                        app.toast_error(format!("Couldn't save your setup: {message}"));
                    }
                };
                // Apply it to the session that just chose it, or the prompt
                // would still offer the harness they replaced.
                app.set_harness(Some(&outcome.agent));
                app.set_theme(Some(&outcome.theme));
                // A default project is a target, and the tree now leads with it.
                if let Some(project) = outcome.project {
                    app.default_project = Some(project.project_id.clone());
                    app.target = Some(Target {
                        project_id: project.project_id,
                        project_name: project.project_name,
                        environment_id: project.environment_id,
                        environment_name: project.environment_name,
                    });
                }
            }
            Some(Effect::SaveSettings(outcome)) => {
                apply_settings(app, &outcome);
            }
            Some(Effect::ScanEverywhere) => {
                // A deliberate scan clears a previous rate-limit stop: the user
                // is asking again, and by now the window may have passed.
                stop_fetching.store(false, std::sync::atomic::Ordering::Relaxed);
                let effects = app.scan_environments();
                match effects.len() {
                    // Nothing left to discover — the account-wide query answers
                    // for every environment at once, so this is the normal case
                    // rather than an edge one. "Every project is already loaded"
                    // was a true sentence that did nothing, and it was the reply
                    // anyone reaching for shift+r to see a new agent got.
                    0 => {
                        app.status = "Refreshing…".into();
                        app.refresh_announce = true;
                        start_refresh(app, &tx, &client, &backboard);
                    }
                    n => {
                        app.status =
                            format!("Looking for agents in {n} more environment{}…", plural(n));
                        spawn_sweep(effects, &tx, &client, &backboard, stop_fetching.clone());
                    }
                }
            }
            Some(Effect::RefreshAll) => start_refresh(app, &tx, &client, &backboard),
            Some(Effect::OpenUrl(url)) => {
                // Best-effort: a machine with no browser is a normal way to run
                // this, and the ssh command in the toast is still copyable.
                match ::open::that_detached(&url) {
                    Ok(()) => app.toast(format!("Opened {}", elide(&url, 48))),
                    Err(err) => app.toast_error(format!("Couldn't open it: {err}")),
                }
            }
            Some(Effect::SaveDefaultProject(target)) => {
                app.status = match save_default_project(&target) {
                    Ok(()) => format!("Default project is now {}", target.label()),
                    Err(err) => format!("Couldn't save your default project: {err:#}"),
                };
            }
            Some(Effect::CopySsh {
                agent_id,
                environment_id,
                session_name,
            }) => {
                let command = ssh_command_for(&environment_id, &agent_id, &session_name);
                match crate::util::clipboard::copy(&command) {
                    Ok(()) => app.toast("Copied the ssh command"),
                    Err(err) => app.toast_error(format!("Couldn't copy: {err}")),
                }
            }
            Some(Effect::KillSession {
                agent_id,
                environment_id,
                session_name,
            }) => {
                // Our pane onto it goes first: the process is about to die, and
                // a pane left rendering a dead pty is just a frozen screen.
                if let Some(index) = app
                    .sessions
                    .iter()
                    .position(|s| s.durable_name == session_name)
                {
                    close_session(app, index, &client, &backboard).await;
                }
                let tx = tx.clone();
                tokio::spawn(async move {
                    let error = code::kill_session(&environment_id, &agent_id, &session_name)
                        .await
                        .err()
                        .map(|e| format!("{e:#}"));
                    let _ = tx.send(Message::SessionKilled {
                        agent_id,
                        session_name,
                        error,
                    });
                });
            }
            Some(Effect::CloseSession { index }) => {
                close_session(app, index, &client, &backboard).await
            }
            Some(Effect::Agent {
                op,
                agent_id,
                environment_id,
            }) => {
                // Deleting the agent a session is attached to leaves the pane
                // pointing at something that no longer exists.
                if op == AgentOp::Delete
                    && let Some(index) = app.sessions.iter().position(|s| s.agent_id == agent_id)
                {
                    close_session(app, index, &client, &backboard).await;
                }
                let tx = tx.clone();
                let client = client.clone();
                let backboard = backboard.clone();
                tokio::spawn(async move {
                    let error = run_agent_op(&client, &backboard, op, &agent_id, &environment_id)
                        .await
                        .err()
                        .map(|e| format!("{e:#}"));
                    super::telemetry::track_agent_op(op, error.as_deref()).await;
                    let _ = tx.send(Message::AgentOpDone {
                        agent_id,
                        environment_id,
                        op,
                        error,
                    });
                });
            }
            Some(Effect::Launch(req)) => dispatch_launch(app, req, &tx, &client, &backboard),
            Some(Effect::LoadSessions { agent_id, path }) => {
                spawn_session_fetch(agent_id, path, &tx, &client, &backboard);
            }
            Some(Effect::LoadAgents {
                environment_id,
                path,
            }) => {
                spawn_env_agents_fetch(environment_id, path, &tx, &client, &backboard);
            }
        }
    }
}

/// Fetch one environment's agents in the background, delivering the answer —
/// or its rate-limit classification — as a message.
/// Resolve one background auto-connect's ssh info off the loop. The same
/// shape as a keyed reattach, but its outcome lands as the quiet messages —
/// success must not steal focus and failure must not toast.
fn spawn_auto_connect(connect: app::AutoConnect, tx: &mpsc::UnboundedSender<Message>) {
    let app::AutoConnect {
        agent_id,
        agent_name,
        environment_id,
        session_name,
    } = connect;
    let tx = tx.clone();
    tokio::spawn(async move {
        let message = match code::connect_info(&environment_id, &agent_id).await {
            Ok(info) => Message::AutoReattachReady {
                agent_id,
                agent_name,
                session_name,
                info: Box::new(info),
            },
            Err(err) => {
                let error = format!("{err:#}");
                super::telemetry::track_session_event(
                    "auto_reattach_connect_failed",
                    Some(error.as_str()),
                )
                .await;
                Message::AutoConnectFailed {
                    session_name,
                    error,
                }
            }
        };
        let _ = tx.send(message);
    });
}

fn spawn_env_agents_fetch(
    environment_id: String,
    path: (usize, usize, usize),
    tx: &mpsc::UnboundedSender<Message>,
    client: &reqwest::Client,
    backboard: &str,
) {
    let tx = tx.clone();
    let client = client.clone();
    let backboard = backboard.to_string();
    tokio::spawn(async move {
        // A closed receiver just means the TUI already handed back;
        // the next entry re-requests, so the drop is harmless.
        match fetch_agents(&client, &backboard, &environment_id).await {
            Ok(agents) => {
                let _ = tx.send(Message::AgentsLoaded {
                    path,
                    result: Ok(agents),
                });
            }
            // The same classification the background fetches do:
            // a 429 puts the row back to "not loaded" with the
            // Retry-After toast, instead of pinning "rate limited"
            // to this one environment as if it were its failure.
            Err(err) => match rate_limit_from(&err) {
                Some(retry_after_secs) => {
                    let _ = tx.send(Message::RateLimited { retry_after_secs });
                }
                None => {
                    let _ = tx.send(Message::AgentsLoaded {
                        path,
                        result: Err(err.to_string()),
                    });
                }
            },
        }
    });
}

fn handle_message(
    app: &mut App,
    message: Message,
    tx: &mpsc::UnboundedSender<Message>,
    client: &reqwest::Client,
    backboard: &str,
    stop_fetching: &StopFlag,
) -> Option<Effect> {
    match message {
        Message::RateLimited { retry_after_secs } => {
            app.rate_limited(retry_after_secs);
            None
        }
        Message::AgentsLoaded { path, result } => {
            app.agents_loaded(path, result);
            // Fill in each running agent's session count without waiting for
            // someone to expand it. Bounded: one environment usually holds a
            // handful of agents, but nothing guarantees it.
            let prefetch = app.sessions_to_prefetch();
            if !prefetch.is_empty() {
                spawn_session_prefetch(prefetch, tx, client, backboard, stop_fetching.clone());
            }
            // A launch that just created an agent asked for it to be opened;
            // its row only exists now.
            app.expand_pending()
        }
        Message::MyAgentsLoaded { result, asked_at } => match result {
            Ok(agents) => {
                let count = agents.len();
                app.refresh_finished();
                app.my_agents_loaded(agents, asked_at);
                let prefetch = app.sessions_to_prefetch();
                if !prefetch.is_empty() {
                    spawn_session_prefetch(prefetch, tx, client, backboard, stop_fetching.clone());
                }
                // What someone is looking at, asked about again — the counts and
                // session rows are as able to go stale as the agents are. Narrow
                // by design: see [`App::sessions_to_refresh`].
                let watched = app.sessions_to_refresh();
                if !watched.is_empty() {
                    spawn_session_prefetch(watched, tx, client, backboard, stop_fetching.clone());
                }
                app.refreshed(count);
                // Normally redundant — a launch's own refetch answers this —
                // but it is the safety net when that refetch failed before
                // this settle arrived.
                app.expand_pending()
            }
            // Whether the field doesn't exist yet or the request died, the
            // per-environment path still works, so startup degrades to what
            // it loaded before `myCloudAgents`: the environments a keypress
            // would immediately need.
            Err(err) => {
                app.refresh_finished();
                // The per-environment fallback below is the answer to whatever
                // asked, and the rows are its report; a pending "up to date"
                // line must not be claimed by the next refresh to succeed.
                app.refresh_announce = false;
                // Asking again every tick would fail again every tick: a caller
                // this field refuses — a workspace-scoped token, or a backboard
                // without it — is refused permanently, so refreshes switch to
                // the per-environment path from here.
                let first_failure = !app.account_query_unavailable;
                app.account_query_unavailable = true;
                // These are fresh requests; a stop left over from an earlier
                // 429 would strand them as spinners that never resolve.
                stop_fetching.store(false, std::sync::atomic::Ordering::Relaxed);
                let mut sweep = app.initial_environments();
                if sweep.is_empty() {
                    // Nothing left to load for the first time, so this was a
                    // refresh rather than startup — and it still has to refresh
                    // something. Without this, the ⌥r that discovered the field
                    // was unavailable would change nothing and only the next one
                    // would work.
                    sweep = app.environments_to_refresh();
                }
                if !sweep.is_empty() {
                    spawn_sweep(sweep, tx, client, backboard, stop_fetching.clone());
                } else if first_failure {
                    // No target and no default project means nothing loads
                    // lazily either — without this line an expired login
                    // looks like an account with no agents anywhere. Said once:
                    // a refresh that keeps failing must not toast on a timer.
                    app.toast_error(format!("Couldn't load agents: {err}"));
                }
                None
            }
        },
        Message::SessionsLoaded {
            path,
            agent_id,
            result,
        } => {
            app.sessions_loaded(path, &agent_id, result);
            None
        }
        Message::LaunchStep(text) => {
            app.loading_step(text);
            None
        }
        Message::LaunchFailed(err) => {
            app.launch_failed(err);
            None
        }
        Message::AgentOpDone {
            agent_id,
            environment_id,
            op,
            error,
        } => {
            app.agent_op_finished(&agent_id, &environment_id, op, error);
            // Refetch rather than guess the new state: a wake can land in
            // STARTING, and a delete removes the row entirely. Sleep and wake
            // keep being asked after this one, until they arrive.
            app.reveal_environment(&environment_id)
        }
        Message::LaunchReady(prepared, req) => open_session(app, *prepared, *req, tx),
        Message::ReattachReady {
            agent_id,
            agent_name,
            session_name,
            info,
        } => {
            let notify_tx = tx.clone();
            match session::Session::spawn(
                agent_id.clone(),
                agent_name,
                "session".to_string(),
                &info.ssh_target,
                info.identity.as_deref(),
                &info.relay_opts,
                // Reattaching runs nothing: the relay hands back the screen the
                // session already has.
                "",
                true,
                &session_name,
                24,
                80,
                move || {
                    let _ = notify_tx.send(Message::SessionOutput);
                },
            ) {
                Ok(session) => {
                    app.attach_session(session, agent_id);
                    tokio::spawn(super::telemetry::track_session_event("reattach", None));
                    None
                }
                Err(err) => {
                    app.connecting.remove(&session_name);
                    let message = format!("couldn't reattach: {err}");
                    let telemetry_message = message.clone();
                    tokio::spawn(async move {
                        super::telemetry::track_session_event(
                            "reattach_open_failed",
                            Some(telemetry_message.as_str()),
                        )
                        .await;
                    });
                    app.launch_failed(message);
                    None
                }
            }
        }
        Message::ReattachFailed {
            session_name,
            error,
        } => {
            app.connecting.remove(&session_name);
            app.launch_failed(error);
            None
        }
        Message::AutoReattachReady {
            agent_id,
            agent_name,
            session_name,
            info,
        } => {
            let notify_tx = tx.clone();
            match session::Session::spawn(
                agent_id.clone(),
                agent_name,
                "session".to_string(),
                &info.ssh_target,
                info.identity.as_deref(),
                &info.relay_opts,
                // Reattaching runs nothing: the relay hands back the screen
                // the session already has.
                "",
                true,
                &session_name,
                24,
                80,
                move || {
                    let _ = notify_tx.send(Message::SessionOutput);
                },
            ) {
                Ok(session) => {
                    app.attach_session_background(session, agent_id);
                    tokio::spawn(super::telemetry::track_session_event("auto_reattach", None));
                }
                Err(err) => {
                    let error = format!("{err:#}");
                    let telemetry_error = error.clone();
                    tokio::spawn(async move {
                        super::telemetry::track_session_event(
                            "auto_reattach_open_failed",
                            Some(telemetry_error.as_str()),
                        )
                        .await;
                    });
                    app.auto_connect_failed(&session_name, &error);
                }
            }
            None
        }
        Message::AutoConnectFailed {
            session_name,
            error,
        } => {
            app.auto_connect_failed(&session_name, &error);
            None
        }
        Message::ProjectCreated(result) => {
            if let Some(w) = app.wizard.as_mut() {
                w.project_created(result);
            } else if let Some(outcome) = app
                .settings
                .as_mut()
                .and_then(|s| s.project_created(result))
            {
                // A create that stuck is a change; it saves like any other.
                apply_settings(app, &outcome);
            }
            None
        }
        Message::ClaudeMintDone { ok, req } => {
            if ok {
                // The cache holds the token now; the pipeline reads it there.
                start_launch(app, *req, tx);
            } else {
                // The manual paste needs the real terminal. Stop the loading
                // screen and hand the request back through the step-out; the
                // fallback skips straight to the paste prompt rather than
                // re-running the automation that just lost.
                app.loading.active = false;
                return Some(Effect::StepOutForMint(*req));
            }
            None
        }
        Message::SshKeyRegistered { result, then } => match result {
            Ok(()) => {
                app.ssh_key = SshKeyState::Ready;
                app.toast("SSH key registered");
                // The held connect resumes as the effect it was; it passes the
                // now-Ready gate and takes the normal path from there.
                then.map(HeldConnect::into_effect)
            }
            Err(message) => {
                // Still unregistered: the next connect raises the gate again.
                // The toast gets the first line; register_ssh_key already maps
                // the duplicate-fingerprint rejection to something actionable.
                let first = message.lines().next().unwrap_or("registration failed");
                app.toast_error(format!("Couldn't register the key: {first}"));
                None
            }
        },
        Message::SessionKilled {
            agent_id,
            session_name,
            error,
        } => {
            let telemetry_error = error.clone();
            tokio::spawn(async move {
                super::telemetry::track_session_event("kill_session", telemetry_error.as_deref())
                    .await;
            });
            app.session_killed(&session_name, error);
            // The list is the proof: refetch so the row goes, rather than
            // asserting it did.
            app.refresh_agent_sessions(&agent_id)
        }
        Message::RefreshAgentSessions(agent_id) => app.refresh_agent_sessions(&agent_id),
        // The draw at the top of the loop is the response.
        // Output also carries the end: the reader thread flips `ended` and
        // sends one last wake, which is when a finished pane gets closed.
        Message::SessionOutput => app.reap_ended_sessions(),
    }
}

fn open_session(
    app: &mut App,
    prepared: Prepared,
    req: LaunchRequest,
    tx: &mpsc::UnboundedSender<Message>,
) -> Option<Effect> {
    // A plain connect to an agent that is already open should show that pane
    // rather than run a second ssh into the same box. A prompt should not: it
    // is new work and gets a session of its own.
    if !req.wants_new_session()
        && req.session_name.is_none()
        && app.activate_session(&prepared.agent_id)
    {
        app.screen = app::Screen::Manage;
        app.status = "Switched to the open session".into();
        return app.reveal_environment(&prepared.environment_id);
    }

    // Every session we start is a *named* durable session, so the platform
    // tracks it, `cloudAgentConsoleSessions` lists it, and it survives this ssh
    // dying. Reattaching supplies the name instead of inventing one.
    let durable_session = req
        .session_name
        .clone()
        .unwrap_or_else(|| session::durable_name(prepared.harness));

    let notify_tx = tx.clone();
    // A placeholder size: the next frame measures the real pane and resizes
    // both the pty and the emulator before anything is drawn from it.
    let (rows, cols) = (24u16, 80u16);
    match session::Session::spawn(
        prepared.agent_id.clone(),
        prepared.agent_name.clone(),
        prepared.harness.to_string(),
        &prepared.ssh_target,
        prepared.identity.as_deref(),
        &prepared.relay_opts,
        &prepared.remote_cmd,
        req.session_name.is_some(),
        &durable_session,
        rows,
        cols,
        move || {
            let _ = notify_tx.send(Message::SessionOutput);
        },
    ) {
        Ok(session) => {
            app.attach_session(session, prepared.agent_id.clone());
            schedule_session_refresh(prepared.agent_id.clone(), tx);
            // Refetch the environment so a newly created agent appears, and
            // remember to open it: the session we just started is one of its
            // children now, and that is where it should be visible.
            app.expand_agent_after_load(prepared.agent_id.clone());
            app.reveal_environment(&prepared.environment_id)
        }
        Err(err) => {
            let message = format!("couldn't open the session: {err}");
            let telemetry_message = message.clone();
            tokio::spawn(async move {
                super::telemetry::track_session_event(
                    "launch_open_failed",
                    Some(telemetry_message.as_str()),
                )
                .await;
            });
            app.launch_failed(message);
            None
        }
    }
}

/// How long after opening a session to re-ask the platform for the agent's
/// session list, and again after that.
///
/// The relay registers a session a moment after ssh connects, so the refresh
/// that fires with the launch usually misses it — which is why a newly started
/// session only appeared after closing and reopening the agent. Two cheap
/// retries cover the gap without polling forever.
const SESSION_SETTLE: [u64; 2] = [900, 2600];

/// How many count queries are allowed in flight at once.
const SWEEP_CONCURRENCY: usize = 5;

/// `s` when there is more than one of something.
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A 429 seen by any background fetch. Shared so the rest of a batch stops
/// rather than spending the caller's remaining budget on requests that will be
/// refused too.
type StopFlag = std::sync::Arc<std::sync::atomic::AtomicBool>;

/// Was this a rate limit, and for how long?
///
/// `anyhow` erases the type on the way through `fetch_agents`, so the concrete
/// error is recovered here. Matching on the message would work until someone
/// reworded it.
fn rate_limit_from(err: &anyhow::Error) -> Option<Option<u64>> {
    match err.downcast_ref::<crate::errors::RailwayError>() {
        Some(crate::errors::RailwayError::Ratelimited { retry_after_secs }) => {
            Some(*retry_after_secs)
        }
        _ => None,
    }
}

/// Fetch a batch of environments' agents, a few at a time.
fn spawn_sweep(
    effects: Vec<Effect>,
    tx: &mpsc::UnboundedSender<Message>,
    client: &reqwest::Client,
    backboard: &str,
    stop: StopFlag,
) {
    use std::sync::atomic::Ordering;

    let tx = tx.clone();
    let client = client.clone();
    let backboard = backboard.to_string();
    tokio::spawn(async move {
        let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(SWEEP_CONCURRENCY));
        for effect in effects {
            let Effect::LoadAgents {
                environment_id,
                path,
            } = effect
            else {
                continue;
            };
            // Checked before each request rather than only at the top: the answer
            // arrives partway through a batch.
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(permit) = permits.clone().acquire_owned().await else {
                return;
            };
            let tx = tx.clone();
            let client = client.clone();
            let backboard = backboard.clone();
            let stop = stop.clone();
            tokio::spawn(async move {
                match fetch_agents(&client, &backboard, &environment_id).await {
                    Ok(agents) => {
                        let _ = tx.send(Message::AgentsLoaded {
                            path,
                            result: Ok(agents),
                        });
                    }
                    Err(err) => match rate_limit_from(&err) {
                        Some(retry_after_secs) => {
                            stop.store(true, Ordering::Relaxed);
                            let _ = tx.send(Message::RateLimited { retry_after_secs });
                        }
                        None => {
                            let _ = tx.send(Message::AgentsLoaded {
                                path,
                                result: Err(err.to_string()),
                            });
                        }
                    },
                }
                drop(permit);
            });
        }
    });
}

/// Fetch one agent's sessions in the background.
fn spawn_session_fetch(
    agent_id: String,
    path: (usize, usize, usize, usize),
    tx: &mpsc::UnboundedSender<Message>,
    client: &reqwest::Client,
    backboard: &str,
) {
    let tx = tx.clone();
    let client = client.clone();
    let backboard = backboard.to_string();
    tokio::spawn(async move {
        let result = match fetch_sessions(&client, &backboard, &agent_id).await {
            Ok(sessions) => Ok(sessions),
            Err(err) => {
                // Reported as a rate limit rather than as this agent's failure,
                // so the pane says what is actually wrong.
                if let Some(retry_after_secs) = rate_limit_from(&err) {
                    let _ = tx.send(Message::RateLimited { retry_after_secs });
                    return;
                }
                Err(err.to_string())
            }
        };
        let _ = tx.send(Message::SessionsLoaded {
            path,
            agent_id,
            result,
        });
    });
}

/// Fetch a batch of agents' sessions, a few at a time.
///
/// Same discipline as [`spawn_sweep`]: the account-wide settle can name every
/// running agent at once, and firing one request per agent simultaneously is
/// the burst this TUI exists to avoid. The first 429 abandons the rest.
fn spawn_session_prefetch(
    effects: Vec<Effect>,
    tx: &mpsc::UnboundedSender<Message>,
    client: &reqwest::Client,
    backboard: &str,
    stop: StopFlag,
) {
    use std::sync::atomic::Ordering;

    let tx = tx.clone();
    let client = client.clone();
    let backboard = backboard.to_string();
    tokio::spawn(async move {
        let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(SWEEP_CONCURRENCY));
        for effect in effects {
            let Effect::LoadSessions { agent_id, path } = effect else {
                continue;
            };
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(permit) = permits.clone().acquire_owned().await else {
                return;
            };
            let tx = tx.clone();
            let client = client.clone();
            let backboard = backboard.clone();
            let stop = stop.clone();
            tokio::spawn(async move {
                match fetch_sessions(&client, &backboard, &agent_id).await {
                    Ok(sessions) => {
                        let _ = tx.send(Message::SessionsLoaded {
                            path,
                            agent_id,
                            result: Ok(sessions),
                        });
                    }
                    Err(err) => match rate_limit_from(&err) {
                        Some(retry_after_secs) => {
                            stop.store(true, Ordering::Relaxed);
                            let _ = tx.send(Message::RateLimited { retry_after_secs });
                        }
                        None => {
                            let _ = tx.send(Message::SessionsLoaded {
                                path,
                                agent_id,
                                result: Err(err.to_string()),
                            });
                        }
                    },
                }
                drop(permit);
            });
        }
    });
}

/// Register the gate's key with Railway. String errors because the result
/// crosses the message channel; `register_ssh_key` has already mapped the
/// duplicate-fingerprint rejection to a user-facing message.
async fn register_gate_key(client: &reqwest::Client, offer: &SshKeyOffer) -> Result<(), String> {
    let configs = Configs::new().map_err(|e| format!("{e:#}"))?;
    crate::controllers::ssh::keys::register_ssh_key(
        client,
        &configs,
        &offer.name,
        &offer.public_key,
        None,
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("{e:#}"))
}

/// Re-ask for an agent's sessions shortly after one is opened.
fn schedule_session_refresh(agent_id: String, tx: &mpsc::UnboundedSender<Message>) {
    let tx = tx.clone();
    tokio::spawn(async move {
        for delay in SESSION_SETTLE {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            if tx
                .send(Message::RefreshAgentSessions(agent_id.clone()))
                .is_err()
            {
                return;
            }
        }
    });
}

/// Translate a request from the TUI into the launcher's arguments.
///
/// Its own function because this mapping is where "new session on this agent"
/// quietly became "new agent": the request named an agent and the arguments
/// dropped it, leaving the pipeline to infer one and create a VM when it could
/// not. Tested directly, so a dropped field fails here rather than on a bill.
fn launch_args_for(req: &LaunchRequest) -> LaunchArgs {
    (*req.base).clone().retargeted(
        req.project_id.clone(),
        req.environment_id.clone(),
        &req.harness,
        req.force_new,
        req.prompt.clone(),
        req.agent_id.clone(),
    )
}

/// Everything a launch has to clear before the pipeline sees it: the screen it
/// belongs on, the ssh key it will connect with, and the Claude credential it
/// may need minting.
///
/// A function rather than the body of one match arm because two things start
/// launches — an [`Effect::Launch`] someone pressed a key for, and the
/// `autostart` a `railway code` invocation arrived with — and the second must
/// clear exactly the same gates as the first. Each gate that holds the launch
/// stores it and returns; the loop redraws with whatever question it raised.
fn dispatch_launch(
    app: &mut App,
    req: LaunchRequest,
    tx: &mpsc::UnboundedSender<Message>,
    client: &reqwest::Client,
    backboard: &str,
) {
    // The launch lives on the manage screen — the tree the agent will land in —
    // so go there first and reveal its environment. The ssh gate's question
    // then hangs over the place the answer matters, not over wherever it
    // happened to be asked from.
    app.screen = Screen::Manage;
    if let Some(Effect::LoadAgents {
        environment_id,
        path,
    }) = app.reveal_environment(&req.environment_id)
    {
        spawn_env_agents_fetch(environment_id, path, tx, client, backboard);
    }
    // Connecting rides SSH, so an unregistered key is settled with an in-frame
    // question before anything is spent on the launch.
    if app.hold_for_ssh_key(HeldConnect::Launch(req.clone())) {
        return;
    }
    // The mint's browser round-trip needs no terminal — the browser does the
    // interacting and the flow runs hidden — so it runs under the frame with
    // the loading screen narrating. Only its manual-paste fallback needs the
    // real terminal, and only that failure steps out (see ClaudeMintDone).
    if req.harness == "claude" && code::claude_needs_local_mint() {
        app.start_loading(&req);
        let _ = tx.send(Message::LaunchStep(
            "Minting a Claude token — approve the browser prompt if one appears".to_string(),
        ));
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let ok = code::mint_claude_credential_headless().is_ok();
            let _ = tx.send(Message::ClaudeMintDone {
                ok,
                req: Box::new(req),
            });
        });
        return;
    }
    start_launch(app, req, tx);
}

/// Kick off a launch in the background and show the loading screen.
fn start_launch(app: &mut App, req: LaunchRequest, tx: &mpsc::UnboundedSender<Message>) {
    app.start_loading(&req);
    spawn_prepare(req, tx.clone());
}

/// One lifecycle mutation. Kept next to the loop rather than in the app so the
/// state machine stays free of the network.
async fn run_agent_op(
    client: &reqwest::Client,
    backboard: &str,
    op: AgentOp,
    agent_id: &str,
    environment_id: &str,
) -> Result<()> {
    let backboard = backboard.to_string();
    match op {
        AgentOp::Sleep => {
            crate::controllers::cloud_agent::sleep(client, &backboard, environment_id, agent_id)
                .await?;
        }
        AgentOp::Wake => {
            post_graphql::<mutations::CloudAgentWake, _>(
                client,
                backboard,
                mutations::cloud_agent_wake::Variables {
                    id: agent_id.to_string(),
                },
            )
            .await?;
        }
        AgentOp::Delete => {
            post_graphql::<mutations::CloudAgentDelete, _>(
                client,
                backboard,
                mutations::cloud_agent_delete::Variables {
                    id: agent_id.to_string(),
                },
            )
            .await?;
        }
    }
    Ok(())
}

/// Close our end of a session.
///
/// Detach only. The agent keeps running, because it may well have other
/// sessions on it and because sleeping is `s` — a deliberate act on the agent,
/// not a side effect of closing one window onto it. Quitting detaches the
/// same way; `railway ca sleep` (or `s` on the tree) is what stops the bill.
async fn close_session(app: &mut App, index: usize, _client: &reqwest::Client, _backboard: &str) {
    if let Some(mut session) = app.take_session(index) {
        session.detach();
    }
}

/// Keep the emulator the same shape as the pane it is drawn into — done after a
/// draw, when the layout that produced the pane is known. A mismatch here is
/// what makes a remote TUI wrap in the wrong place.
fn sync_session_size(app: &mut App, terminal: &Terminal<CrosstermBackend<std::io::Stdout>>) {
    let Some((rows, cols)) = ui::session_pane_size(terminal.size().ok(), app.pane_is_full()) else {
        return;
    };
    // Every session gets the pane's shape, not just the visible one: a
    // background agent that redraws while hidden should already be the right
    // size when you switch to it.
    for session in app.sessions.iter_mut() {
        session.resize(rows, cols);
    }
}

/// Put the lifted text on the clipboard and clear the highlight.
fn finish_copy(app: &mut App, text: Option<String>) {
    app.selection = None;
    let Some(text) = text else {
        return;
    };
    let lines = text.lines().count();
    // In the corner rather than the header: a drag ends wherever the pointer
    // is, and the only other evidence that it worked is the clipboard, which
    // is not on the screen.
    match crate::util::clipboard::copy(&text) {
        Ok(()) => app.toast(format!(
            "Copied {lines} line{}",
            if lines == 1 { "" } else { "s" }
        )),
        Err(err) => app.toast_error(format!("Couldn't copy: {err}")),
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    // While the TUI holds the terminal, no inquire prompt can work — the event
    // loop would eat its keystrokes and the next frame would paint over it.
    // This flag makes every prompt helper (and ensure_ssh_key's registration
    // fallback) fail fast instead of deadlocking the caller.
    crate::util::prompt::set_terminal_owned(true);
    // Mouse capture takes the terminal's own selection away, which is why the
    // TUI implements drag-to-copy itself.
    //
    // Bracketed paste is what keeps a paste from arriving as typed keys: text
    // inserted by the terminal — ⌘v, and dictation tools like Wispr Flow,
    // which insert into a terminal the same way — comes wrapped in markers
    // and reaches the loop as one `Event::Paste`, instead of a stream of
    // keystrokes whose every newline hits Enter in the pane.
    execute!(
        stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        Hide
    )?;
    // Ask for the enhanced keyboard protocol, which is what makes a modifier on
    // Escape reportable at all: a plain terminal sends ⇧esc as a bare Escape,
    // indistinguishable from the one meant for the agent. Terminals that do not
    // support it ignore the request, which is why `^]` and `^o` also release.
    //
    // Disambiguation is deliberately the only flag, and it is worth knowing what
    // it does not buy. The kitty protocol exempts Enter, Tab and Backspace from
    // this mode by design — "they still generate the same bytes as in legacy
    // mode", so a shell stays usable if a crashed program leaves the mode set —
    // which means shift+enter reaches us as a bare `\r` with no modifier on it,
    // and no amount of work in `encode_key_for` can recover what the terminal
    // never said. REPORT_ALL_KEYS_AS_ESCAPE_CODES would lift the exemption, but
    // crossterm cannot yet read the associated text those events carry, so
    // composed input would break: the ⌥-composed characters `alt_chord` leans
    // on, and every dead key and IME besides. Reporting shift+enter is the
    // terminal's job to opt into (Claude Code's own `/terminal-setup` binds it
    // to `ESC CR` for exactly this reason); when it does, the pane forwards it.
    if matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    ) {
        let _ = execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal() {
    // Popping a protocol that was never pushed is harmless; leaving one pushed
    // would follow the user out of the TUI and into their shell.
    let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    let _ = execute!(
        stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show
    );
    // Again, on the main screen. Mouse tracking left on is the one piece of
    // state the user cannot see and cannot clear: every pointer movement
    // becomes `35;21;32M` at their shell prompt. Terminals disagree about
    // whether the modes belong to the screen buffer that set them, so turn
    // them off on both.
    let _ = execute!(stdout(), DisableBracketedPaste, DisableMouseCapture);
    let _ = disable_raw_mode();
    crate::util::prompt::set_terminal_owned(false);
    let _ = stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> LaunchRequest {
        LaunchRequest {
            project_id: "proj_1".into(),
            environment_id: "env_prod".into(),
            agent_id: None,
            session_name: None,
            force_new: false,
            new_session: false,
            harness: "claude".into(),
            prompt: None,
            label: "devtools/production".into(),
            base: Default::default(),
        }
    }

    /// `n` on an agent asks for another session on *that* agent. The arguments
    /// must say so: an agent id that goes missing here is a new VM.
    #[test]
    fn a_new_session_request_pins_its_agent_and_creates_nothing() {
        let args = launch_args_for(&LaunchRequest {
            agent_id: Some("ca_1".into()),
            new_session: true,
            ..request()
        });
        assert_eq!(args.agent_id.as_deref(), Some("ca_1"));
        assert!(!args.new, "must not ask the pipeline to create an agent");
        assert_eq!(args.environment.as_deref(), Some("env_prod"));
        assert_eq!(args.project.as_deref(), Some("proj_1"));
    }

    /// `n` on a project or environment is the opposite: create one.
    #[test]
    fn a_new_agent_request_creates_and_pins_nothing() {
        let args = launch_args_for(&LaunchRequest {
            force_new: true,
            ..request()
        });
        assert!(args.new);
        assert_eq!(args.agent_id, None);
    }

    /// The copied command has to be the one that reaches *this* session: the
    /// relay target is a username, and the session is named through SetEnv.
    #[test]
    fn the_copied_ssh_command_names_the_session_and_the_relay() {
        let command = ssh_command_for("env_1", "ca_1", "claude-3s9r89");
        assert!(command.starts_with("ssh "), "{command}");
        assert!(
            command.contains("-o SetEnv=RAILWAY_DURABLE_SESSION_NAME=claude-3s9r89"),
            "{command}"
        );
        assert!(command.contains("agent:env_1:ca_1@"), "{command}");
        // The target is a username on the relay, not a host of its own.
        assert!(!command.contains(" agent:env_1:ca_1 "), "{command}");
    }

    /// A `railway code` launch opens in the pane, so its flags reach the
    /// pipeline through the request rather than straight off the command line.
    /// The ones no card asks for have nowhere else to travel.
    /// Compared whole rather than field by field: `--name` and `--variable`
    /// are private to `code`, and comparing against the retargeted base is the
    /// stronger claim anyway — nothing was dropped, not just the two we
    /// thought to name.
    #[test]
    fn a_command_line_launch_keeps_the_flags_it_arrived_with() {
        use clap::Parser;

        let base = LaunchArgs::parse_from(["code", "--new", "--name", "api", "--variable", "K=V"]);
        let args = launch_args_for(&LaunchRequest {
            base: Box::new(base.clone()),
            force_new: true,
            ..request()
        });
        assert_eq!(
            args,
            base.retargeted(
                "proj_1".into(),
                "env_prod".into(),
                "claude",
                true,
                None,
                None
            )
        );
        // And the request is what aimed it: the base named no target.
        assert_eq!(args.environment.as_deref(), Some("env_prod"));
    }

    /// A prompt rides through to the session it seeds.
    #[test]
    fn a_prompt_request_carries_its_task_and_agent() {
        let args = launch_args_for(&LaunchRequest {
            agent_id: Some("ca_9".into()),
            prompt: Some("fix the tests".into()),
            ..request()
        });
        assert_eq!(args.initial_prompt.as_deref(), Some("fix the tests"));
        assert_eq!(args.agent_id.as_deref(), Some("ca_9"));
        assert!(!args.new);
    }
}
