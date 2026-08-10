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
pub mod theme;
mod ui;
pub mod wizard;

use std::io::{Write, stdout};
use std::panic;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
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
    Agent, AgentOp, ConsoleSession, EnvNode, Load, LoadSessions, ProjectNode, WorkspaceNode,
};
pub use app::{App, Effect, LaunchRequest, Target};

use crate::client::post_graphql;
use crate::commands::code::{self, LaunchArgs, Prepared, Progress};
use crate::config::Configs;
use crate::gql::{mutations, queries};

/// Create the project first-run setup offers to make, in the workspace the
/// wizard asked for.
///
/// `workspace_id` is `None` only when there was one workspace to choose from,
/// so falling back to the first is the same answer the user would have given.
/// An id that no longer resolves is an error rather than a silent fallback:
/// creating the project somewhere other than where they pointed is how one ends
/// up lost.
async fn create_default_project(
    client: &reqwest::Client,
    backboard: &str,
    workspace_id: Option<String>,
) -> Result<wizard::ProjectOption> {
    use crate::gql::mutations;

    let configs = Configs::new()?;
    let workspaces = crate::workspace::workspaces_with_client(client, &configs).await?;
    let workspace = match workspace_id {
        Some(id) => workspaces
            .iter()
            .find(|ws| ws.id() == id)
            .map(|ws| ws.id().to_string())
            .ok_or_else(|| anyhow::anyhow!("that workspace is no longer available"))?,
        None => workspaces
            .first()
            .map(|ws| ws.id().to_string())
            .ok_or_else(|| anyhow::anyhow!("no workspace to create a project in"))?,
    };

    let created = post_graphql::<mutations::ProjectCreate, _>(
        client,
        backboard.to_string(),
        mutations::project_create::Variables {
            name: Some("Cloud Agents".to_string()),
            description: Some("Home for Railway cloud agents".to_string()),
            workspace_id: Some(workspace),
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
    SessionKilled {
        agent_id: String,
        session_name: String,
        error: Option<String>,
    },
    ProjectCreated(Result<wizard::ProjectOption, String>),
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

/// Build the tree from the workspace listing. Deleted projects are dropped, and
/// so are environments the caller cannot access — an agent can't be listed or
/// created in either, so showing them would only offer dead ends.
///
/// Workspaces with nothing left in them stay: they are dropped from the browse
/// tree by [`App::new`], but setup still offers them as somewhere to create the
/// default project, which is the whole point of an empty workspace.
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

/// Ask for the whole account's agents in the background, once, at startup.
fn spawn_my_agents_fetch(
    tx: &mpsc::UnboundedSender<Message>,
    client: &reqwest::Client,
    backboard: &str,
) {
    let tx = tx.clone();
    let client = client.clone();
    let backboard = backboard.to_string();
    tokio::spawn(async move {
        match fetch_my_agents(&client, &backboard).await {
            Ok(agents) => {
                let _ = tx.send(Message::MyAgentsLoaded { result: Ok(agents) });
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
                    });
                }
            },
        }
    });
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
    // loading just the environments a keypress would immediately need. On
    // re-entry after a session the tree has already settled, and the settle
    // would discard a fresh answer — so don't spend the request.
    let stop_fetching: StopFlag = Default::default();
    if app.has_unloaded_environments() {
        spawn_my_agents_fetch(&tx, &client, &backboard);
    }

    loop {
        let mut rects = app.panes;
        let mut copied: Option<String> = None;
        terminal.draw(|f| {
            let (r, text) = ui::render_with_layout(app, f);
            rects = r;
            copied = text;
        })?;
        app.panes = rects;
        if app.pending_copy.take().is_some() {
            finish_copy(app, copied);
        }
        sync_session_size(app, &terminal);

        let effect = tokio::select! {
            // Background work first: draining it keeps the tree and the session
            // honest even while keys arrive faster than frames.
            Some(message) = rx.recv() => handle_message(app, message, &tx, &client, &backboard, &stop_fetching),
            // Animate the loading screen. Only armed while it is showing, so an
            // idle TUI still blocks rather than spinning on a timer.
            // The wizard borrows the same tick for its "creating…" spinner.
            _ = tokio::time::sleep(SPINNER_TICK), if app.loading.active
                || app.wizard.as_ref().is_some_and(|w| w.busy.is_some()) => {
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
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => app.on_key(key),
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

        match effect {
            None => {}
            Some(Effect::Quit) => {
                // Every open session sleeps on the way out; leaving one awake
                // bills compute with nothing attached to it.
                while !app.sessions.is_empty() {
                    close_and_sleep(app, 0, &client, &backboard).await;
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
                            let message = format!("{err:#}");
                            super::telemetry::track_session_event(
                                "reattach_connect_failed",
                                Some(message.as_str()),
                            )
                            .await;
                            Message::LaunchFailed(message)
                        }
                    };
                    let _ = tx.send(message);
                });
            }
            Some(Effect::CreateDefaultProject { workspace_id }) => {
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
                app.status = match save_setup(app, &outcome) {
                    Ok(prefs) => {
                        tokio::spawn(async move {
                            super::telemetry::track_setup_saved("wizard", &prefs).await;
                        });
                        "Saved — Setup again to change it".into()
                    }
                    Err(err) => {
                        let message = format!("{err:#}");
                        let telemetry_message = message.clone();
                        tokio::spawn(async move {
                            super::telemetry::track_setup_failed("wizard", &telemetry_message)
                                .await;
                        });
                        format!("Couldn't save your setup: {message}")
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
            Some(Effect::ScanEverywhere) => {
                // A deliberate scan clears a previous rate-limit stop: the user
                // is asking again, and by now the window may have passed.
                stop_fetching.store(false, std::sync::atomic::Ordering::Relaxed);
                let effects = app.scan_environments();
                match effects.len() {
                    0 => app.status = "Every project is already loaded".into(),
                    n => {
                        app.status =
                            format!("Looking for agents in {n} more environment{}…", plural(n));
                        spawn_sweep(effects, &tx, &client, &backboard, stop_fetching.clone());
                    }
                }
            }
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
                match arboard::Clipboard::new().and_then(|mut c| c.set_text(command)) {
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
            Some(Effect::Launch(req)) => {
                // Minting needs a browser and a paste, neither of which works
                // underneath a frame.
                if req.harness == "claude" && !code::claude_credential_cached() {
                    return Ok(Outcome::NeedsCredential(req));
                }
                start_launch(app, req, &tx);
            }
            Some(Effect::LoadSessions { agent_id, path }) => {
                spawn_session_fetch(agent_id, path, &tx, &client, &backboard);
            }
            Some(Effect::LoadAgents {
                environment_id,
                path,
            }) => {
                let tx = tx.clone();
                let client = client.clone();
                let backboard = backboard.clone();
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
        }
    }
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
        Message::MyAgentsLoaded { result } => match result {
            Ok(agents) => {
                app.my_agents_loaded(agents);
                let prefetch = app.sessions_to_prefetch();
                if !prefetch.is_empty() {
                    spawn_session_prefetch(prefetch, tx, client, backboard, stop_fetching.clone());
                }
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
                // These are fresh requests; a stop left over from an earlier
                // 429 would strand them as spinners that never resolve.
                stop_fetching.store(false, std::sync::atomic::Ordering::Relaxed);
                let sweep = app.initial_environments();
                if sweep.is_empty() {
                    // No target and no default project means nothing loads
                    // lazily either — without this line an expired login
                    // looks like an account with no agents anywhere.
                    app.status = format!("Couldn't load agents: {err}");
                } else {
                    spawn_sweep(sweep, tx, client, backboard, stop_fetching.clone());
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
        Message::ProjectCreated(result) => {
            if let Some(w) = app.wizard.as_mut() {
                // Say where it went. The wizard moves straight on to the next
                // question, so without this the only record of which workspace
                // now holds a "Cloud Agents" project is the dashboard.
                if let Ok(project) = &result {
                    app.status = match &w.created_in {
                        Some(workspace) => {
                            format!("Created {} in {workspace}", project.project_name)
                        }
                        None => format!("Created {}", project.project_name),
                    };
                }
                w.project_created(result);
            }
            None
        }
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
        Message::SessionOutput => None,
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
    LaunchArgs::for_target(
        req.project_id.clone(),
        req.environment_id.clone(),
        &req.harness,
        req.force_new,
        req.prompt.clone(),
        req.agent_id.clone(),
    )
}

/// Kick off a launch in the background and show the loading screen.
fn start_launch(app: &mut App, req: LaunchRequest, tx: &mpsc::UnboundedSender<Message>) {
    app.start_loading(&req);
    let tx = tx.clone();
    tokio::spawn(async move {
        let req = req;
        let args = launch_args_for(&req);
        let progress = ChannelProgress(tx.clone());
        let message = match code::prepare(&args, &progress).await {
            Ok(prepared) => Message::LaunchReady(Box::new(prepared), Box::new(req)),
            Err(err) => Message::LaunchFailed(format!("{err:#}")),
        };
        let _ = tx.send(message);
    });
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
            post_graphql::<mutations::CloudAgentSleep, _>(
                client,
                backboard,
                mutations::cloud_agent_sleep::Variables {
                    id: agent_id.to_string(),
                },
            )
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
    let _ = environment_id;
    Ok(())
}

/// Close our end of a session.
///
/// Detach only. The agent keeps running, because it may well have other
/// sessions on it and because sleeping is `s` — a deliberate act on the agent,
/// not a side effect of closing one window onto it. Quitting still sleeps
/// everything, which is what stops an agent billing forever.
async fn close_session(app: &mut App, index: usize, _client: &reqwest::Client, _backboard: &str) {
    if let Some(mut session) = app.take_session(index) {
        session.detach();
    }
}

/// Detach a session and sleep its agent — the way out, where nothing is left
/// watching and an awake agent would bill unattended.
async fn close_and_sleep(app: &mut App, index: usize, client: &reqwest::Client, backboard: &str) {
    let Some(mut session) = app.take_session(index) else {
        return;
    };
    let agent_id = session.agent_id.clone();
    session.detach();
    drop(session);

    let result = post_graphql::<mutations::CloudAgentSleep, _>(
        client,
        backboard.to_string(),
        mutations::cloud_agent_sleep::Variables { id: agent_id },
    )
    .await;
    // Worth its own event, not just a silent `let _ =`: a failure here is an
    // agent left running and billing compute with nothing attached to it —
    // exactly the case sleep-on-quit exists to prevent.
    if let Err(err) = result {
        let message = format!("{err:#}");
        super::telemetry::track_session_event("quit_sleep_failed", Some(message.as_str())).await;
    }
}

/// Keep the emulator the same shape as the pane it is drawn into — done after a
/// draw, when the layout that produced the pane is known. A mismatch here is
/// what makes a remote TUI wrap in the wrong place.
fn sync_session_size(app: &mut App, terminal: &Terminal<CrosstermBackend<std::io::Stdout>>) {
    let Some((rows, cols)) = ui::session_pane_size(terminal.size().ok(), app.maximized) else {
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
    match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
        Ok(()) => app.toast(format!(
            "Copied {lines} line{}",
            if lines == 1 { "" } else { "s" }
        )),
        Err(err) => app.toast_error(format!("Couldn't copy: {err}")),
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    // Mouse capture takes the terminal's own selection away, which is why the
    // TUI implements drag-to-copy itself.
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture, Hide)?;
    // Ask for the enhanced keyboard protocol, which is what makes a modifier on
    // Escape reportable at all: a plain terminal sends shift+esc as a bare Escape,
    // indistinguishable from the one meant for the agent. Terminals that do not
    // support it ignore the request, which is why `^]` and `^o` also release.
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
    let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen, Show);
    // Again, on the main screen. Mouse tracking left on is the one piece of
    // state the user cannot see and cannot clear: every pointer movement
    // becomes `35;21;32M` at their shell prompt. Terminals disagree about
    // whether the modes belong to the screen buffer that set them, so turn
    // them off on both.
    let _ = execute!(stdout(), DisableMouseCapture);
    let _ = disable_raw_mode();
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
