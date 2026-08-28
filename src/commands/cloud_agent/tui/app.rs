//! State and key handling for the `railway ca` TUI.
//!
//! Deliberately free of rendering and of I/O: every key produces state changes
//! plus at most one [`Effect`] for the event loop to carry out. That keeps the
//! navigation model — which is most of the behaviour worth getting right —
//! testable without a terminal or a network.
//!
//! The tree is workspace → project → environment → agent. Structure comes from
//! one `UserProjects` call; agents arrive from one `myCloudAgents` call, which
//! answers for the whole account at once. Against a backboard that predates
//! that field, agents are per environment and load only when an environment is
//! expanded, because `cloudAgents` takes one environment at a time and a
//! workspace can hold dozens.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::session;
use super::theme::Theme;

/// Harnesses the launcher can put a session on, in cycle order. `railway`
/// leads the list — and so is the default pick with nothing configured —
/// since it is the one exception needing no credential of its own: no
/// carry-a-local-sign-in step, just the VM's own integrated Railway
/// credentials. `shell` closes it: not a harness at all, just the VM's login
/// shell, so it takes no prompt and sits after every real agent.
pub const HARNESSES: &[&str] = &["railway", "claude", "codex", "grok", "shell"];

/// The slice of [`HARNESSES`] that can be saved as the default agent —
/// everything but `shell`, which starts no harness and so makes no sense as
/// a standing default. What the setup wizard and settings offer.
pub fn default_harnesses() -> &'static [&'static str] {
    &HARNESSES[..HARNESSES.len() - 1]
}

/// Everything the Manage screen responds to, grouped for the `?` overlay. The
/// footer shows two or three of these; this is where the rest lives, so the
/// screen is not a permanent wall of key names.
pub const KEY_HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "move",
        &[
            ("↑ ↓", "up and down"),
            ("→ ←", "open and close"),
            ("enter", "open · connect to a session"),
            ("click", "select · double-click connects"),
        ],
    ),
    (
        "sessions",
        &[
            ("enter", "connect and type in it"),
            ("⌥f", "give it the whole screen · again to restore"),
            ("⌥enter / f", "leave the TUI and connect full screen"),
            ("⌥⇧[ ⌥⇧]", "previous / next session"),
            ("c", "copy an ssh command for it"),
            ("⌥⇧esc / ^]", "stop typing in it"),
            ("wheel", "scroll its output"),
            ("click a link", "open it in your browser"),
            ("shift+pgup/pgdn", "scroll without the mouse"),
            ("x", "end the session"),
            ("r", "reconnect a pane whose connection dropped"),
        ],
    ),
    (
        "agents",
        &[
            ("n", "new agent — pick its harness first"),
            ("⌥n", "new agent now, on the selected harness"),
            (
                "⌥p",
                "new session from a prompt, on the selected row's agent",
            ),
            ("s", "sleep"),
            ("w", "wake"),
            ("d", "delete, with a confirmation"),
            ("⌥r", "refresh everything, from anywhere"),
            ("r", "refresh this environment"),
            ("shift+r", "look for agents in every project"),
        ],
    ),
    (
        "elsewhere",
        &[
            ("t", "set the prompt's target"),
            ("esc", "step back — clear the draft, then home"),
            ("q", "quit, from home"),
            ("^c", "quit"),
        ],
    ),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    pub name: String,
    /// Already lowercased for display: `running`, `sleeping`, `starting`…
    pub status: String,
    /// Shell and exec sessions running on the agent's VM, which the platform
    /// tracks and which can be reattached to by name. Loaded when the agent row
    /// is expanded.
    pub sessions: LoadSessions,
    pub expanded: bool,
}

/// One reattachable session on an agent's VM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleSession {
    /// The durable name the relay reattaches by.
    pub name: String,
    /// `SHELL` for an interactive session, `EXEC` for a one-shot command.
    pub kind: String,
    pub command: Option<String>,
    pub running: bool,
    pub attached: bool,
    /// When the platform says it began — what breaks ties when a pane has to
    /// be matched to a listed session (see [`App::adopt_pane_sessions`]).
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    /// What the harness inside this session last reported it was doing —
    /// joined to the session by its durable name at fetch time. `None` when
    /// the harness never reported (a plain shell, or a report that predates
    /// name attribution).
    pub snapshot: Option<ThreadSnapshot>,
}

/// The reported state of one coding-agent run, from `CloudAgent.sessions` —
/// the same snapshot the dashboard's session cards render. Display-only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadSnapshot {
    /// Which harness reported: claude, codex, grok, railway-agent…
    pub harness: String,
    /// The harness's own session id — for `railway-agent`, the conversation
    /// the in-VM daemon addresses over the gate WebSocket.
    pub session_id: String,
    /// One of: unknown, working, waiting, idle, failed, done.
    pub state: String,
    /// Truncated first prompt of the run.
    pub prompt: Option<String>,
    /// Truncated most recent prompt of the run.
    pub latest_prompt: Option<String>,
    /// What the agent last said: the newest assistant text of the
    /// conversation, read from the daemon's catchup transcript over the
    /// harness gate. Only railway-agent threads have a daemon to ask, so this
    /// is None for every other harness — their responses exist only inside
    /// the VM (the hook pipeline deliberately never carries assistant text).
    pub last_reply: Option<String>,
    /// The VM's clock at the last report — orders snapshots per session.
    pub updated_at: String,
}

/// The env prologue every launch prepends. Everything before it is plumbing —
/// PATH, the GitHub token, the Claude env file — and none of it says what the
/// session is doing.
const LAUNCH_PROLOGUE: &str = "export RAILWAY_CODE_AUTOSTARTED=1; ";

impl ConsoleSession {
    /// Is this worth showing?
    ///
    /// Only what is still running. Finished sessions are our own provisioning
    /// execs and shells that have already ended — including one just killed,
    /// which should leave the list rather than linger as "exited" and look like
    /// the kill did not take.
    pub fn is_interesting(&self) -> bool {
        self.running
    }

    /// The session's name, folded short and led by its harness:
    /// `merry-daisy-ld9` running railway becomes `railway-ld9` — a petname
    /// says nothing its random suffix doesn't, and a minted `railway-x2k9qm`
    /// already leads with its harness. The plain name when the harness is
    /// unknowable.
    pub fn short_name(&self) -> String {
        match self.harness_slug() {
            Some(slug) if !self.name.starts_with(&format!("{slug}-")) => {
                let segments: Vec<&str> = self.name.split('-').collect();
                let short = match segments.as_slice() {
                    [.., suffix] if segments.len() >= 3 => suffix,
                    _ => self.name.as_str(),
                };
                format!("{slug}-{short}")
            }
            _ => self.name.clone(),
        }
    }

    /// The thread list's label: what is happening in the thread, truncated to
    /// the tree's width. While the harness works, the prompt names the work;
    /// once the turn is over, what the agent last said is the news (known for
    /// railway-agent threads, whose daemon serves the transcript). The short
    /// name is the fallback for a session nothing has reported from (a plain
    /// shell, a run that hasn't spoken yet).
    pub fn thread_label(&self) -> String {
        if let Some(snapshot) = &self.snapshot {
            fn clean(text: Option<&str>) -> Option<&str> {
                text.map(str::trim).filter(|text| !text.is_empty())
            }
            let prompt = clean(
                snapshot
                    .latest_prompt
                    .as_deref()
                    .or(snapshot.prompt.as_deref()),
            );
            let reply = clean(snapshot.last_reply.as_deref());
            let text = if snapshot.state == "working" {
                prompt.or(reply)
            } else {
                reply.or(prompt)
            };
            if let Some(text) = text {
                return truncate(text, 28);
            }
        }
        format!("[S] {}", self.short_name())
    }

    /// The harness this session runs, read off its launch line's binary.
    fn harness_slug(&self) -> Option<&'static str> {
        let summary = self.command_summary();
        if summary == self.name {
            return None;
        }
        let binary = summary.split_whitespace().next()?;
        let base = binary.rsplit('/').next().unwrap_or(binary);
        match base {
            "railway-agent-tui" | "railway-agent" => Some("railway"),
            "claude" => Some("claude"),
            "codex" => Some("codex"),
            "grok" => Some("grok"),
            _ => None,
        }
    }

    /// The command, cleaned up for the detail pane: the harness invocation
    /// lifted out of the launch line's plumbing.
    pub fn command_summary(&self) -> String {
        let Some(command) = self
            .command
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        else {
            return self.name.clone();
        };
        let meaningful = match command.split_once(LAUNCH_PROLOGUE) {
            Some((_, rest)) => rest.split(';').next().unwrap_or(rest).trim(),
            None => command.lines().next().unwrap_or(command).trim(),
        };
        if meaningful.is_empty() {
            return self.name.clone();
        }
        truncate(meaningful, 160)
    }
}

/// Trim to `max` characters on a character boundary, with an ellipsis.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

impl LaunchRequest {
    /// Should this open a session of its own?
    ///
    /// A prompt is a new piece of work — submitting a second one must start a
    /// second agent session, not drop you back into the first. Only a plain
    /// "connect to that agent" reuses what is already open.
    pub fn wants_new_session(&self) -> bool {
        self.session_name.is_none() && (self.new_session || self.prompt.is_some() || self.force_new)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadSessions {
    NotLoaded,
    Loading,
    Loaded(Vec<ConsoleSession>),
    Failed(String),
}

/// Per-environment agent list. `Failed` is kept rather than collapsed into an
/// empty list so a network blip reads as a network blip instead of "you have no
/// agents here", which would invite creating a duplicate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load {
    NotLoaded,
    Loading,
    Loaded(Vec<Agent>),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct EnvNode {
    pub id: String,
    pub name: String,
    pub expanded: bool,
    pub agents: Load,
}

#[derive(Clone, Debug)]
pub struct ProjectNode {
    pub id: String,
    pub name: String,
    pub expanded: bool,
    pub envs: Vec<EnvNode>,
}

#[derive(Clone, Debug)]
pub struct WorkspaceNode {
    pub id: String,
    pub name: String,
    pub expanded: bool,
    pub projects: Vec<ProjectNode>,
}

/// Where the prompt lands, and what a bare "Launch" acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub project_id: String,
    pub project_name: String,
    pub environment_id: String,
    pub environment_name: String,
}

impl Target {
    pub fn label(&self) -> String {
        format!("{}/{}", self.project_name, self.environment_name)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    /// First-run setup, over the tree.
    Setup,
    /// The ⌥s settings card, over the tree: every preference setup collects,
    /// changeable after the fact.
    Settings,
    /// Choosing where the prompt lands, over the tree. The same card list the
    /// setup flow asks with — picking a target is the same question, so it
    /// should not send anyone through the whole management tree to answer it.
    TargetPick,
    /// ⌥n on Manage: choosing which agent a new session runs, over the tree.
    HarnessPick,
    /// ⌥p on Manage: composing a prompt for a new session, over the tree —
    /// the launcher's prompt box, without the walk back to the New Session
    /// row.
    ManagePrompt,
    Manage,
}

/// A short confirmation in the bottom corner.
///
/// For the things that happen without a screen of their own — a drag that put
/// text on the clipboard is the case that needs it, because the only other
/// evidence is the clipboard itself, which you cannot see. It fades on its own;
/// nothing has to be dismissed.
/// An agent that has been told to wake or sleep and has not got there yet.
///
/// Waking returns the moment the platform accepts it; the VM then boots on its
/// own time, and the list keeps reporting `sleeping` until it does. Dropping
/// the pending label when the mutation returned made that look like the wake
/// had failed and rolled back — it hadn't, which is why launching onto the
/// agent worked seconds later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentWatch {
    /// The status this is heading for.
    pub want: &'static str,
    pub environment_id: String,
    pub until: std::time::Instant,
}

/// How long to keep asking. A cold VM can take a while to boot, and giving up
/// early puts the wrong state back on the screen — the failure this whole
/// mechanism exists to avoid.
pub const WAKE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(180);
/// Sleeping is a request the platform acts on quickly.
pub const SLEEP_PATIENCE: std::time::Duration = std::time::Duration::from_secs(60);
/// How often to ask again while waiting.
pub const WATCH_TICK: std::time::Duration = std::time::Duration::from_millis(1500);

/// How many background attaches may be in flight at once. Startup wants the
/// whole tree green, but each attach is a relay ssh, a reader thread, and a
/// scrollback replay — a fleet's worth at once is a thundering herd.
const AUTO_CONNECT_INFLIGHT: usize = 3;

/// How often the tree asks the platform for everything again on its own.
///
/// One `myCloudAgents` request covers the whole account — every workspace,
/// project and environment — so the cost of this is one request per interval no
/// matter how large the account is, plus a session query for each agent someone
/// has open or expanded (usually none or one). For scale: the dashboard polls
/// `cloudAgents` every 15s *per environment* it is showing, and every 3s while
/// any agent is in a transient state.
///
/// Enough of the same work already happens on demand — a launch refetches its
/// environment, a wake polls at [`WATCH_TICK`], ⌥r asks immediately — that this
/// is a safety net for changes made somewhere else entirely: another terminal,
/// the dashboard, a teammate. 25s is short enough that nobody reaches for ⌥r out
/// of doubt, and long enough to be invisible in anyone's rate-limit budget.
pub const AUTO_REFRESH_EVERY: std::time::Duration = std::time::Duration::from_secs(25);

/// How often the watched threads' sessions are re-asked about, for the
/// sidebar's live labels. See [`App::thread_refresh_in`].
pub const THREAD_REFRESH_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub at: std::time::Instant,
    /// A tick or a cross; a failure that looks like a success is worse than no
    /// toast at all.
    pub ok: bool,
}

/// Long enough to read four words, short enough not to sit on the screen.
pub const TOAST_LIFETIME: std::time::Duration = std::time::Duration::from_millis(1800);

impl Toast {
    /// Errors carry things to read — a failure reason, sometimes a recipe —
    /// and stay up long enough to read them. A confirmation needs a glance.
    fn lifetime(&self) -> std::time::Duration {
        if self.ok {
            TOAST_LIFETIME
        } else {
            TOAST_LIFETIME * 5
        }
    }

    pub fn expired(&self) -> bool {
        self.at.elapsed() >= self.lifetime()
    }

    /// Time until this toast has had its moment.
    pub fn remaining(&self) -> std::time::Duration {
        self.lifetime().saturating_sub(self.at.elapsed())
    }
}

/// The target chooser: the setup flow's project card, on its own.
pub struct TargetPicker {
    pub options: Vec<Target>,
    pub cursor: usize,
}

impl TargetPicker {
    /// Every environment in the tree, the default project's first — one row per
    /// place an agent can actually live, since that is what is being chosen.
    pub fn new(
        tree: &[WorkspaceNode],
        default_project: Option<&str>,
        current: Option<&Target>,
    ) -> Self {
        let mut options: Vec<Target> = Vec::new();
        for ws in tree {
            for p in sorted_projects(ws, default_project) {
                let project = &ws.projects[p];
                for env in &project.envs {
                    options.push(Target {
                        project_id: project.id.clone(),
                        project_name: project.name.clone(),
                        environment_id: env.id.clone(),
                        environment_name: env.name.clone(),
                    });
                }
            }
        }
        // Open on the current target, so Enter twice is a no-op rather than a
        // surprise change.
        let cursor = current
            .and_then(|t| {
                options
                    .iter()
                    .position(|o| o.environment_id == t.environment_id)
            })
            .unwrap_or(0);
        Self { options, cursor }
    }

    /// The rows to draw: (label, note).
    pub fn rows(&self, default_project: Option<&str>) -> Vec<(String, String)> {
        self.options
            .iter()
            .map(|t| {
                let note = if Some(t.project_id.as_str()) == default_project {
                    "default".to_string()
                } else {
                    String::new()
                };
                (format!("{} ({})", t.project_name, t.environment_name), note)
            })
            .collect()
    }
}

/// A rectangle recorded by the renderer so mouse events can be mapped back to
/// the pane that drew it. Plain numbers rather than a ratatui `Rect` so the app
/// stays free of the drawing layer.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct PaneBox {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl PaneBox {
    pub fn contains(&self, col: u16, row: u16) -> bool {
        self.w > 0
            && self.h > 0
            && col >= self.x
            && row >= self.y
            && col < self.x + self.w
            && row < self.y + self.h
    }
}

/// Where each pane last drew itself. Recorded every frame; read by the mouse.
///
/// Two rectangles per pane: the whole block, which is what a click has to hit
/// (nobody aims for the inside of a border), and the interior, which is what a
/// selection may cover.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct PaneRects {
    pub tree: PaneBox,
    pub session: PaneBox,
    pub tree_outer: PaneBox,
    pub session_outer: PaneBox,
    /// The new-session prompt box, borders included.
    pub prompt: PaneBox,
    /// The header's session tabs, drawn only while the pane is maximized. A
    /// fixed array so this stays `Copy`; sessions past the cap keep their
    /// ⌥⇧[ ⌥⇧] keys but aren't clickable.
    pub tabs: [PaneBox; MAX_TABS],
}

/// How many maximized-header tabs get a clickable box.
pub const MAX_TABS: usize = 8;

/// A drag in progress, confined to one pane. Copying out of the session must
/// not pick up tree rows sitting at the same screen rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    pub pane: ManageFocus,
    pub anchor: (u16, u16),
    pub cursor: (u16, u16),
}

impl Selection {
    /// Start and end in reading order, so dragging up or backwards selects the
    /// same text as dragging down or forwards.
    fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        let (ax, ay) = self.anchor;
        let (cx, cy) = self.cursor;
        if (ay, ax) <= (cy, cx) {
            ((ax, ay), (cx, cy))
        } else {
            ((cx, cy), (ax, ay))
        }
    }

    /// The selected cells, as one inclusive column range per row.
    ///
    /// Line-wise, the way a terminal selects: from the anchor to the end of its
    /// line, whole lines through the middle, and the start of the last line up
    /// to the cursor. A rectangle would be right for a column of numbers and
    /// wrong for everything else — selecting three lines of agent output would
    /// clip every one of them to the same columns.
    pub fn spans(&self, bounds: PaneBox) -> Vec<(u16, u16, u16)> {
        if bounds.w == 0 || bounds.h == 0 {
            return Vec::new();
        }
        let left = bounds.x;
        let right = bounds.x + bounds.w - 1;
        let ((sx, sy), (ex, ey)) = self.ordered();
        let (sy, ey) = (sy.max(bounds.y), ey.min(bounds.y + bounds.h - 1));
        if sy > ey {
            return Vec::new();
        }
        (sy..=ey)
            .map(|y| {
                let x0 = if y == sy { sx.max(left) } else { left };
                let x1 = if y == ey { ex.min(right) } else { right };
                (y, x0, x1)
            })
            .filter(|(_, x0, x1)| x0 <= x1)
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.cursor
    }
}

/// What the keyboard is driving on the Manage screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ManageFocus {
    Tree,
    /// The session pane — keystrokes go to the agent.
    Session,
}

/// A row in the flattened tree. Indices point back into [`App::tree`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    /// The pinned first row: standing on it puts the keyboard in the prompt
    /// and the session pane shows the launcher — the way a new session
    /// starts, without a separate screen to come from.
    NewSession,
    Workspace(usize),
    Project(usize, usize),
    Environment(usize, usize, usize),
    Agent(usize, usize, usize, usize),
    /// A reattachable session on an agent's VM.
    Session(usize, usize, usize, usize, usize),
    /// A non-selectable line under an environment: loading, empty, or failed.
    Note(usize, usize, usize),
    /// The collapsible tail of projects with no agents — where `n` goes to
    /// start somewhere new.
    OtherProjects,
    /// A non-selectable line that belongs to no environment: the empty state.
    Hint,
    /// A rule between the agent groups and the projects tail.
    Separator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub depth: usize,
    pub kind: RowKind,
    pub label: String,
    /// Right-hand annotation: agent count, status, workspace marker.
    pub note: String,
    /// `Some` for agents, driving the status glyph and its colour.
    pub status: Option<String>,
    pub expanded: Option<bool>,
    /// Drawn de-emphasised: there is nothing here yet.
    pub dimmed: bool,
}

impl Row {
    pub fn selectable(&self) -> bool {
        !matches!(
            self.kind,
            RowKind::Note(..) | RowKind::Hint | RowKind::Separator
        )
    }
}

/// A lifecycle action on one agent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentOp {
    Sleep,
    Wake,
    Delete,
}

impl AgentOp {
    /// The optimistic label shown against the row while it runs. Present tense
    /// on purpose: the row is claiming what is happening, not what happened.
    pub fn pending_label(self) -> &'static str {
        match self {
            AgentOp::Sleep => "sleeping…",
            AgentOp::Wake => "waking…",
            AgentOp::Delete => "deleting…",
        }
    }
}

/// An action held back for a `y`/`n`. Only the ones that lose work need it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PendingConfirm {
    pub op: AgentOp,
    pub agent_id: String,
    pub agent_name: String,
    pub environment_id: String,
}

impl PendingConfirm {
    pub fn question(&self) -> String {
        match self.op {
            AgentOp::Delete => format!(
                "Delete {} and its disk? This cannot be undone.  y / n",
                self.agent_name
            ),
            AgentOp::Sleep => format!("Sleep {}?  y / n", self.agent_name),
            AgentOp::Wake => format!("Wake {}?  y / n", self.agent_name),
        }
    }
}

/// What the startup check learned about the user's SSH key. Connecting to an
/// agent rides SSH and the relay only answers registered keys, so a connect
/// while unregistered is held behind [`SshGate`] rather than sent to fail —
/// or worse, to the relay's interactive signup screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SshKeyState {
    /// The check hasn't answered, or couldn't. Connects proceed; the launch
    /// pipeline re-checks and produces the error if there is one.
    #[default]
    Unknown,
    /// A local key is registered with Railway.
    Ready,
    /// A local key exists but Railway doesn't know it yet.
    NeedsRegistration(SshKeyOffer),
    /// Nothing in the SSH agent or ~/.ssh to register.
    NoLocalKeys,
}

/// The local key the gate offers to register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshKeyOffer {
    pub name: String,
    pub fingerprint: String,
    pub public_key: String,
}

/// A connect held back until the SSH key question is answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeldConnect {
    Launch(LaunchRequest),
    Reattach {
        agent_id: String,
        agent_name: String,
        environment_id: String,
        session_name: String,
    },
}

impl HeldConnect {
    /// Back into the effect it was before the gate held it.
    pub fn into_effect(self) -> Effect {
        match self {
            HeldConnect::Launch(req) => Effect::Launch(req),
            HeldConnect::Reattach {
                agent_id,
                agent_name,
                environment_id,
                session_name,
            } => Effect::Reattach {
                agent_id,
                agent_name,
                environment_id,
                session_name,
            },
        }
    }
}

/// The register-your-key question, floated over whatever raised it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshGate {
    pub offer: SshKeyOffer,
    /// Resumed after a successful registration. `None` when the gate came
    /// from setup rather than a connect: declining just ends the question.
    pub then: Option<HeldConnect>,
}

/// What the loading screen is showing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Loading {
    /// A launch is in flight; the session pane shows its progress. Kept as a
    /// pane state rather than a screen so the tree stays visible and usable
    /// while an agent boots.
    pub active: bool,
    /// Where this is going, e.g. `devtools/production`.
    pub target: String,
    pub harness: String,
    /// The task typed into the prompt, echoed back so the wait has context.
    pub prompt: Option<String>,
    /// Steps reported by the launch pipeline, oldest first.
    pub steps: Vec<String>,
    /// Advanced by the loop's tick; drives the spinner so a slow step still
    /// looks alive.
    pub tick: usize,
}

/// The mouse gestures the TUI reacts to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseAction {
    Down,
    Drag,
    Up,
    ScrollUp,
    ScrollDown,
}

/// What the event loop must do after a key. At most one per keystroke.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Fetch this environment's agents; the result comes back via
    /// [`App::agents_loaded`].
    LoadAgents {
        environment_id: String,
        path: (usize, usize, usize),
    },
    /// Fetch the reattachable sessions on one agent, along with the harness
    /// snapshots that label them (which need the environment id).
    LoadSessions {
        agent_id: String,
        environment_id: String,
        path: (usize, usize, usize, usize),
    },
    Launch(LaunchRequest),
    /// Close one session (by index) and sleep its agent.
    CloseSession {
        index: usize,
    },
    /// End a session on the agent. Its pane, if we have one, goes with it.
    KillSession {
        agent_id: String,
        environment_id: String,
        session_name: String,
    },
    /// Reconnect to an existing session on a running agent — no provisioning,
    /// no credential work, just ssh with the session's name.
    Reattach {
        agent_id: String,
        agent_name: String,
        environment_id: String,
        session_name: String,
    },
    /// Create the default project the wizard asked for, in this workspace.
    CreateDefaultProject(String),
    /// Persist what first-run setup collected.
    SaveSetup(Box<super::wizard::Outcome>),
    /// Persist a change made on the settings card. The same snapshot shape as
    /// setup, but merged over the file on disk rather than replacing it.
    SaveSettings(Box<super::wizard::Outcome>),
    /// Remember the default project chosen from the target card.
    SaveDefaultProject(Box<Target>),
    /// Open a link that was double-clicked in a session.
    OpenUrl(String),
    /// Look for agents in every project, on request. See
    /// [`App::scan_environments`].
    ScanEverywhere,
    /// Ask the platform for everything again: one account-wide agent query,
    /// plus the sessions of the agents someone is actually looking at. Raised
    /// by ⌥r, by the auto-refresh tick, and on re-entry after the TUI has
    /// handed the terminal back. See [`super::start_refresh`].
    RefreshAll,
    /// Put an `ssh` command for one session on the clipboard.
    CopySsh {
        agent_id: String,
        environment_id: String,
        session_name: String,
    },
    /// Leave the TUI and give the whole terminal to one session.
    FullScreen {
        agent_id: String,
        session_name: String,
        agent_name: String,
    },
    /// Run a lifecycle action, then refresh the environment it lives in.
    Agent {
        op: AgentOp,
        agent_id: String,
        environment_id: String,
    },
    /// Register the gate's key with Railway, then resume the held connect.
    RegisterSshKey {
        offer: SshKeyOffer,
        then: Option<HeldConnect>,
    },
    /// Leave the TUI for the Claude mint's manual-paste fallback; the caller
    /// re-enters with the same request. Raised only after the hidden mint
    /// already ran and failed under the frame.
    StepOutForMint(LaunchRequest),
    Quit,
}

/// One session the background auto-connect should attach — everything a
/// reattach needs, lifted out of the tree so the loop can spawn the ssh work
/// without holding a borrow on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoConnect {
    pub agent_id: String,
    pub agent_name: String,
    pub environment_id: String,
    pub session_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchRequest {
    pub project_id: String,
    pub environment_id: String,
    /// `Some` connects to this specific agent; `None` reuses or creates one.
    pub agent_id: Option<String>,
    /// Reattach to this durable session instead of starting the harness.
    pub session_name: Option<String>,
    /// Always create a fresh agent, even when one exists.
    pub force_new: bool,
    /// Open another session on the agent this request names, rather than
    /// reusing the one already open.
    pub new_session: bool,
    pub harness: String,
    pub prompt: Option<String>,
    /// Human-facing description of where this is going, for the handoff line.
    pub label: String,
    /// The command-line flags this launch started from, when a command line
    /// started it. The fields above overwrite the target, harness, agent and
    /// prompt; everything else — `--name`, `--variable`, `--env-file`,
    /// `--refresh-auth` — rides along from here, because nothing in the TUI
    /// asks for those and dropping them would quietly ignore what was typed.
    /// Default for launches the TUI starts itself.
    ///
    /// Boxed because a `LaunchRequest` is the largest thing several enums
    /// carry — `Effect`, `HeldConnect`, `Outcome`, `Message` — and inlining
    /// another 200 bytes of flags widens every one of them for a field only
    /// the command line ever fills.
    pub base: Box<crate::commands::code::LaunchArgs>,
}

pub struct App {
    /// The project new agents go to: the linked directory's project, or the
    /// preferences file when this directory has no link. Sorted to the top of
    /// the tree and separated from the rest.
    pub default_project: Option<String>,
    /// Whether preferences exist yet. Decides whether first run offers the
    /// setup wizard or leaves changing things to the ⌥s settings card.
    pub configured: bool,
    /// Environments this machine has launched an agent in, from the CLI's own
    /// records. Loaded eagerly, because an agent you made is one you expect to
    /// find without hunting for it.
    pub known_environments: Vec<String>,
    /// The target chooser, while it is open.
    pub target_pick: Option<TargetPicker>,
    /// The agent `n` was pressed on, when it was: the picked harness launches
    /// a new session on that box rather than minting a fresh agent.
    pub harness_pick_agent: Option<String>,
    /// ⌥n's picker cursor while [`Screen::HarnessPick`] is up.
    pub harness_pick: Option<usize>,
    /// ⌥p's draft while [`Screen::ManagePrompt`] is up.
    pub manage_prompt: Option<String>,
    /// The session pane has the whole screen: no tree, no detail column.
    pub maximized: bool,
    /// A launch to start as soon as the first frame is up, and then forget.
    ///
    /// How `railway code` opens: it has already answered where and which
    /// harness, so there is nothing to browse for — the TUI is here to hold
    /// the session, not to ask a question. Taken by the loop and fed through
    /// the same path a keypress would, so the ssh-key gate and the Claude
    /// mint still get their say.
    pub autostart: Option<LaunchRequest>,
    /// Like `autostart`, but the pipeline is already running — started beside
    /// the tree load because its gates were verified up front. The loop adopts
    /// it on frame one instead of dispatching.
    pub autostart_inflight: Option<super::InflightLaunch>,
    /// Leave the TUI when the last session pane closes on its own.
    ///
    /// `railway code` came for one session; when the harness exits (ctrl-c
    /// included), the person is done, and the right place to land is their
    /// local prompt — not a browser they never opened. Bare `railway ca`
    /// keeps its tree instead.
    pub quit_when_done: bool,
    /// One line for the caller to print after the terminal is restored — how
    /// a quit that closed a session says the agent is still running (and
    /// billing) now that the frame it would have said it in is gone.
    pub exit_note: Option<String>,
    /// This gesture belongs to the application in the session, not to us.
    pointer_to_app: bool,
    /// A short confirmation in the corner, and when it was raised.
    pub toast: Option<Toast>,
    pub theme: &'static Theme,
    pub screen: Screen,
    /// Open sessions, in the order they were opened. Several can run at once;
    /// the pane renders whichever is active and the rest keep working.
    pub sessions: Vec<super::session::Session>,
    /// Index into `sessions` of the one being rendered.
    pub active: Option<usize>,
    pub focus: ManageFocus,
    /// The task being prepared, and the steps reported so far.
    pub loading: Loading,
    /// After a launch, select this agent once its environment has loaded.
    pub pending_select: Option<String>,
    /// After the next load, expand this agent.
    pub pending_expand: Option<String>,
    /// After sessions load, put the cursor on this durable name.
    pub pending_select_session: Option<String>,
    /// Where the panes were drawn last frame, for hit-testing the mouse.
    pub panes: PaneRects,
    /// Sessions we have asked to end. Hidden from the tree the moment the kill
    /// is sent: the agent takes a second or two to reap the processes, and a
    /// row that reads "running" in the meantime looks like the key did nothing.
    /// A name leaves this set when a refresh no longer lists it.
    pub ending: std::collections::HashSet<String>,
    /// Session names whose attach is in flight — the row wears a spinner
    /// instead of the branch marker until the pane opens or the attempt fails.
    pub connecting: std::collections::HashSet<String>,
    /// Session names auto-connect has already tried this run. One attempt
    /// each: a failed relay path must not be retried in a loop, and a pane
    /// closed with `x` must stay closed rather than reopening on the next
    /// sweep. A name lands here when its attempt starts and whenever a pane
    /// attaches by any path.
    auto_attempted: std::collections::HashSet<String>,
    /// Session names whose dropped connection has already pulled the keyboard
    /// to its pane — once per drop, so Esc-ing away sticks. Cleared when the
    /// name reattaches, so a second drop announces itself the same way.
    drop_seen: std::collections::HashSet<String>,
    /// First-run setup, when there are no preferences yet.
    pub wizard: Option<super::wizard::Wizard>,
    /// The ⌥s settings card, while it is open.
    pub settings: Option<super::settings::Settings>,
    /// Which local directory skills would come from, if any.
    pub skills_source: Option<String>,
    /// Whether the skills preference is on, mirrored from the file so the
    /// settings card opens showing the truth.
    pub skills_enabled: bool,
    /// Hide the maximized layout's header tabs (⌥s settings): ⌥⇧[ ⌥⇧] stay
    /// the way between sessions, and the header keeps its status line.
    pub hide_tabs: bool,
    /// The key overlay is open.
    pub keys_open: bool,
    /// A drag in progress or a completed selection.
    pub selection: Option<Selection>,
    /// Where and when the last click landed, for spotting a double click.
    last_click: Option<((u16, u16), std::time::Instant)>,
    /// A selection whose text should be lifted out of the next frame. The
    /// rendered buffer is the only place the session pane's text exists in
    /// screen coordinates, and it can only be read from inside a draw.
    pub pending_copy: Option<Selection>,
    /// An action waiting on a `y`/`n`.
    pub confirm: Option<PendingConfirm>,
    /// What the startup check learned about the user's SSH key.
    pub ssh_key: SshKeyState,
    /// A register-your-key question holding a connect, when one is up.
    pub ssh_gate: Option<SshGate>,
    /// Agent id → what is happening to it right now. Shown immediately so a
    /// slow mutation looks like an action rather than a frozen key.
    pub ops: std::collections::HashMap<String, &'static str>,
    /// Agents whose state is still on its way. See [`AgentWatch`].
    pub watching: std::collections::HashMap<String, AgentWatch>,
    /// A refresh is in flight. Coalescing: a held ⌥r, or several actions
    /// finishing at once, must not stack account-wide queries.
    pub refreshing: bool,
    /// When the last refresh of any origin started, which is what the automatic
    /// one measures from — pressing ⌥r pushes the next tick out rather than
    /// having it arrive a second later.
    pub last_refresh: Option<std::time::Instant>,
    /// Refreshing is paused until this passes, because the API said so. A 429
    /// answered by polling harder is how a rate limit becomes a longer rate
    /// limit.
    pub refresh_paused_until: Option<std::time::Instant>,
    /// When the watched threads were last re-asked about — the fast cadence
    /// behind the sidebar's live labels, separate from the account refresh.
    pub last_thread_refresh: Option<std::time::Instant>,
    /// Agents whose fast thread poll is still in flight, so a slow reply (the
    /// gate transcript dials can take seconds) is never stacked under a
    /// second ask for the same agent.
    pub thread_polls: std::collections::HashSet<String>,
    /// Someone pressed a key for the refresh in flight, so its result is worth
    /// a line when it lands. See [`App::refreshed`].
    pub refresh_announce: bool,
    /// Environment id → when its agent list last came back from the platform.
    /// Keeps a slower account-wide reply from overwriting fresher per-environment
    /// news; see [`App::my_agents_loaded`].
    pub answered_at: std::collections::HashMap<String, std::time::Instant>,
    /// `myCloudAgents` is not available to this caller, so a refresh has to ask
    /// per environment. True for a workspace-scoped `RAILWAY_TOKEN` — the field
    /// requires an authenticated user — and for a backboard old enough not to
    /// have the field at all. Learned from the first failure, so the fallback
    /// costs one wasted request per run rather than being guessed at.
    pub account_query_unavailable: bool,
    /// Whether the New Session row's prompt has the keyboard. True whenever
    /// the cursor arrives on the row; Esc there sets it false — the "home"
    /// state, where letters are keys again and `q` quits.
    pub prompt_focused: bool,
    pub prompt: String,
    /// Where the next character lands in `prompt`, as a char offset — ←/→
    /// move it, so a typo mid-sentence is fixable in place.
    pub prompt_cursor: usize,
    pub harness: usize,
    pub target: Option<Target>,
    pub tree: Vec<WorkspaceNode>,
    pub cursor: usize,
    /// Whether the projects tail is open. `None` decides automatically: open
    /// while there are no agents to show — the tail is the whole tree then —
    /// and folded away once agent groups exist to lead with.
    pub others_expanded: Option<bool>,
    /// Transient one-line message shown in the header.
    pub status: String,
}

impl App {
    pub fn new(
        tree: Vec<WorkspaceNode>,
        target: Option<Target>,
        harness: Option<&str>,
        theme: Option<&str>,
        default_project: Option<String>,
        configured: bool,
    ) -> Self {
        let harness = harness
            .and_then(|h| HARNESSES.iter().position(|x| *x == h))
            .unwrap_or(0);
        let mut app = Self {
            default_project,
            configured,
            known_environments: Vec::new(),
            target_pick: None,
            harness_pick: None,
            harness_pick_agent: None,
            manage_prompt: None,
            maximized: false,
            autostart: None,
            autostart_inflight: None,
            quit_when_done: false,
            exit_note: None,
            pointer_to_app: false,
            toast: None,
            theme: Theme::from_slug(theme),
            screen: Screen::Manage,
            sessions: Vec::new(),
            active: None,
            focus: ManageFocus::Tree,
            loading: Loading::default(),
            pending_select: None,
            pending_expand: None,
            pending_select_session: None,
            panes: PaneRects::default(),
            ending: std::collections::HashSet::new(),
            connecting: std::collections::HashSet::new(),
            auto_attempted: std::collections::HashSet::new(),
            drop_seen: std::collections::HashSet::new(),
            wizard: None,
            settings: None,
            skills_source: None,
            skills_enabled: false,
            hide_tabs: false,
            keys_open: false,
            selection: None,
            last_click: None,
            pending_copy: None,
            confirm: None,
            ssh_key: SshKeyState::default(),
            ssh_gate: None,
            ops: std::collections::HashMap::new(),
            watching: std::collections::HashMap::new(),
            refreshing: false,
            last_refresh: None,
            refresh_paused_until: None,
            last_thread_refresh: None,
            thread_polls: std::collections::HashSet::new(),
            refresh_announce: false,
            answered_at: std::collections::HashMap::new(),
            account_query_unavailable: false,
            prompt_focused: true,
            prompt: String::new(),
            prompt_cursor: 0,
            harness,
            target,
            tree,
            cursor: 0,
            others_expanded: None,
            status: String::new(),
        };
        // Open the first workspace so the projects tail is never a wall of
        // collapsed rows on a multi-workspace account.
        if let Some(ws) = app.tree.first_mut() {
            ws.expanded = true;
        }
        app.clamp_cursor();
        // The tree can lead with an unselectable hint; the cursor belongs on
        // the first row a key can act on.
        if app.selected_row().is_some_and(|row| !row.selectable()) {
            app.move_cursor_inner(1);
        }
        app
    }

    /// Adopt a harness slug, ignoring one we don't know — the preferences file
    /// is user-editable and a typo there should not change the selection.
    pub fn set_harness(&mut self, slug: Option<&str>) {
        if let Some(i) = slug.and_then(|s| HARNESSES.iter().position(|x| *x == s)) {
            self.harness = i;
        }
    }

    /// Open the target chooser over the tree.
    pub fn start_target_pick(&mut self) {
        self.target_pick = Some(TargetPicker::new(
            &self.tree,
            self.default_project.as_deref(),
            self.target.as_ref(),
        ));
        self.screen = Screen::TargetPick;
    }

    fn on_key_target_pick(&mut self, key: KeyEvent) -> Option<Effect> {
        let picker = self.target_pick.as_mut()?;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                picker.cursor = picker.cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.cursor = (picker.cursor + 1).min(picker.options.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                let picked = picker.options.get(picker.cursor).cloned();
                self.target_pick = None;
                self.screen = Screen::Manage;
                // This card is where the default project is set, not just where
                // this run is pointed: it is the same question setup asks, and
                // answering it twice — once here, once in setup — would be a
                // way to end up with two different answers.
                if let Some(target) = picked {
                    self.status = format!("Default project set to {}", target.label());
                    self.default_project = Some(target.project_id.clone());
                    self.target = Some(target.clone());
                    return Some(Effect::SaveDefaultProject(Box::new(target)));
                }
            }
            KeyCode::Esc => {
                self.target_pick = None;
                self.screen = Screen::Manage;
            }
            _ => {}
        }
        None
    }

    /// Close the pane's window on the world along with the pane: a maximized
    /// layout with no session in it is an empty screen.
    fn unmaximize_without_a_session(&mut self) {
        if self.maximized && self.active.is_none() {
            self.maximized = false;
        }
    }

    /// Is the right-hand pane taking the whole screen — no tree, no detail
    /// column?
    ///
    /// The layout and the terminal emulator both have to agree on this, or a
    /// remote TUI wraps at the wrong column, so both ask here rather than
    /// reading `maximized` and re-deriving the rest. A launch in flight counts:
    /// `railway code` collapses the tree from the first frame, and the loading
    /// screen is what stands in for the session until there is one. A
    /// maximized flag with neither is not a full pane — it is a blank screen,
    /// which is what happens between a failed launch and the next key.
    pub fn pane_is_full(&self) -> bool {
        self.maximized && (self.active.is_some() || self.loading.active)
    }

    /// Open setup over the tree.
    ///
    /// `ask_first` puts the "set up cloud agents?" question in front of it,
    /// which is right when nobody asked for setup — a first run — and wrong
    /// when they asked for it themselves and have already answered it.
    pub fn start_wizard(&mut self, ask_first: bool) {
        let mut wizard = super::wizard::Wizard::new(
            &self.tree,
            Some(self.harness_name()),
            self.theme,
            self.skills_source.clone(),
            self.hide_tabs,
        );
        if !ask_first {
            wizard.skip_intro();
        }
        self.wizard = Some(wizard);
        self.screen = Screen::Setup;
    }

    /// Close it, whatever the reason.
    pub fn end_wizard(&mut self) {
        self.wizard = None;
        self.screen = Screen::Manage;
    }

    /// Open the ⌥s settings card over the tree, seeded with what is saved.
    ///
    /// The default project's names come from the target, which holds the
    /// saved default whenever one exists — the tree would only have the id.
    pub fn start_settings(&mut self) {
        let project = self
            .default_project
            .as_ref()
            .and(self.target.as_ref())
            .map(|t| super::wizard::ProjectOption {
                project_id: t.project_id.clone(),
                project_name: t.project_name.clone(),
                environment_id: t.environment_id.clone(),
                environment_name: t.environment_name.clone(),
            });
        // The settings card is over the saved default, whose index space is
        // `default_harnesses()` — a live shell selection has no row there and
        // falls back to the first entry rather than clamping onto the last
        // real agent.
        let agent = default_harnesses()
            .iter()
            .position(|slug| *slug == self.harness_name())
            .unwrap_or(0);
        self.settings = Some(super::settings::Settings::new(
            &self.tree,
            project,
            agent,
            self.skills_enabled,
            self.skills_source.clone(),
            self.theme,
            self.hide_tabs,
        ));
        self.screen = Screen::Settings;
    }

    /// Close it; every change was already saved on the way.
    pub fn end_settings(&mut self) {
        self.settings = None;
        self.screen = Screen::Manage;
    }

    /// Adopt a theme slug; an unknown one leaves the current theme alone.
    pub fn set_theme(&mut self, slug: Option<&str>) {
        if slug.is_some() {
            self.theme = Theme::from_slug(slug);
        }
    }

    pub fn harness_name(&self) -> &'static str {
        HARNESSES[self.harness.min(HARNESSES.len() - 1)]
    }

    /// Is the shell option selected? It launches no harness, so the prompt
    /// boxes go inert while it is: there is nothing to hand a prompt to.
    pub fn shell_selected(&self) -> bool {
        self.harness_name() == "shell"
    }

    /// The flattened, currently-visible tree: agent groups first, then the
    /// projects tail.
    ///
    /// Agents are what this screen is about, so every environment that has
    /// any is promoted to a top-level group — always open, never a level to
    /// expand. The containers survive as context rather than navigation: the
    /// group is labelled with its project (and environment, when that adds
    /// something), and projects with nothing in them wait in a collapsible
    /// tail at the bottom, which is where `n` goes to start an agent
    /// somewhere new.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        // The launcher, pinned first: where the cursor starts, and where it
        // goes back to for the next task. Standing here puts the keyboard in
        // the prompt and the launcher in the session pane.
        rows.push(Row {
            depth: 0,
            kind: RowKind::NewSession,
            label: "New Agent".into(),
            note: String::new(),
            status: None,
            expanded: None,
            dimmed: false,
        });
        // Ruled off from the list below it: the launcher is an action, and
        // without the line it reads as the first thread.
        rows.push(separator_row());
        let groups = self.agent_groups();
        for &(w, p, e) in &groups {
            self.push_thread_rows(&mut rows, w, p, e);
        }
        if groups.is_empty() {
            // "None yet" is a definitive claim, so it waits until every
            // environment has actually answered; anything still on its way
            // reads as searching, and a failure says so rather than passing
            // itself off as an empty account.
            let mut envs = self
                .tree
                .iter()
                .flat_map(|ws| ws.projects.iter())
                .flat_map(|project| project.envs.iter());
            let searching = envs
                .clone()
                .any(|env| matches!(env.agents, Load::NotLoaded | Load::Loading));
            let failed = envs.any(|env| matches!(env.agents, Load::Failed(_)));
            rows.push(Row {
                depth: 0,
                kind: RowKind::Hint,
                label: if searching {
                    "looking for cloud agents…".into()
                } else if failed {
                    "couldn't check every environment — r retries".into()
                } else {
                    "no threads yet — the prompt above starts one".into()
                },
                note: String::new(),
                status: None,
                expanded: None,
                dimmed: false,
            });
        }
        self.push_project_tail(&mut rows, &groups);
        rows
    }

    /// One environment's threads, flat: every running session is an entry of
    /// its own — an agent thread — and an agent with no sessions keeps a row
    /// so it stays reachable (to wake, connect to, or start a thread on).
    /// The containers are context, not levels: the project rides along as the
    /// row's note, and the whole story lives in the detail pane.
    fn push_thread_rows(&self, rows: &mut Vec<Row>, w: usize, p: usize, e: usize) {
        let proj = &self.tree[w].projects[p];
        let env = &proj.envs[e];
        let agents = env.agents_vec();
        let attached = self.agents_with_live_panes();
        for a in sorted_agents(agents, &attached) {
            let agent = &agents[a];
            let pending = self.ops.get(agent.id.as_str()).copied();
            let status = displayed_status(agent, &attached);
            // The threads this agent carries: what the platform says is
            // running in there, which outlives our connections and can be
            // rejoined by name.
            let live: Vec<(usize, &ConsoleSession)> = match &agent.sessions {
                LoadSessions::Loaded(sessions) => sessions
                    .iter()
                    .enumerate()
                    .filter(|(_, session)| {
                        session.is_interesting() && !self.ending.contains(&session.name)
                    })
                    .collect(),
                _ => Vec::new(),
            };
            // The agent heads its threads: always on the list, so a sleeping
            // or freshly created agent is reachable too — and an emptied one
            // still says what it is once its last session closes.
            let empty = matches!(&agent.sessions, LoadSessions::Loaded(_)) && live.is_empty();
            rows.push(Row {
                depth: 0,
                kind: RowKind::Agent(w, p, e, a),
                label: agent.name.clone(),
                // No status tag: the orb's colour is the status, pending
                // states included.
                note: String::new(),
                status: Some(pending.unwrap_or(status).to_string()),
                expanded: Some(agent.expanded),
                dimmed: false,
            });
            // The agent's orb is green only when its effective status says
            // running — and its sessions never look better than it does: a
            // pane held open onto a sleeping agent is not "connected" in any
            // way that matters.
            let agent_live = pending.unwrap_or(status) == "running";
            for (i, session) in live {
                // Each thread leads with what is happening in it — the latest
                // prompt while the harness works, its last reply once done —
                // falling back to `[S] name` for a session nothing has
                // reported from. `connected` is whether THIS UI is attached;
                // the platform's own `attached` flag counts other clients
                // too, which is why it flickered.
                let connected = self.pane_for(&session.name).is_some();
                let connecting = self.connecting.contains(&session.name);
                let reported = session.snapshot.as_ref().map(|s| s.state.as_str());
                rows.push(Row {
                    depth: 1,
                    kind: RowKind::Session(w, p, e, a, i),
                    label: session.thread_label(),
                    note: String::new(),
                    status: if connected && agent_live {
                        Some("connected".to_string())
                    } else if connecting && agent_live {
                        Some("connecting".to_string())
                    } else if agent_live
                        && let Some(state @ ("working" | "waiting")) = reported
                    {
                        // The harness's own state, when it is worth a glyph:
                        // a spinner while it works, a hollow mark when it
                        // waits on a human.
                        Some(state.to_string())
                    } else {
                        None
                    },
                    expanded: None,
                    dimmed: false,
                });
            }
            // An agent whose listing came back empty says so where its
            // threads would be — otherwise closing the last session leaves a
            // bare name with nothing marking it as an agent.
            if empty {
                rows.push(Row {
                    depth: 1,
                    kind: RowKind::Note(w, p, e),
                    label: "no sessions — n starts one".into(),
                    note: String::new(),
                    status: None,
                    expanded: None,
                    dimmed: true,
                });
            }
            // A list still on its way — or one that failed to come — says so
            // where the rows would be: silence here is indistinguishable
            // from "no sessions", which reads as work lost.
            match &agent.sessions {
                LoadSessions::Loading => rows.push(Row {
                    depth: 1,
                    kind: RowKind::Note(w, p, e),
                    label: "loading sessions…".into(),
                    note: String::new(),
                    status: None,
                    expanded: None,
                    dimmed: true,
                }),
                LoadSessions::Failed(err) => rows.push(Row {
                    depth: 1,
                    kind: RowKind::Note(w, p, e),
                    label: format!("couldn't load sessions — retrying ({err})"),
                    note: String::new(),
                    status: None,
                    expanded: None,
                    dimmed: true,
                }),
                _ => {}
            }
        }
    }

    /// The projects with agent-less environments, folded under one heading at
    /// the bottom.
    ///
    /// This is the browse-to-create surface the groups can't be: selecting a
    /// project or environment here and pressing `n` is how the first agent
    /// gets somewhere new. A project appears whenever it has an environment
    /// that is not a group above — usually because it has no agents at all,
    /// but also when its staging sits empty next to an occupied production;
    /// every environment stays reachable for `n`, `t`, and `r`. Workspaces
    /// appear as a level only when there is more than one to tell apart.
    fn push_project_tail(&self, rows: &mut Vec<Row>, groups: &[(usize, usize, usize)]) {
        let default_project = self.default_project.as_deref();
        let tails: Vec<(usize, Vec<usize>)> = self
            .tree
            .iter()
            .enumerate()
            .map(|(w, ws)| {
                let order = sorted_projects(ws, default_project)
                    .into_iter()
                    .filter(|&p| {
                        ws.projects[p]
                            .envs
                            .iter()
                            .any(|env| env.agents_vec().is_empty())
                    })
                    .collect();
                (w, order)
            })
            .collect();
        let total: usize = tails.iter().map(|(_, order)| order.len()).sum();
        if total == 0 {
            return;
        }
        if !groups.is_empty() {
            rows.push(separator_row());
        }
        let open = self.others_expanded.unwrap_or(groups.is_empty());
        rows.push(Row {
            depth: 0,
            kind: RowKind::OtherProjects,
            // "Other" is relative to the groups; without any there is nothing
            // for these to be other than.
            label: if groups.is_empty() {
                "projects".into()
            } else {
                "other projects".into()
            },
            note: format!("({total})"),
            status: None,
            expanded: Some(open),
            dimmed: false,
        });
        if !open {
            return;
        }
        let multi_workspace = self.tree.len() > 1;
        for (w, order) in tails {
            if order.is_empty() {
                continue;
            }
            let ws = &self.tree[w];
            let base = if multi_workspace {
                rows.push(Row {
                    depth: 1,
                    kind: RowKind::Workspace(w),
                    label: ws.name.clone(),
                    note: "workspace".into(),
                    status: None,
                    expanded: Some(ws.expanded),
                    dimmed: false,
                });
                if !ws.expanded {
                    continue;
                }
                2
            } else {
                1
            };
            for p in order {
                let proj = &ws.projects[p];
                let is_default = Some(proj.id.as_str()) == default_project;
                rows.push(Row {
                    depth: base,
                    kind: RowKind::Project(w, p),
                    label: proj.name.clone(),
                    note: if is_default {
                        "(default)".to_string()
                    } else {
                        String::new()
                    },
                    status: None,
                    expanded: Some(proj.expanded),
                    // Everything here is empty, so everything recedes — except
                    // the default, which is where agents go and has to be
                    // findable even while empty.
                    dimmed: !is_default,
                });
                if !proj.expanded {
                    continue;
                }
                for (e, env) in proj.envs.iter().enumerate() {
                    // An environment with agents is a group above; repeating
                    // it down here would be the same thing twice.
                    if !env.agents_vec().is_empty() {
                        continue;
                    }
                    let note = match &env.agents {
                        Load::Loading => "…".into(),
                        Load::Failed(_) => "!".into(),
                        // Everything left here is empty, and a marker against
                        // every empty environment would be noise.
                        _ => String::new(),
                    };
                    rows.push(Row {
                        depth: base + 1,
                        kind: RowKind::Environment(w, p, e),
                        label: env.name.clone(),
                        note,
                        status: None,
                        expanded: Some(env.expanded),
                        dimmed: false,
                    });
                    if !env.expanded {
                        continue;
                    }
                    match &env.agents {
                        Load::Loaded(agents) if agents.is_empty() => {
                            rows.push(note_row(w, p, e, base + 2, "no agents here"));
                        }
                        Load::Loading => rows.push(note_row(w, p, e, base + 2, "loading…")),
                        Load::Failed(err) => {
                            rows.push(note_row(
                                w,
                                p,
                                e,
                                base + 2,
                                &format!("couldn't load: {err}"),
                            ));
                        }
                        // Loaded and non-empty lives in the groups; nothing to
                        // add under it here.
                        Load::Loaded(_) | Load::NotLoaded => {}
                    }
                }
            }
        }
    }

    /// Every environment with agents, as `(workspace, project, environment)`
    /// paths in display order: the groups with something running first, then
    /// the default project, then by name.
    ///
    /// Indices rather than references, so `RowKind` keeps pointing at the same
    /// environment as statuses change and the order shifts under it — the
    /// cursor is restored by identity, not by position.
    fn agent_groups(&self) -> Vec<(usize, usize, usize)> {
        let mut groups = Vec::new();
        for (w, ws) in self.tree.iter().enumerate() {
            for (p, proj) in ws.projects.iter().enumerate() {
                for (e, env) in proj.envs.iter().enumerate() {
                    if !env.agents_vec().is_empty() {
                        groups.push((w, p, e));
                    }
                }
            }
        }
        let default = self.default_project.as_deref();
        let running = |(w, p, e): &(usize, usize, usize)| {
            self.tree[*w].projects[*p].envs[*e]
                .agents_vec()
                .iter()
                .any(|agent| agent.status == "running")
        };
        groups.sort_by(|a, b| {
            let (pa, pb) = (&self.tree[a.0].projects[a.1], &self.tree[b.0].projects[b.1]);
            let is_default = |p: &ProjectNode| Some(p.id.as_str()) == default;
            running(b)
                .cmp(&running(a))
                .then_with(|| is_default(pb).cmp(&is_default(pa)))
                .then_with(|| pa.name.to_lowercase().cmp(&pb.name.to_lowercase()))
                .then_with(|| a.2.cmp(&b.2))
        });
        groups
    }

    pub fn selected_row(&self) -> Option<Row> {
        self.rows().into_iter().nth(self.cursor)
    }

    /// The environment a row belongs to, whatever kind of row it is — so `t`
    /// and `n` work with the cursor on an agent, not just on its environment.
    fn env_of(&self, kind: RowKind) -> Option<(usize, usize, usize)> {
        match kind {
            RowKind::Environment(w, p, e)
            | RowKind::Agent(w, p, e, _)
            | RowKind::Session(w, p, e, _, _)
            | RowKind::Note(w, p, e) => Some((w, p, e)),
            _ => None,
        }
    }

    fn target_at(&self, path: (usize, usize, usize)) -> Option<Target> {
        let (w, p, e) = path;
        let proj = self.tree.get(w)?.projects.get(p)?;
        let env = proj.envs.get(e)?;
        Some(Target {
            project_id: proj.id.clone(),
            project_name: proj.name.clone(),
            environment_id: env.id.clone(),
            environment_name: env.name.clone(),
        })
    }

    fn clamp_cursor(&mut self) {
        let len = self.rows().len();
        if len == 0 {
            self.cursor = 0;
        } else if self.cursor >= len {
            self.cursor = len - 1;
        }
    }

    /// Move by `delta`, skipping non-selectable note rows, and bring the pane
    /// along when the new row is a session.
    fn move_cursor(&mut self, delta: isize) -> Option<Effect> {
        self.move_cursor_inner(delta);
        // Arriving on the launcher puts the keyboard in its prompt; only Esc
        // there sets it down again.
        if self.cursor == 0 {
            self.prompt_focused = true;
        }
        self.sync_active_to_cursor();
        self.auto_expand_agent()
    }

    /// Open the agent the cursor just landed on, so its sessions are visible
    /// without a second keypress.
    ///
    /// Not when we already know it has none: expanding then would replace the
    /// sessions with a "no sessions" line, which is noise for the common case
    /// of walking past an idle agent.
    fn auto_expand_agent(&mut self) -> Option<Effect> {
        let row = self.selected_row()?;
        let RowKind::Agent(w, p, e, a) = row.kind else {
            return None;
        };
        let Load::Loaded(agents) = &self.tree.get(w)?.projects.get(p)?.envs.get(e)?.agents else {
            return None;
        };
        let agent = agents.get(a)?;
        if agent.expanded {
            return None;
        }
        match &agent.sessions {
            LoadSessions::Loaded(sessions)
                if sessions.iter().any(ConsoleSession::is_interesting) =>
            {
                self.set_agent_expanded((w, p, e, a), true)
            }
            LoadSessions::NotLoaded => self.set_agent_expanded((w, p, e, a), true),
            _ => None,
        }
    }

    fn move_cursor_inner(&mut self, delta: isize) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let mut i = self.cursor as isize;
        loop {
            i += delta;
            if i < 0 || i as usize >= rows.len() {
                return; // ran off the end: leave the cursor where it was
            }
            if rows[i as usize].selectable() {
                self.cursor = i as usize;
                return;
            }
        }
    }

    /// Record a finished agent fetch. Ignores paths that no longer exist, so a
    /// response arriving after the tree changed can't panic or mis-file.
    pub fn agents_loaded(
        &mut self,
        path: (usize, usize, usize),
        result: Result<Vec<Agent>, String>,
    ) {
        let (w, p, e) = path;
        // A load can change the order — a project that gains its first agent
        // moves up — so the cursor is put back by row identity rather than
        // left on whatever index it was.
        let anchor = self.selected_row().map(|row| row.kind);
        let mut refresh_failed = None;
        let mut answered = None;
        if let Some(env) = self
            .tree
            .get_mut(w)
            .and_then(|ws| ws.projects.get_mut(p))
            .and_then(|proj| proj.envs.get_mut(e))
        {
            let previous = std::mem::replace(&mut env.agents, Load::NotLoaded);
            env.agents = match (result, previous) {
                // Merged, not replaced: a fetch describes the platform's
                // agents, and whether a row is open and what is running on it
                // is ours. See [`merge_agents`].
                (Ok(agents), Load::Loaded(previous)) => {
                    answered = Some(env.id.clone());
                    Load::Loaded(merge_agents(previous, agents))
                }
                (Ok(agents), _) => {
                    answered = Some(env.id.clone());
                    Load::Loaded(agents)
                }
                // A refresh that fails must not take the agents with it:
                // stale beats gone, and the status line carries the reason.
                (Err(err), Load::Loaded(agents)) => {
                    refresh_failed = Some(err);
                    Load::Loaded(agents)
                }
                (Err(err), _) => Load::Failed(err),
            };
        }
        // When this environment last heard from the platform, so an
        // account-wide snapshot taken before it cannot overwrite it. See
        // [`Self::my_agents_loaded`].
        if let Some(id) = answered {
            self.answered_at.insert(id, std::time::Instant::now());
        }
        if let Some(err) = refresh_failed {
            self.toast_error(format!("Couldn't refresh: {err}"));
        }
        self.collapse_if_empty(w, p);
        self.settle_watched_agents();
        self.restore_cursor(anchor);
        self.select_pending();
        // A just-launched agent arrives here before its sessions can be
        // asked about; the pane already attached to it is proof enough.
        self.adopt_pane_sessions();
    }

    /// The whole account's agents arrived in one `myCloudAgents` request.
    ///
    /// Every unanswered environment becomes loaded: one absent from the
    /// response holds no agents of the caller's, and saying so is what lets
    /// the tree show counts everywhere without a request per environment. An
    /// environment that already had a list gets the snapshot instead — that is
    /// what makes this a refresh rather than a first fill, and it is the only
    /// thing that can tell the tree an agent was created or deleted somewhere
    /// else. (Skipping environments that already had a list is why `shift+r`
    /// used to report "already loaded" and change nothing.)
    ///
    /// One rule keeps that from undoing fresher news: a snapshot may not
    /// overwrite an answer that arrived *after* it was asked for. `asked_at` is
    /// when this request went out, and [`Self::answered_at`] is when each
    /// environment last heard from the platform. Without the comparison, a
    /// refresh already in flight when you delete an agent would put the row
    /// back — the snapshot was taken while the agent still existed — and it
    /// would stay back until the next refresh. Environments still `Loading` are
    /// skipped for the same reason, one step earlier: their reply has not landed
    /// to be compared, and it is newer than this.
    pub fn my_agents_loaded(&mut self, agents: Vec<(String, Agent)>, asked_at: std::time::Instant) {
        let anchor = self.selected_row().map(|row| row.kind);
        let mut by_env: HashMap<String, Vec<Agent>> = HashMap::new();
        for (environment_id, agent) in agents {
            by_env.entry(environment_id).or_default().push(agent);
        }
        let mut answered = Vec::new();
        for ws in &mut self.tree {
            for project in &mut ws.projects {
                for env in &mut project.envs {
                    if self
                        .answered_at
                        .get(&env.id)
                        .is_some_and(|at| *at > asked_at)
                    {
                        continue;
                    }
                    let fresh = match &env.agents {
                        Load::NotLoaded | Load::Failed(_) => by_env.remove(&env.id),
                        Load::Loaded(previous) => Some(merge_agents(
                            previous.clone(),
                            by_env.remove(&env.id).unwrap_or_default(),
                        )),
                        Load::Loading => continue,
                    };
                    env.agents = Load::Loaded(fresh.unwrap_or_default());
                    answered.push(env.id.clone());
                }
            }
        }
        let now = std::time::Instant::now();
        for id in answered {
            self.answered_at.insert(id, now);
        }
        self.settle_watched_agents();
        self.restore_cursor(anchor);
        self.select_pending();
        self.adopt_pane_sessions();
    }

    /// Fold up a project whose last agent has gone.
    ///
    /// After a delete the tree would otherwise sit open on an empty
    /// environment under an empty project, holding space for nothing. Only when
    /// every environment has answered — collapsing one that is still loading
    /// would hide agents that are about to appear.
    fn collapse_if_empty(&mut self, w: usize, p: usize) {
        let Some(project) = self.tree.get_mut(w).and_then(|ws| ws.projects.get_mut(p)) else {
            return;
        };
        let all_answered = project
            .envs
            .iter()
            .all(|env| matches!(env.agents, Load::Loaded(_)));
        let empty = project.envs.iter().all(|env| match &env.agents {
            Load::Loaded(agents) => agents.is_empty(),
            _ => true,
        });
        if !all_answered || !empty {
            return;
        }
        project.expanded = false;
        for env in project.envs.iter_mut() {
            env.expanded = false;
        }
    }

    /// Put the cursor back on the row it was on, wherever that row now sits.
    ///
    /// A row can be gone entirely — a load can fold the projects tail the
    /// cursor was in — and then the nearest selectable row is the best that
    /// can be done.
    fn restore_cursor(&mut self, anchor: Option<RowKind>) {
        if let Some(kind) = anchor {
            let rows = self.rows();
            if let Some(index) = rows.iter().position(|row| row.kind == kind) {
                self.cursor = index;
            }
        }
        self.clamp_cursor();
        if self.selected_row().is_some_and(|row| !row.selectable()) {
            self.move_cursor_inner(1);
        }
        if self.selected_row().is_some_and(|row| !row.selectable()) {
            self.move_cursor_inner(-1);
        }
    }

    /// Expand or collapse an agent, fetching its sessions the first time.
    fn set_agent_expanded(
        &mut self,
        path: (usize, usize, usize, usize),
        open: bool,
    ) -> Option<Effect> {
        let (w, p, e, a) = path;
        let env = self.tree.get_mut(w)?.projects.get_mut(p)?.envs.get_mut(e)?;
        let environment_id = env.id.clone();
        let Load::Loaded(agents) = &mut env.agents else {
            return None;
        };
        let agent = agents.get_mut(a)?;
        agent.expanded = open;
        if !open {
            return None;
        }
        // Always refetch on expand: sessions come and go while you are looking
        // at something else, and a stale list is worse than a brief spinner.
        agent.sessions = LoadSessions::Loading;
        Some(Effect::LoadSessions {
            agent_id: agent.id.clone(),
            environment_id,
            path,
        })
    }

    /// Record a finished session fetch.
    pub fn sessions_loaded(
        &mut self,
        path: (usize, usize, usize, usize),
        agent_id: &str,
        result: Result<Vec<ConsoleSession>, String>,
    ) {
        // Whatever this reply says, its agent's fast poll is no longer in
        // flight (see `threads_to_poll`).
        self.thread_polls.remove(agent_id);
        let (w, p, e, _) = path;
        // Resolved by id rather than trusting the index the request went out
        // with: the environment can be refetched while sessions are in
        // flight, and a new agent shifting the list would otherwise attach
        // these sessions to whoever now sits at that index.
        let a = match self
            .tree
            .get(w)
            .and_then(|ws| ws.projects.get(p))
            .and_then(|proj| proj.envs.get(e))
            .map(|env| &env.agents)
        {
            Some(Load::Loaded(agents)) => {
                match agents.iter().position(|agent| agent.id == agent_id) {
                    Some(a) => a,
                    // The agent is gone (deleted, or the refetch dropped it);
                    // its sessions have nowhere to belong.
                    None => return,
                }
            }
            _ => return,
        };
        let mut refresh_failed = None;
        if let Some(Load::Loaded(agents)) = self
            .tree
            .get_mut(w)
            .and_then(|ws| ws.projects.get_mut(p))
            .and_then(|proj| proj.envs.get_mut(e))
            .map(|env| &mut env.agents)
            && let Some(agent) = agents.get_mut(a)
        {
            let previous = std::mem::replace(&mut agent.sessions, LoadSessions::NotLoaded);
            agent.sessions = match (result, previous) {
                (Ok(sessions), _) => LoadSessions::Loaded(sessions),
                // The same rule the agent list follows: a refresh that fails
                // keeps what it had rather than replacing a good list with an
                // error. The reason goes to the status line, not a toast — a
                // background refresh must not raise one per tick while the
                // network is out.
                (Err(err), LoadSessions::Loaded(sessions)) => {
                    refresh_failed = Some(err);
                    LoadSessions::Loaded(sessions)
                }
                (Err(err), _) => LoadSessions::Failed(err),
            };
        }
        if let Some(err) = refresh_failed {
            self.status = format!("Couldn't refresh sessions: {err}");
        }
        // Anything we hid that the agent no longer reports has actually gone;
        // stop tracking it. One that is still listed stays hidden until it is.
        if let Some(LoadSessions::Loaded(sessions)) = self
            .tree
            .get(w)
            .and_then(|ws| ws.projects.get(p))
            .and_then(|proj| proj.envs.get(e))
            .and_then(|env| match &env.agents {
                Load::Loaded(agents) => agents.get(a).map(|agent| &agent.sessions),
                _ => None,
            })
        {
            let live: std::collections::HashSet<&str> = sessions
                .iter()
                .filter(|s| s.is_interesting())
                .map(|s| s.name.as_str())
                .collect();
            self.ending.retain(|name| live.contains(name.as_str()));
        }
        self.clamp_cursor();
        // The platform's list may still be missing sessions we are attached
        // to (the relay registers them a moment after ssh connects); fold
        // those back in rather than letting the reply hide them.
        self.adopt_pane_sessions();
    }

    /// Expand an environment, loading its agents the first time.
    fn expand_env(&mut self, path: (usize, usize, usize)) -> Option<Effect> {
        let (w, p, e) = path;
        let env = self.tree.get_mut(w)?.projects.get_mut(p)?.envs.get_mut(e)?;
        env.expanded = true;
        if env.agents == Load::NotLoaded {
            env.agents = Load::Loading;
            return Some(Effect::LoadAgents {
                environment_id: env.id.clone(),
                path,
            });
        }
        None
    }

    /// The agent the tree cursor is pointing at, directly or via one of its
    /// sessions. A prompt submitted while an agent is selected belongs to that
    /// agent — the alternative is silently starting work somewhere else.
    fn launch(&self, agent_id: Option<String>, force_new: bool) -> Option<Effect> {
        // A shell session takes no prompt; a draft left over from cycling
        // through the agents must not ride along.
        let draft = (!self.shell_selected() && !self.prompt.trim().is_empty())
            .then(|| self.prompt.trim().to_string());
        self.launch_prompted(agent_id, force_new, draft)
    }

    /// [`Self::launch`], with the prompt supplied by the caller instead of
    /// read from the launcher's box — what the ⌥p composer sends.
    fn launch_prompted(
        &self,
        agent_id: Option<String>,
        force_new: bool,
        prompt: Option<String>,
    ) -> Option<Effect> {
        let target = self.target.clone()?;
        Some(Effect::Launch(LaunchRequest {
            project_id: target.project_id.clone(),
            environment_id: target.environment_id.clone(),
            agent_id,
            session_name: None,
            force_new,
            new_session: false,
            harness: self.harness_name().to_string(),
            prompt,
            label: target.label(),
            base: Default::default(),
        }))
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Effect> {
        // The SSH gate owns the keyboard until its question is answered.
        // Anything other than yes cancels: a mistyped key must never register
        // a credential on the account.
        if let Some(gate) = self.ssh_gate.take() {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => Some(Effect::RegisterSshKey {
                    offer: gate.offer,
                    then: gate.then,
                }),
                _ => {
                    if gate.then.is_some() {
                        self.toast_error("Cancelled — connecting needs a registered SSH key");
                    }
                    None
                }
            };
        }

        // A focused session owns the keyboard: Ctrl-C must interrupt the agent,
        // Esc must reach its editor, and ⌥/^ chords belong to whatever is
        // running in there. Only one chord is reserved, and it is one no agent
        // binds: ^o hands focus back to the tree.
        //
        // Only while the session is the frontmost thing, though. A card
        // floated over it — the ⌥n picker, the ⌥p composer — is what the
        // keyboard is for while it is up, and focus stays on the session
        // underneath so closing the card returns to typing in it.
        if self.focus == ManageFocus::Session && self.screen == Screen::Manage {
            // Four ways out, because terminals disagree about what they will
            // report. `^]` is the classic escape chord and works everywhere,
            // and `^o` stays for anyone who learned it. Both modified Escapes
            // are taken because only one of them can be typed on a stock macOS
            // terminal: Option is a compose key there unless it is mapped to
            // Meta, and every other ⌥ chord survives that by arriving as the
            // character it composes to (⌥f as `ƒ`, ⌥[ as a curly quote — see
            // `alt_chord`). Escape composes to nothing, so ⌥esc arrives as a
            // bare Escape that belongs to the agent, with no modifier left to
            // tell the two apart. Shift is never composed, and the kitty
            // protocol reports a shifted Escape as `CSI 27;2u`, so ⇧esc is the
            // one that survives a terminal nobody has configured.
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let modified_esc = key.code == KeyCode::Esc
                && key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
            let ctrl_bracket = ctrl && key.code == KeyCode::Char(']');
            let ctrl_o = ctrl && matches!(key.code, KeyCode::Char('o') | KeyCode::Char('O'));
            if modified_esc || ctrl_bracket || ctrl_o {
                self.focus = ManageFocus::Tree;
                // Releasing means "show me the tree" — a maximized pane has
                // it folded away, so give it back rather than handing focus
                // to something invisible.
                self.maximized = false;
                return None;
            }
            // A few chords are taken from the agent, all because the moment
            // you want them is while you are using it: ⌥f for room, ⌥⇧] and
            // ⌥⇧[ (with their unshifted aliases) to move between panes, ⌥n
            // and ⌥p to start more work — the thought "this needs its own
            // session" arrives while reading one, and going out to the tree
            // first to act on it is the friction they exist to remove. ⌥r
            // joins them for the same reason: a refresh that only worked with
            // the tree focused would be a chord that silently does nothing
            // half the time you press it. The costs, all in a shell: ⌥f is
            // Meta-f (forward-word), ⌥n and ⌥p are Meta-n / Meta-p (the
            // non-incremental history searches, which few people bind and
            // both harnesses ignore), and ⌥r is Meta-r (revert-line).
            // Readline leaves Meta-] and Meta-[ unbound, bash binds Meta-{
            // only to the rarely-reached complete-into-braces, and `^]`
            // (character-search) is untouched because only the Meta forms are
            // claimed. Nothing else is intercepted — ⌥s still reaches the
            // agent from here.
            if let Some(chord) = alt_chord(&key)
                && matches!(chord, 'f' | 'n' | 'p' | 'r' | ']' | '[')
            {
                return self.alt_action(chord);
            }
            // Scrollback, before the agent sees the key. Shifted so an
            // unshifted PageUp still belongs to whatever is running.
            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
            let scroll = match key.code {
                KeyCode::PageUp if shift => Some(true),
                KeyCode::PageDown if shift => Some(false),
                _ => None,
            };
            if let Some(i) = self.active {
                // A pane whose connection is gone answers with recovery
                // instead of typing: bytes sent to a dead pty vanish, which
                // used to make "session ended" a wall with no door. Scrollback
                // stays live — reading what the connection said on its way
                // out is half the point of keeping the screen.
                let dead = self
                    .sessions
                    .get(i)
                    .is_some_and(|s| s.ended() || s.stalled());
                if dead && scroll.is_none() {
                    return self.dead_pane_key(i, key);
                }
                // Every bare Escape belongs to the harness — including two in
                // a row. There used to be a double-tap release here, but the
                // first tap always went through, and in Claude Code a single
                // Esc cancels the running turn: the release gesture cost the
                // user their work on its way out. ⌥⇧esc (and ⇧esc, ^], ^o)
                // release without sending anything.
                if let Some(session) = self.sessions.get_mut(i) {
                    match scroll {
                        // No pointer for a keyboard scroll; report it over the
                        // middle of the pane.
                        Some(up) => session.scroll(up, 10, (1, 1)),
                        None => session.send_key(key),
                    }
                }
            }
            return None;
        }

        // Ctrl-C always quits, on every screen and in every focus.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            return Some(Effect::Quit);
        }
        // Ctrl-T retargets from anywhere. Deliberately not plain `t`: the
        // prompt has focus by default, where `t` is a letter someone is typing.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('t') | KeyCode::Char('T'))
        {
            self.start_target_pick();
            return None;
        }
        // Alt-chords work from every screen and both focuses — including the
        // prompt, where a bare letter is text someone is typing. macOS sends
        // Option+letter as a composed character unless the terminal maps Option
        // to Meta, so the handful that are not dead keys are accepted too.
        if let Some(action) = alt_chord(&key) {
            return self.alt_action(action);
        }
        self.status.clear();
        match self.screen {
            Screen::Setup => self.on_key_wizard(key),
            Screen::Settings => self.on_key_settings(key),
            Screen::TargetPick => self.on_key_target_pick(key),
            Screen::HarnessPick => self.on_key_harness_pick(key),
            Screen::ManagePrompt => self.on_key_manage_prompt(key),
            Screen::Manage => self.on_key_manage(key),
        }
    }

    /// Pasted text — a ⌘v, or a dictation tool like Wispr Flow, which inserts
    /// into a terminal the same way. It goes where typing would go: a focused
    /// session gets it as a paste, a prompt box gets it as text. Anywhere
    /// else it is dropped whole — fed through `on_key` it would spray across
    /// hotkeys, and a stray paste must never trigger an action.
    pub fn on_paste(&mut self, text: String) -> Option<Effect> {
        // The SSH gate is a y/N question; pasted text is not an answer.
        if self.ssh_gate.is_some() {
            return None;
        }
        if self.focus == ManageFocus::Session && self.screen == Screen::Manage {
            if let Some(i) = self.active {
                // A dead pane's keys mean recovery, but a paste is not a
                // command — bytes to a dead pty vanish, so drop it.
                let dead = self
                    .sessions
                    .get(i)
                    .is_some_and(|s| s.ended() || s.stalled());
                if !dead && let Some(session) = self.sessions.get_mut(i) {
                    session.send_paste(&text);
                }
            }
            return None;
        }
        // A pasted newline stays a newline — the prompt boxes hold
        // paragraphs, and ⇧enter types the same character — but it is never
        // taken as Enter: pasting must not launch. Other control characters
        // have no business in a draft at all.
        let text: String = text
            .replace("\r\n", "\n")
            .chars()
            .map(|c| if c == '\r' { '\n' } else { c })
            .filter(|c| *c == '\n' || !c.is_control())
            .collect();
        match self.screen {
            Screen::Manage if self.new_session_selected() && !self.shell_selected() => {
                self.prompt_insert_str(&text);
            }
            Screen::ManagePrompt if !self.shell_selected() => {
                if let Some(draft) = self.manage_prompt.as_mut() {
                    draft.push_str(&text);
                }
            }
            _ => {}
        }
        None
    }

    fn on_key_wizard(&mut self, key: KeyEvent) -> Option<Effect> {
        use super::wizard::Action;
        let wizard = self.wizard.as_mut()?;
        // A step doing work owns the keyboard until it is done.
        if wizard.busy.is_some() {
            return None;
        }
        let action = match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                wizard.up();
                Action::Redraw
            }
            KeyCode::Down | KeyCode::Char('j') => {
                wizard.down();
                Action::Redraw
            }
            KeyCode::Enter => wizard.select(),
            KeyCode::Esc => wizard.back(),
            _ => Action::None,
        };
        // The theme previews as the cursor moves, so the whole screen follows.
        self.theme = self
            .wizard
            .as_ref()
            .map(|w| w.previewed_theme())
            .unwrap_or(self.theme);
        match action {
            Action::None | Action::Redraw => None,
            Action::CreateProject(workspace_id) => Some(Effect::CreateDefaultProject(workspace_id)),
            Action::Cancel => {
                self.end_wizard();
                None
            }
            Action::Finish(outcome) => {
                // There are preferences now, so the next start skips the
                // wizard and the answers move to the ⌥s settings card.
                self.configured = true;
                self.end_wizard();
                Some(Effect::SaveSetup(outcome))
            }
        }
    }

    fn on_key_settings(&mut self, key: KeyEvent) -> Option<Effect> {
        use super::settings::Action;
        let settings = self.settings.as_mut()?;
        // The picker creating a project owns the keyboard until it is done.
        if settings.busy.is_some() {
            return None;
        }
        let action = match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                settings.up();
                Action::Redraw
            }
            KeyCode::Down | KeyCode::Char('j') => {
                settings.down();
                Action::Redraw
            }
            KeyCode::Left | KeyCode::Char('h') => settings.left(),
            KeyCode::Right | KeyCode::Char('l') => settings.right(),
            KeyCode::Enter => settings.select(),
            KeyCode::Esc => settings.back(),
            _ => Action::None,
        };
        // The theme applies as it cycles, so the whole screen follows.
        self.theme = self
            .settings
            .as_ref()
            .map(|s| s.current_theme())
            .unwrap_or(self.theme);
        match action {
            Action::None | Action::Redraw => None,
            Action::Save(outcome) => Some(Effect::SaveSettings(outcome)),
            Action::CreateProject(workspace_id) => Some(Effect::CreateDefaultProject(workspace_id)),
            Action::RunSetup => {
                self.settings = None;
                self.start_wizard(false);
                None
            }
            Action::Close => {
                self.end_settings();
                None
            }
        }
    }

    /// Hand a pointer event to the application, in the pane's own coordinates.
    /// Reports whether it wanted it.
    fn send_pointer(&mut self, kind: session::Pointer, col: u16, row: u16) -> bool {
        let pane = self.panes.session;
        if !pane.contains(col, row) {
            return false;
        }
        let at = (
            col.saturating_sub(pane.x) + 1,
            row.saturating_sub(pane.y) + 1,
        );
        let Some(index) = self.active else {
            return false;
        };
        match self.sessions.get_mut(index) {
            Some(session) => session.pointer(kind, at),
            None => false,
        }
    }

    /// The link under a screen position, in the open session.
    fn url_at(&self, col: u16, row: u16) -> Option<String> {
        let pane = self.panes.session;
        if !pane.contains(col, row) {
            return None;
        }
        self.active_session()?.url_at(row - pane.y, col - pane.x)
    }

    /// Which pane a screen position belongs to, borders included — a click
    /// anywhere on a panel selects it, which is also how the keyboard is taken
    /// back from a focused session.
    fn pane_at(&self, col: u16, row: u16) -> Option<ManageFocus> {
        if self.panes.session_outer.contains(col, row) {
            Some(ManageFocus::Session)
        } else if self.panes.tree_outer.contains(col, row) {
            Some(ManageFocus::Tree)
        } else {
            None
        }
    }

    /// Handle a mouse event, returning any work it implies — expanding a row
    /// can need its children fetched, the same as the keyboard.
    ///
    /// A finished drag arms `pending_copy`; the text itself is lifted out of
    /// the next frame, since that is the only place the pane's contents exist
    /// in screen coordinates.
    #[cfg(test)]
    pub fn on_mouse(&mut self, kind: MouseAction, col: u16, row: u16) -> Option<Effect> {
        self.on_mouse_shifted(kind, col, row, false)
    }

    /// The same, told whether shift was held.
    ///
    /// Shift is the terminal's own convention for "this click is mine, not the
    /// application's" — it is how you select text in a program that has taken
    /// the mouse, and it means the same thing here.
    pub fn on_mouse_shifted(
        &mut self,
        kind: MouseAction,
        col: u16,
        row: u16,
        shift: bool,
    ) -> Option<Effect> {
        if self.screen != Screen::Manage {
            return None;
        }
        match kind {
            // The wheel scrolls whichever pane it is over, without taking
            // focus: looking back through output is not the same as wanting to
            // type into it.
            MouseAction::ScrollUp | MouseAction::ScrollDown => {
                let up = kind == MouseAction::ScrollUp;
                if self.panes.session_outer.contains(col, row)
                    && let Some(index) = self.active
                {
                    // Where the pointer is, in the pane's own coordinates —
                    // a wheel report carries a position, and an application
                    // may well care which pane region it is over.
                    let at = (
                        col.saturating_sub(self.panes.session.x) + 1,
                        row.saturating_sub(self.panes.session.y) + 1,
                    );
                    if let Some(session) = self.sessions.get_mut(index) {
                        session.scroll(up, 3, at);
                    }
                } else if self.panes.tree_outer.contains(col, row) {
                    self.move_cursor(if up { -1 } else { 1 });
                }
                None
            }
            MouseAction::Down => {
                // A click clears the status line, the same as a keypress
                // (see on_key): the message answered the previous gesture,
                // and whatever this click means sets a fresh one — clicking
                // a disconnected session must not leave "not connected"
                // standing once the cursor is on a connected one.
                self.status.clear();
                // The maximized header's tabs: a click switches the pane.
                if self.pane_is_full()
                    && let Some(i) = (0..self.sessions.len().min(MAX_TABS))
                        .find(|&i| self.panes.tabs[i].contains(col, row))
                {
                    self.active = Some(i);
                    self.select_row_for_active();
                    return None;
                }
                // The prompt box is drawn in the session pane, but clicking it
                // means "let me type the task" — the keyboard for that lives
                // with the tree's New Session row, not with a session.
                if self.panes.prompt.contains(col, row) {
                    self.cursor = 0;
                    self.focus = ManageFocus::Tree;
                    self.prompt_focused = true;
                    return None;
                }
                let pane = self.pane_at(col, row)?;
                // An agent with clickable output — "click here to copy", a menu
                // you can point at — turns mouse reporting on and waits for the
                // events. Forward them, on three conditions: the pane already
                // has the keyboard, so the click that focuses it is still ours;
                // shift is not held, which is the terminal's own "this one is
                // mine"; and there is no plain link under the pointer, because
                // opening that is more useful than anything the app will do
                // with the click.
                if pane == ManageFocus::Session
                    && self.focus == ManageFocus::Session
                    && !shift
                    && self.url_at(col, row).is_none()
                    && self.send_pointer(session::Pointer::Press, col, row)
                {
                    self.pointer_to_app = true;
                    self.selection = None;
                    return None;
                }
                // Clicking the session panel means "let me type here". A tree
                // click lands on Tree first; `click_tree_row` hands the
                // keyboard on to the session when the row clicked is one with
                // an open pane. With no session open the
                // right pane is the launcher (or an empty detail card):
                // focusing it would send every key into a pane nothing is
                // reading, so the keyboard stays where the prompt reads it —
                // a drag there is still a selection.
                let double = self.is_double_click(col, row);
                if pane != ManageFocus::Session || self.active.is_some() {
                    self.focus = pane;
                }
                let effect = (pane == ManageFocus::Tree)
                    .then(|| self.click_tree_row(row, double))
                    .flatten();
                self.selection = Some(Selection {
                    pane,
                    anchor: (col, row),
                    cursor: (col, row),
                });
                effect
            }
            MouseAction::Drag => {
                if self.pointer_to_app {
                    self.send_pointer(session::Pointer::Drag, col, row);
                    return None;
                }
                let selection = self.selection.as_mut()?;
                // A drag stays in the pane it started in; leaving the pane
                // clamps rather than selecting across the divider.
                let pane = selection.pane;
                let bounds = match pane {
                    ManageFocus::Tree => self.panes.tree,
                    ManageFocus::Session => self.panes.session,
                };
                let col = col.clamp(bounds.x, bounds.x + bounds.w.saturating_sub(1));
                let row = row.clamp(bounds.y, bounds.y + bounds.h.saturating_sub(1));
                selection.cursor = (col, row);
                None
            }
            MouseAction::Up => {
                if self.pointer_to_app {
                    self.pointer_to_app = false;
                    self.send_pointer(session::Pointer::Release, col, row);
                    return None;
                }
                let click = self.selection.filter(|selection| selection.is_empty());
                match self.selection {
                    Some(selection) if !selection.is_empty() => {
                        self.pending_copy = Some(selection);
                    }
                    // A plain click is not a selection; drop it so the
                    // highlight does not linger over a single cell.
                    _ => self.selection = None,
                }
                // A click that went nowhere, on a link, opens it. On release
                // rather than on press, so that dragging *from* a link still
                // selects it — copying a URL and opening it are both things
                // people do, and the pointer moving is what tells them apart.
                let (col, row) = click?.anchor;
                let url = self.url_at(col, row)?;
                Some(Effect::OpenUrl(url))
            }
        }
    }

    /// Two clicks on the same row inside the double-click window.
    fn is_double_click(&mut self, col: u16, row: u16) -> bool {
        const WINDOW: std::time::Duration = std::time::Duration::from_millis(400);
        let now = std::time::Instant::now();
        let double = self
            .last_click
            .is_some_and(|(at, when)| at == (col, row) && now.duration_since(when) < WINDOW);
        self.last_click = Some(((col, row), now));
        double
    }

    /// Move the tree cursor to the row that was clicked.
    ///
    /// Clicking a session row with an open pane shows it AND hands the keyboard
    /// over: the click says "that one", and typing next should go into the
    /// session — leaving the keys in the tree turned the first keystroke into
    /// a shortcut instead. Agent and folder rows keep the keyboard in the tree
    /// (there is nothing on the right to type into yet); a double click on a
    /// disconnected row connects, the same as enter.
    fn click_tree_row(&mut self, row: u16, double: bool) -> Option<Effect> {
        self.click_tree_row_inner(row);
        self.sync_active_to_cursor();
        let clicked = self.selected_row()?;
        // Clicking the launcher row is clicking the prompt.
        if matches!(clicked.kind, RowKind::NewSession) {
            self.prompt_focused = true;
            return None;
        }
        // A single click on an agent thread selects it (and asks for its
        // sessions, the same as arriving with the keyboard); a double click
        // connects, the same as enter.
        if let RowKind::Agent(w, p, e, a) = clicked.kind {
            if double {
                return self.connect_agent_row(w, p, e, a);
            }
            return self.auto_expand_agent();
        }
        // Anything with children toggles: clicking a folder to open it is the
        // one gesture every tree has.
        if let Some(expanded) = clicked.expanded {
            return self.set_expanded(clicked.kind, !expanded);
        }
        if let RowKind::Session(w, p, e, a, i) = clicked.kind {
            match self
                .console_session(w, p, e, a, i)
                .map(|s| s.name.clone())
                .and_then(|name| self.pane_for(&name))
            {
                // One click selects the pane and hands it the keyboard: the
                // next thing typed is meant for the session. One click only
                // selects — with the sidebar listing threads, most rows are
                // connected sessions, and a single click that handed the
                // keyboard over made the tree impossible to browse from a
                // focused pane. A double click (or enter) steps in.
                Some(index) => {
                    self.active = Some(index);
                    self.focus = if double {
                        ManageFocus::Session
                    } else {
                        ManageFocus::Tree
                    };
                }
                None if double => return self.reattach_row(clicked.kind),
                // No pane to focus: say how to get one, and re-ask the
                // platform while the hint is up — a row the agent no longer
                // reports validates away instead of inviting a reattach to
                // nothing.
                None => {
                    self.status = "Double-click (or enter) to connect".into();
                    let id = self.tree[w].projects[p].envs[e]
                        .agents_vec()
                        .get(a)?
                        .id
                        .clone();
                    return self.refresh_agent_sessions(&id);
                }
            }
        }
        None
    }

    /// Connect to an agent thread: enter on its row, or a double click.
    ///
    /// Connecting also retargets: the place you just opened is almost
    /// certainly where the next prompt should go.
    fn connect_agent_row(&mut self, w: usize, p: usize, e: usize, a: usize) -> Option<Effect> {
        let id = self.tree[w].projects[p].envs[e]
            .agents_vec()
            .get(a)?
            .id
            .clone();
        self.target = self.target_at((w, p, e));
        // Already open: show that session rather than starting a second ssh
        // to the same agent, which would leave two panes fighting over one
        // terminal.
        if self.activate_session(&id) {
            self.status = "Switched to the open session".into();
            return None;
        }
        // A drafted prompt is new work and gets a session of its own. A plain
        // connect reattaches to what is already running: launching here made
        // a NEW durable session each time, which for a harness like
        // `railway-agent-tui` is another window onto the same conversation
        // under yet another name.
        if self.shell_selected() || self.prompt.trim().is_empty() {
            if let Some(i) = self.first_live_session(w, p, e, a) {
                return self.reattach_row(RowKind::Session(w, p, e, a, i));
            }
        }
        self.launch(Some(id), false)
    }

    /// The first still-running session on an agent, by index — the one a
    /// plain "connect" should land in.
    fn first_live_session(&self, w: usize, p: usize, e: usize, a: usize) -> Option<usize> {
        let Load::Loaded(agents) = &self.tree.get(w)?.projects.get(p)?.envs.get(e)?.agents else {
            return None;
        };
        let LoadSessions::Loaded(sessions) = &agents.get(a)?.sessions else {
            return None;
        };
        sessions
            .iter()
            .position(|session| session.is_interesting() && !self.ending.contains(&session.name))
    }

    fn click_tree_row_inner(&mut self, row: u16) {
        let offset = row.saturating_sub(self.panes.tree.y) as usize;
        let rows = self.rows();
        // The list scrolls, so the clicked line is an offset from whatever is
        // at the top — which ratatui decides. Approximate with the same
        // window the widget would have chosen.
        let first = self.tree_scroll_offset(rows.len());
        if let Some(index) = first.checked_add(offset)
            && index < rows.len()
            && rows[index].selectable()
        {
            self.cursor = index;
        }
    }

    /// The first visible tree row, mirroring ratatui's scroll behaviour.
    fn tree_scroll_offset(&self, total: usize) -> usize {
        let height = self.panes.tree.h as usize;
        if height == 0 || total <= height {
            return 0;
        }
        self.cursor.saturating_sub(height - 1).min(total - height)
    }

    /// Begin showing the loading screen for a request. Takes the prompt with
    /// it: the task has been handed over, so the box should be empty when you
    /// come back to write the next one.
    pub fn start_loading(&mut self, req: &LaunchRequest) {
        self.clear_prompt();
        // Straight to Manage: the wait belongs in the pane the session will
        // appear in, with the tree beside it.
        self.screen = Screen::Manage;
        self.loading = Loading {
            target: req.label.clone(),
            harness: req.harness.clone(),
            active: true,
            prompt: req.prompt.clone(),
            steps: Vec::new(),
            tick: 0,
        };
    }

    /// The launch pipeline gave up. Land back in Manage with the reason, so a
    /// missing sign-in or a disabled feature is fixable without losing the tree.
    ///
    /// The reason rides an error toast over the pane rather than a header
    /// annotation: a failure squeezed in beside the wordmark reads as
    /// furniture, and this one usually carries the fix.
    pub fn launch_failed(&mut self, err: String) {
        self.loading.active = false;
        self.screen = Screen::Manage;
        let flat = err.split_whitespace().collect::<Vec<_>>().join(" ");
        self.toast_error(format!("Launch failed: {flat}"));
    }

    /// Advance the loading animation.
    pub fn tick(&mut self) {
        self.loading.tick = self.loading.tick.wrapping_add(1);
    }

    /// Record a step from the launch pipeline.
    pub fn loading_step(&mut self, text: String) {
        // Consecutive duplicates read as a stall rather than progress.
        if self.loading.steps.last() == Some(&text) {
            return;
        }
        self.loading.steps.push(text);
    }

    pub fn active_session(&self) -> Option<&super::session::Session> {
        self.active.and_then(|i| self.sessions.get(i))
    }

    /// Show the pane belonging to the session row under the cursor, if it has
    /// one. Called after every cursor move: walking the tree is how sessions
    /// are switched now, so the pane follows the highlight.
    fn sync_active_to_cursor(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let RowKind::Session(w, p, e, a, i) = row.kind else {
            return;
        };
        let Some(name) = self
            .console_session(w, p, e, a, i)
            .map(|session| session.name.clone())
        else {
            return;
        };
        if let Some(index) = self.sessions.iter().position(|s| s.durable_name == name) {
            self.active = Some(index);
        }
    }

    /// The console session a row points at.
    pub fn console_session(
        &self,
        w: usize,
        p: usize,
        e: usize,
        a: usize,
        i: usize,
    ) -> Option<&ConsoleSession> {
        let Load::Loaded(agents) = &self.tree.get(w)?.projects.get(p)?.envs.get(e)?.agents else {
            return None;
        };
        let LoadSessions::Loaded(sessions) = &agents.get(a)?.sessions else {
            return None;
        };
        sessions.get(i)
    }

    /// Is this console session already open in a pane?
    fn pane_for(&self, name: &str) -> Option<usize> {
        self.sessions.iter().position(|s| s.durable_name == name)
    }

    /// The sessions the background auto-connect should attach right now:
    /// every listed session on a running agent that has no pane yet, each
    /// once per run. Marks what it returns as in flight, so the rows wear
    /// spinners and the next pass doesn't return them again.
    ///
    /// Empty while a connect could not quietly succeed: an unregistered key
    /// would land the pane on the relay's signup screen, the wizard means
    /// nothing is set up yet, and `railway code` came for its one session.
    pub fn take_auto_connects(&mut self) -> Vec<AutoConnect> {
        if self.quit_when_done
            || self.wizard.is_some()
            || !matches!(self.ssh_key, SshKeyState::Ready | SshKeyState::Unknown)
        {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut already_open = Vec::new();
        for ws in &self.tree {
            for proj in &ws.projects {
                for env in &proj.envs {
                    let Load::Loaded(agents) = &env.agents else {
                        continue;
                    };
                    for agent in agents {
                        // Only a green agent: attaching to a sleeping or
                        // waking VM would hang the pane, and the wake path
                        // reattaches on its own once the agent is up.
                        if agent.status != "running" || self.ops.contains_key(agent.id.as_str()) {
                            continue;
                        }
                        let LoadSessions::Loaded(sessions) = &agent.sessions else {
                            continue;
                        };
                        for session in sessions {
                            // `created_at` is only absent on the placeholders
                            // adopted from our own panes — already attached by
                            // definition.
                            if !session.is_interesting()
                                || session.created_at.is_none()
                                || self.ending.contains(&session.name)
                                || self.auto_attempted.contains(&session.name)
                            {
                                continue;
                            }
                            if self.pane_for(&session.name).is_some() {
                                // Attached by some other path; remember that,
                                // so closing the pane later doesn't get undone
                                // by the next sweep.
                                already_open.push(session.name.clone());
                                continue;
                            }
                            out.push(AutoConnect {
                                agent_id: agent.id.clone(),
                                agent_name: agent.name.clone(),
                                environment_id: env.id.clone(),
                                session_name: session.name.clone(),
                            });
                        }
                    }
                }
            }
        }
        self.auto_attempted.extend(already_open);
        // Bounded fan-out: an account with a dozen running agents must not
        // open every session's ssh at once — each attach is a relay
        // connection, a reader thread, and a full replay competing for the
        // render lock. Only what fits under the cap starts now; the rest are
        // left unmarked, and every completion wakes the loop, which calls
        // here again and takes the next batch.
        let budget = AUTO_CONNECT_INFLIGHT.saturating_sub(self.connecting.len());
        out.truncate(budget);
        for connect in &out {
            self.auto_attempted.insert(connect.session_name.clone());
            self.connecting.insert(connect.session_name.clone());
        }
        out
    }

    /// Adopt a session the background auto-connect opened. Quietly: the pane
    /// joins `sessions` and its row turns green, but focus, screen, cursor
    /// and prompt all stay where they are — coming up connected must not
    /// yank the keyboard out from under whatever is being typed.
    pub fn attach_session_background(
        &mut self,
        session: super::session::Session,
        agent_id: String,
    ) {
        self.connecting.remove(&session.durable_name);
        self.auto_attempted.insert(session.durable_name.clone());
        self.drop_seen.remove(&session.durable_name);
        // A pane opening proves a wake landed, same as a foreground attach.
        if self.ops.get(agent_id.as_str()) == Some(&AgentOp::Wake.pending_label()) {
            self.ops.remove(agent_id.as_str());
        }
        if self
            .watching
            .get(agent_id.as_str())
            .is_some_and(|w| w.want == "running")
        {
            self.watching.remove(agent_id.as_str());
        }
        self.sessions.push(session);
        self.adopt_pane_sessions();
    }

    /// A background attach didn't make it. Quietly again: the spinner comes
    /// off, the reason rides the status line rather than a toast, and the
    /// name stays attempted so the failure isn't retried in a loop —
    /// double-clicking the row still connects by hand.
    pub fn auto_connect_failed(&mut self, session_name: &str, err: &str) {
        self.connecting.remove(session_name);
        let flat = err.split_whitespace().collect::<Vec<_>>().join(" ");
        self.status = format!("Couldn't connect {session_name}: {flat}");
    }

    /// Adopt a freshly opened session, make it active, and focus it.
    ///
    /// Appended rather than replacing: several agents can be working at once,
    /// and closing one because another opened would throw away a running task.
    pub fn attach_session(&mut self, session: super::session::Session, agent_id: String) {
        self.loading.active = false;
        self.connecting.remove(&session.durable_name);
        self.auto_attempted.insert(session.durable_name.clone());
        self.drop_seen.remove(&session.durable_name);
        // A pane just opened on this agent, so a wake we were waiting for has
        // demonstrably landed — whatever the status projection still says. Left
        // alone, `waking…` would sit on the row until a poll happened to catch
        // `running`, and the 1.5s watch tick would keep asking the environment
        // for up to WAKE_PATIENCE about an agent already taking keystrokes.
        // Sleep and delete are not settled by this: neither is proven by a pane
        // opening, and a delete is about to take the row away regardless.
        if self.ops.get(agent_id.as_str()) == Some(&AgentOp::Wake.pending_label()) {
            self.ops.remove(agent_id.as_str());
        }
        if self
            .watching
            .get(agent_id.as_str())
            .is_some_and(|w| w.want == "running")
        {
            self.watching.remove(agent_id.as_str());
        }
        // The session is what was opened, so that is what the cursor should be
        // on. The agent is only a fallback for a session whose row does not
        // exist yet — a fresh launch, whose session list has not come back.
        self.pending_select_session = Some(self.sessions_next_name(&session));
        self.pending_select = Some(agent_id);
        self.sessions.push(session);
        self.active = Some(self.sessions.len() - 1);
        self.focus = ManageFocus::Session;
        self.screen = Screen::Manage;
        self.clear_prompt();
        self.status.clear();
        // The session just opened is a fact; put it in the tree now rather
        // than after the platform lists it (this also lands the cursor on it).
        self.adopt_pane_sessions();
        if self.pending_select_session.is_none() {
            // Landed on the session; the agent fallback is not needed.
            self.pending_select = None;
        } else {
            self.select_pending();
        }
    }

    /// Fold every open pane's session into the tree, ahead of the platform.
    ///
    /// We opened these sessions ourselves — the pane is attached over ssh —
    /// so waiting for `cloudAgentConsoleSessions` to list them is waiting for
    /// the relay's bookkeeping to catch up with a fact we already know. On a
    /// fresh agent that wait was the whole story: the settle refreshes fired
    /// before the agent row existed, and the session showed up disconnected-
    /// looking (or not at all) until the next sweep. Called after every load,
    /// so a platform reply that hasn't caught up yet can't erase the row; the
    /// reply that has simply replaces it with the real record.
    pub fn adopt_pane_sessions(&mut self) {
        // First, reconcile names. The relay registers a session under a name
        // of its own — the one the client sent rides along only as an env
        // stamp — so once the platform lists the session a pane opened, that
        // listing's name is the durable identity: reattach, copy-ssh and kill
        // all go by it. The pane we hold takes the listed name over the one it
        // was minted with, and the placeholder row under the minted name
        // vanishes with the rename. Matched conservatively: only a listed
        // session that is running, `attached` (we demonstrably are), and not
        // already claimed by another pane — newest first when several fit.
        for i in 0..self.sessions.len() {
            if self.sessions[i].ended() {
                continue;
            }
            let agent_id = self.sessions[i].agent_id.clone();
            let minted = self.sessions[i].durable_name.clone();
            let listed = self.agent_session_names(&agent_id);
            if listed.is_empty() || listed.iter().any(|(name, ..)| *name == minted) {
                continue;
            }
            let claimed: std::collections::HashSet<String> = self
                .sessions
                .iter()
                .map(|pane| pane.durable_name.clone())
                .collect();
            let real = listed
                .into_iter()
                .filter(|(name, attached, running, _)| {
                    // Never adopt a name whose attach is in flight: the
                    // auto-connect that dialed it is about to land a pane
                    // under that exact name, and two panes sharing a durable
                    // name would misroute reattach/copy-ssh/kill by
                    // first-match.
                    *attached
                        && *running
                        && !claimed.contains(name)
                        && !self.connecting.contains(name.as_str())
                })
                .max_by_key(|(_, _, _, created)| *created)
                .map(|(name, ..)| name);
            if let Some(real) = real {
                if self.pending_select_session.as_deref() == Some(minted.as_str()) {
                    self.pending_select_session = Some(real.clone());
                }
                // The listed name is attached now, whatever it was minted as —
                // auto-connect must treat it as already tried.
                self.auto_attempted.insert(real.clone());
                self.sessions[i].durable_name = real;
            }
        }

        // Then adopt what is still missing: a pane whose session the platform
        // has not listed yet gets a placeholder row, so a fresh launch shows
        // selected and connected instead of waiting on the relay's
        // bookkeeping. The reconciliation above retires it as soon as the
        // real record lands.
        let panes: Vec<(String, String)> = self
            .sessions
            .iter()
            .filter(|pane| !pane.ended())
            .map(|pane| (pane.agent_id.clone(), pane.durable_name.clone()))
            .collect();
        for (agent_id, name) in panes {
            'tree: for ws in &mut self.tree {
                for proj in &mut ws.projects {
                    for env in &mut proj.envs {
                        let Load::Loaded(agents) = &mut env.agents else {
                            continue;
                        };
                        let Some(agent) = agents.iter_mut().find(|a| a.id == agent_id) else {
                            continue;
                        };
                        let ours = ConsoleSession {
                            name: name.clone(),
                            kind: "SHELL".into(),
                            command: None,
                            running: true,
                            attached: true,
                            created_at: None,
                            snapshot: None,
                        };
                        match &mut agent.sessions {
                            LoadSessions::Loaded(sessions) => {
                                if !sessions.iter().any(|s| s.name == name) {
                                    sessions.push(ours);
                                }
                            }
                            other => *other = LoadSessions::Loaded(vec![ours]),
                        }
                        break 'tree;
                    }
                }
            }
        }
        self.select_pending_session();
    }

    /// What a maximized-header tab says for the pane at `index`: the
    /// session's own label — its seeded task, or its harness-led name — not
    /// the agent it happens to run on. Falls back to the pane's durable name
    /// while the platform hasn't listed the session yet.
    pub fn session_tab_label(&self, index: usize) -> String {
        let Some(pane) = self.sessions.get(index) else {
            return String::new();
        };
        for ws in &self.tree {
            for proj in &ws.projects {
                for env in &proj.envs {
                    let Load::Loaded(agents) = &env.agents else {
                        continue;
                    };
                    let Some(agent) = agents.iter().find(|a| a.id == pane.agent_id) else {
                        continue;
                    };
                    if let LoadSessions::Loaded(sessions) = &agent.sessions
                        && let Some(session) = sessions.iter().find(|s| s.name == pane.durable_name)
                    {
                        return truncate(&session.short_name(), 20);
                    }
                }
            }
        }
        truncate(&pane.durable_name, 20)
    }

    /// The session pane's title, read like a project reference:
    /// `project / agent / session`. The session leg is the listed session's
    /// short name once the platform has it, the pane's durable name until
    /// then; the project leg drops off when the agent isn't in the tree yet,
    /// leaving `agent / session` rather than a hole.
    pub fn pane_breadcrumb(&self, pane: &super::session::Session) -> String {
        let mut project = None;
        let mut session = None;
        'tree: for ws in &self.tree {
            for proj in &ws.projects {
                for env in &proj.envs {
                    let Load::Loaded(agents) = &env.agents else {
                        continue;
                    };
                    let Some(agent) = agents.iter().find(|a| a.id == pane.agent_id) else {
                        continue;
                    };
                    project = Some(proj.name.clone());
                    if let LoadSessions::Loaded(sessions) = &agent.sessions
                        && let Some(listed) = sessions.iter().find(|s| s.name == pane.durable_name)
                    {
                        session = Some(listed.short_name());
                    }
                    break 'tree;
                }
            }
        }
        let session = session.unwrap_or_else(|| pane.durable_name.clone());
        match project {
            Some(project) => format!("{project} / {} / {session}", pane.agent_name),
            None => format!("{} / {session}", pane.agent_name),
        }
    }

    /// Every session the platform lists for an agent, as
    /// `(name, attached, running, created_at)` — the fields pane
    /// reconciliation matches on.
    #[allow(clippy::type_complexity)]
    fn agent_session_names(
        &self,
        agent_id: &str,
    ) -> Vec<(String, bool, bool, Option<chrono::DateTime<chrono::Utc>>)> {
        for ws in &self.tree {
            for proj in &ws.projects {
                for env in &proj.envs {
                    let Load::Loaded(agents) = &env.agents else {
                        continue;
                    };
                    let Some(agent) = agents.iter().find(|a| a.id == agent_id) else {
                        continue;
                    };
                    let LoadSessions::Loaded(sessions) = &agent.sessions else {
                        return Vec::new();
                    };
                    return sessions
                        .iter()
                        // Placeholders carry no timestamp; only platform
                        // records can be adopted as a pane's real name.
                        .filter(|s| s.created_at.is_some())
                        .map(|s| (s.name.clone(), s.attached, s.running, s.created_at))
                        .collect();
                }
            }
        }
        Vec::new()
    }

    /// Show an already-open session. Returns false when there isn't one.
    ///
    /// A pane whose connection has ended doesn't count — "connect" must not
    /// resolve to a corpse — and asking for the agent again is the natural
    /// moment to clear such panes out of the way of the fresh connection.
    pub fn activate_session(&mut self, agent_id: &str) -> bool {
        match self
            .sessions
            .iter()
            .position(|s| s.agent_id == agent_id && !s.ended())
        {
            Some(i) => {
                self.active = Some(i);
                self.focus = ManageFocus::Session;
                true
            }
            None => {
                while let Some(i) = self.sessions.iter().position(|s| s.agent_id == agent_id) {
                    drop(self.take_session(i));
                }
                false
            }
        }
    }

    /// Drop a session's local half without sleeping its agent — for handing
    /// the same session to a full-screen client, which needs the box awake.
    pub fn detach_session(&mut self, index: usize) -> Option<super::session::Session> {
        self.take_session(index)
    }

    /// Refetch one agent's sessions, wherever it is in the tree.
    ///
    /// Only when someone is looking: the row is expanded, so its session rows
    /// are on screen, or there is an open pane onto the agent — a session
    /// started or ended from that pane changes the `(N)` beside a collapsed
    /// row, and refusing to refetch left that count wrong until the agent was
    /// expanded again.
    pub fn refresh_agent_sessions(&mut self, agent_id: &str) -> Option<Effect> {
        let has_pane = self.sessions.iter().any(|s| s.agent_id == agent_id);
        for w in 0..self.tree.len() {
            for p in 0..self.tree[w].projects.len() {
                for e in 0..self.tree[w].projects[p].envs.len() {
                    let Load::Loaded(agents) = &self.tree[w].projects[p].envs[e].agents else {
                        continue;
                    };
                    let Some(a) = agents.iter().position(|agent| agent.id == agent_id) else {
                        continue;
                    };
                    if !agents[a].expanded && !has_pane {
                        return None;
                    }
                    return Some(Effect::LoadSessions {
                        agent_id: agent_id.to_string(),
                        environment_id: self.tree[w].projects[p].envs[e].id.clone(),
                        path: (w, p, e, a),
                    });
                }
            }
        }
        None
    }

    /// Remove a session by index, keeping `active` pointing at something real.
    pub fn take_session(&mut self, index: usize) -> Option<super::session::Session> {
        if index >= self.sessions.len() {
            return None;
        }
        let session = self.sessions.remove(index);
        self.active = if self.sessions.is_empty() {
            None
        } else {
            Some(self.active.unwrap_or(0).min(self.sessions.len() - 1))
        };
        if self.sessions.is_empty() && self.focus != ManageFocus::Tree {
            self.focus = ManageFocus::Tree;
        }
        self.unmaximize_without_a_session();
        Some(session)
    }

    /// Close panes whose ssh finished — the harness exited and the remote
    /// command ran out.
    ///
    /// The reader thread flips `ended` and wakes the loop, which lands here.
    /// With the pane remote command's shell fallback gone, "finished" now
    /// includes ctrl-c: interrupting the agent out of existence ends the
    /// session rather than dropping into a VM shell inside the frame. Only a
    /// clean exit reaps (see [`Session::finished`]): a dropped connection or
    /// a failed connect exits nonzero, and that pane stays up — it holds the
    /// only account of what happened, and the recovery keys can reconnect it.
    /// When the last pane closes under `railway code`, the TUI leaves with it
    /// — see [`Self::quit_when_done`].
    /// A finish this soon after connecting is a harness that cannot start;
    /// replacing it would just watch it die again, forever.
    const RESPAWN_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(10);

    /// Keystrokes this close to the end mean the user ended it themselves —
    /// ctrl-c, ctrl-d, and `/exit` all arrive as input moments before ssh
    /// does. A respawn then would undo the gesture.
    const DELIBERATE_EXIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

    pub fn reap_ended_sessions(&mut self) -> Option<Effect> {
        let mut closed = None;
        let mut respawn = None;
        let mut i = 0;
        while i < self.sessions.len() {
            if self.sessions[i].finished() {
                // Judged before the take, while the pane still knows: only
                // the session the user was typing into earns a replacement,
                // and only when nothing says they ended it on purpose. A
                // background pane ending must never conjure sessions the
                // user is not looking at.
                let watched = self.screen == Screen::Manage
                    && self.focus == ManageFocus::Session
                    && self.active == Some(i);
                let unasked = !self.sessions[i].input_within(Self::DELIBERATE_EXIT_WINDOW);
                let settled = self.sessions[i].age() >= Self::RESPAWN_MIN_AGE;
                // Dropping the session detaches its local half; the agent
                // stays running (sleeping is deliberate, never a side effect).
                if let Some(session) = self.take_session(i) {
                    if watched && unasked && settled {
                        respawn = Some((session.agent_id.clone(), session.harness.clone()));
                    }
                    closed = Some(session.agent_name.clone());
                }
            } else {
                i += 1;
            }
        }
        self.notice_dropped_panes();
        let agent_name = closed?;
        if self.quit_when_done && self.sessions.is_empty() {
            self.exit_note = Some(format!(
                "Session ended — agent {agent_name} is still running. `railway ca sleep {agent_name}` stops the compute bill."
            ));
            return Some(Effect::Quit);
        }
        // The session died out from under the user: start a fresh one on the
        // same agent so the pane they were working in comes back, rather than
        // leaving them at a toast. Skipped when another pane on the agent is
        // still up (that one is the work now), and while a dialog owns the
        // keyboard — a launch underneath a y/n would race its answer.
        if let Some((agent_id, harness)) = respawn
            && !self.sessions.iter().any(|s| s.agent_id == agent_id)
            && self.confirm.is_none()
            && self.ssh_gate.is_none()
            && let Some(effect) = self.respawn_on(&agent_id, &harness)
        {
            self.toast(format!(
                "Session on {agent_name} ended — starting a new one"
            ));
            return Some(effect);
        }
        self.toast(format!("Session on {agent_name} ended"));
        None
    }

    /// A replacement session for one that died unasked (see the respawn arm
    /// of [`Self::reap_ended_sessions`]).
    ///
    /// Deliberately not [`Self::launch`]: that reads the launcher's draft, and
    /// a respawn must not submit whatever half-sentence is sitting in the
    /// prompt box. Same harness as the session that died, a fresh durable
    /// session (`force_new` — the old one exited with its harness), and only
    /// onto a machine the platform still says is running: waking an agent is
    /// a gesture, never a reap side effect.
    fn respawn_on(&mut self, agent_id: &str, harness: &str) -> Option<Effect> {
        for w in 0..self.tree.len() {
            for p in 0..self.tree[w].projects.len() {
                for e in 0..self.tree[w].projects[p].envs.len() {
                    let agents = self.tree[w].projects[p].envs[e].agents_vec();
                    let Some(agent) = agents.iter().find(|agent| agent.id == agent_id) else {
                        continue;
                    };
                    if agent.status != "running" {
                        return None;
                    }
                    let target = self.target_at((w, p, e))?;
                    let harness = if harness.is_empty() {
                        self.harness_name().to_string()
                    } else {
                        harness.to_string()
                    };
                    return Some(Effect::Launch(LaunchRequest {
                        project_id: target.project_id.clone(),
                        environment_id: target.environment_id.clone(),
                        agent_id: Some(agent_id.to_string()),
                        session_name: None,
                        force_new: true,
                        new_session: false,
                        harness,
                        prompt: None,
                        label: target.label(),
                        base: Default::default(),
                    }));
                }
            }
        }
        None
    }

    /// A reconnect aimed at a session the agent no longer has — it closed, or
    /// the machine died and came back without it. Connecting was the ask, so
    /// the answer is a fresh session on that agent, not an error: the stale
    /// row goes now (the platform just said it does not exist), and the
    /// replacement runs the harness the row named where it still names one.
    pub fn reattach_target_gone(
        &mut self,
        agent_id: &str,
        agent_name: &str,
        session_name: &str,
    ) -> Option<Effect> {
        self.connecting.remove(session_name);
        let harness = self
            .remove_session_row(agent_id, session_name)
            .unwrap_or_default();
        if let Some(effect) = self.respawn_on(agent_id, &harness) {
            self.toast(format!(
                "Session {session_name} is gone — starting a new one"
            ));
            return Some(effect);
        }
        // No replacement to offer — the agent stopped running underneath the
        // reconnect. Say what happened and re-ask for the list, so the tree
        // settles on the truth rather than the row that just misled us.
        self.toast_error(format!(
            "Session {session_name} is gone, and {agent_name} isn't running"
        ));
        self.refresh_agent_sessions(agent_id)
    }

    /// Drop one session row from the tree — the platform just said the
    /// session does not exist — returning the harness it claimed to run, for
    /// a replacement to launch.
    fn remove_session_row(&mut self, agent_id: &str, session_name: &str) -> Option<String> {
        let mut harness = None;
        'search: for ws in &mut self.tree {
            for project in &mut ws.projects {
                for env in &mut project.envs {
                    let Load::Loaded(agents) = &mut env.agents else {
                        continue;
                    };
                    let Some(agent) = agents.iter_mut().find(|agent| agent.id == agent_id) else {
                        continue;
                    };
                    let LoadSessions::Loaded(sessions) = &mut agent.sessions else {
                        break 'search;
                    };
                    let Some(i) = sessions.iter().position(|s| s.name == session_name) else {
                        break 'search;
                    };
                    harness = sessions.remove(i).harness_slug().map(str::to_string);
                    break 'search;
                }
            }
        }
        self.clamp_cursor();
        harness
    }

    /// A pane whose connection just DIED (rather than finished) pulls the
    /// keyboard to itself, once: its banner says "r reconnects", and r has to
    /// mean that immediately — not the tree's refresh — without needing a
    /// click first. Once per drop: Esc-ing back to the tree sticks, and a
    /// reconnected pane that drops again gets to announce itself again.
    ///
    /// Only the pane on screen: a background pane (an auto-connect the user
    /// never opened) dropping must not yank the keyboard out from under
    /// whatever they are doing — its banner waits on its row. And never over
    /// a dialog or the launcher prompt: a y/n mid-answer or a sentence
    /// mid-typing would route keystrokes into the corpse, where a stray `x`
    /// closes it.
    fn notice_dropped_panes(&mut self) {
        for i in 0..self.sessions.len() {
            if !self.sessions[i].dropped() {
                continue;
            }
            let name = self.sessions[i].durable_name.clone();
            if !self.drop_seen.insert(name) {
                continue;
            }
            if self.screen == Screen::Manage
                && self.active == Some(i)
                && !self.launcher_selected()
                && self.confirm.is_none()
                && self.ssh_gate.is_none()
            {
                self.focus = ManageFocus::Session;
            }
        }
    }

    /// Any pane that has ended but whose finished/dropped call is still
    /// waiting on ssh's exit status. The loop polls briefly while this holds
    /// — the EOF that woke it can beat waitpid, and nothing else would wake
    /// the loop again to ask.
    pub fn awaiting_exit_status(&self) -> bool {
        self.sessions.iter().any(|s| s.awaiting_exit_status())
    }

    /// Hand the whole terminal to the session under the cursor.
    ///
    /// ⌥enter is the intended chord, but plenty of terminals do not send a
    /// modifier with Enter at all, so `f` does the same thing.
    ///
    /// Deliberately not shift+enter: that one belongs to the harness, where it
    /// is the newline every text field gives you. This binding only ever fires
    /// with the tree focused, so ⌥enter inside a session still reaches the
    /// agent as `ESC CR` — which is the other newline chord harnesses take.
    fn full_screen_current(&mut self) -> Option<Effect> {
        // The row under the cursor if it names a session, else whatever the
        // pane is showing.
        if let Some(row) = self.selected_row()
            && let RowKind::Session(w, p, e, a, i) = row.kind
            && let Some(session) = self.console_session(w, p, e, a, i)
        {
            let name = session.name.clone();
            let (agent_id, agent_name) = self.agent_at(w, p, e, a)?;
            return Some(Effect::FullScreen {
                agent_id,
                session_name: name,
                agent_name,
            });
        }
        let session = self.sessions.get(self.active?)?;
        Some(Effect::FullScreen {
            agent_id: session.agent_id.clone(),
            session_name: session.durable_name.clone(),
            agent_name: session.agent_name.clone(),
        })
    }

    /// Keys for a pane whose connection is gone — its ssh exited (ended) or
    /// its attach is streaming nothing (stalled).
    ///
    /// Reconnect, close, or step back out; everything else is swallowed
    /// rather than written into a pty nothing is reading. Enter is taken as
    /// reconnect because it is the reflex after "connection closed", and
    /// reconnecting is the only thing it could mean here.
    fn dead_pane_key(&mut self, index: usize, key: KeyEvent) -> Option<Effect> {
        match key.code {
            KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Enter => {
                self.reconnect_session(index)
            }
            KeyCode::Char('x') | KeyCode::Char('X') => Some(Effect::CloseSession { index }),
            // Nothing in the pane can receive an Escape any more, so it
            // releases to the tree — the same place the release chords go.
            KeyCode::Esc => {
                self.focus = ManageFocus::Tree;
                self.maximized = false;
                None
            }
            _ => None,
        }
    }

    /// Drop a pane whose connection died and dial its durable session again.
    ///
    /// Through [`Effect::Reattach`] rather than respawning ssh from the
    /// pane's stored plumbing: the reattach path re-resolves the identity and
    /// passes the ssh-key gate, so a key deregistered mid-session gets the
    /// gate's question instead of another connection that dies the same way.
    fn reconnect_session(&mut self, index: usize) -> Option<Effect> {
        let session = self.take_session(index)?;
        let Some(environment_id) = session.environment_id() else {
            // A target that isn't `agent:<env>:<id>` has nothing to resolve;
            // the session row in the tree still knows how to reach it.
            self.toast_error("Couldn't resolve this session — reconnect from its row in the tree");
            return None;
        };
        self.status = format!("Reconnecting to {}…", session.durable_name);
        Some(Effect::Reattach {
            agent_id: session.agent_id.clone(),
            agent_name: session.agent_name.clone(),
            environment_id,
            session_name: session.durable_name.clone(),
        })
    }

    /// Connect to a session row: show its pane if we have one, else reattach.
    fn reattach_row(&mut self, kind: RowKind) -> Option<Effect> {
        let RowKind::Session(w, p, e, a, i) = kind else {
            return None;
        };
        let name = self.console_session(w, p, e, a, i)?.name.clone();
        let (agent_id, agent_name) = self.agent_at(w, p, e, a)?;
        self.target = self.target_at((w, p, e));

        // An attach already in flight — usually the startup auto-connect —
        // would race a second ssh onto the same session.
        if self.connecting.contains(&name) {
            self.status = format!("Connecting to {name}…");
            return None;
        }

        // Already open: this means "put me in it", not "open another one" —
        // unless the pane's connection has died, in which case "connect"
        // means what it says. Focusing the corpse was a trap: the dead pane
        // blocked every reattach to its session until someone discovered
        // that `x` on the *agent* row closes panes.
        if let Some(index) = self.pane_for(&name) {
            if self.sessions[index].ended() {
                drop(self.take_session(index));
            } else {
                self.active = Some(index);
                self.focus = ManageFocus::Session;
                return None;
            }
        }
        // Not open: reconnect straight to it. Reattaching needs no credential,
        // no skills sync and no provisioning — the agent is already set up,
        // which is what makes this instant rather than a launch.
        let environment_id = self.tree[w].projects[p].envs[e].id.clone();
        let status = self
            .agent_status(w, p, e, a)
            .unwrap_or_else(|| "unknown".into());
        if status != "running" {
            self.status = format!("{agent_name} is {status} — press w to wake it first");
            return None;
        }
        Some(Effect::Reattach {
            agent_id,
            agent_name,
            environment_id,
            session_name: name,
        })
    }

    /// The status of the agent the cursor is on, directly or via a session.
    /// The footer uses it to offer sleep or wake, never both.
    pub fn selected_agent_status(&self) -> Option<String> {
        let row = self.selected_row()?;
        let (w, p, e, a) = match row.kind {
            RowKind::Agent(w, p, e, a) | RowKind::Session(w, p, e, a, _) => (w, p, e, a),
            _ => return None,
        };
        self.agent_status(w, p, e, a)
    }

    /// The open pane a row refers to: a session's own, or the first one on an
    /// agent.
    pub fn pane_for_row(&self, kind: RowKind) -> Option<usize> {
        match kind {
            RowKind::Session(w, p, e, a, i) => {
                let name = self.console_session(w, p, e, a, i)?.name.clone();
                self.pane_for(&name)
            }
            RowKind::Agent(w, p, e, a) => {
                let (agent_id, _) = self.agent_at(w, p, e, a)?;
                self.sessions.iter().position(|s| s.agent_id == agent_id)
            }
            _ => None,
        }
    }

    fn agent_status(&self, w: usize, p: usize, e: usize, a: usize) -> Option<String> {
        let Load::Loaded(agents) = &self.tree.get(w)?.projects.get(p)?.envs.get(e)?.agents else {
            return None;
        };
        Some(agents.get(a)?.status.clone())
    }

    fn agent_at(&self, w: usize, p: usize, e: usize, a: usize) -> Option<(String, String)> {
        let Load::Loaded(agents) = &self.tree.get(w)?.projects.get(p)?.envs.get(e)?.agents else {
            return None;
        };
        let agent = agents.get(a)?;
        Some((agent.id.clone(), agent.name.clone()))
    }

    /// Cycle focus: tree → sessions → the pane → tree, skipping what isn't
    /// there. One key to move between the three things on this screen.
    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            ManageFocus::Tree if self.active.is_some() => ManageFocus::Session,
            _ => ManageFocus::Tree,
        };
    }

    /// Move the cursor onto the agent a launch just opened, if its row exists
    /// yet. Called again after each load, since the row appears only once the
    /// environment's agents have arrived.
    pub fn select_pending(&mut self) {
        let Some(id) = self.pending_select.clone() else {
            return;
        };
        let rows = self.rows();
        for (i, row) in rows.iter().enumerate() {
            if let RowKind::Agent(w, p, e, a) = row.kind
                && self.tree[w].projects[p].envs[e]
                    .agents_vec()
                    .get(a)
                    .is_some_and(|agent| agent.id == id)
            {
                self.cursor = i;
                self.pending_select = None;
                return;
            }
        }
    }

    /// Running agents whose sessions have not been fetched.
    ///
    /// Called after an environment loads, so the `(N)` beside an agent is
    /// there without expanding it. Only running agents: a sleeping box has
    /// nothing running on it, and asking would spend a request to learn that.
    pub fn sessions_to_prefetch(&mut self) -> Vec<Effect> {
        let mut out = Vec::new();
        for w in 0..self.tree.len() {
            for p in 0..self.tree[w].projects.len() {
                for e in 0..self.tree[w].projects[p].envs.len() {
                    let environment_id = self.tree[w].projects[p].envs[e].id.clone();
                    let Load::Loaded(agents) = &mut self.tree[w].projects[p].envs[e].agents else {
                        continue;
                    };
                    for (a, agent) in agents.iter_mut().enumerate() {
                        if agent.status != "running" || agent.sessions != LoadSessions::NotLoaded {
                            continue;
                        }
                        agent.sessions = LoadSessions::Loading;
                        out.push(Effect::LoadSessions {
                            agent_id: agent.id.clone(),
                            environment_id: environment_id.clone(),
                            path: (w, p, e, a),
                        });
                    }
                }
            }
        }
        out
    }

    /// The agents whose session lists a refresh should ask about again.
    ///
    /// Deliberately narrower than [`Self::sessions_to_prefetch`], which fills in
    /// every running agent's count once. `cloudAgentConsoleSessions` takes one
    /// agent — `CloudAgent.sessions` in the schema is snapshots, not console
    /// sessions, so there is no batch form — which makes a full sweep one
    /// request per agent, every tick. Only what someone is looking at is worth
    /// that: an expanded row, whose session rows are on screen, or an agent with
    /// an open pane. In practice nought to two, whatever the size of the
    /// account.
    ///
    /// Unlike the prefetch it does not blank the list on its way out: the rows
    /// stay put and are replaced when the reply lands, so nothing flickers on a
    /// timer.
    pub fn sessions_to_refresh(&self) -> Vec<Effect> {
        let mut out = Vec::new();
        for (w, ws) in self.tree.iter().enumerate() {
            for (p, project) in ws.projects.iter().enumerate() {
                for (e, env) in project.envs.iter().enumerate() {
                    let Load::Loaded(agents) = &env.agents else {
                        continue;
                    };
                    for (a, agent) in agents.iter().enumerate() {
                        // A sleeping box runs nothing, so asking would spend a
                        // request to learn that.
                        if agent.status != "running" {
                            continue;
                        }
                        // An agent whose count was never fetched is left to
                        // [`Self::sessions_to_prefetch`], which the same reply
                        // runs — and which has already marked what it claimed
                        // as loading. Asking about those here as well would be
                        // two requests for one agent.
                        if agent.sessions == LoadSessions::Loading {
                            continue;
                        }
                        // A failed fetch retries on the refresh cadence,
                        // watched or not: a transient 502 during the startup
                        // prefetch would otherwise leave the agent showing
                        // zero threads forever — nothing else re-asks for an
                        // unexpanded row.
                        let failed = matches!(agent.sessions, LoadSessions::Failed(_));
                        // A loaded thread's row is always on screen now — the
                        // sidebar lists sessions, not agents — so a running
                        // agent whose threads are visible stays on the refresh
                        // cadence: its labels are live status text.
                        let visible = matches!(&agent.sessions, LoadSessions::Loaded(sessions)
                            if sessions.iter().any(|s| s.is_interesting()));
                        let watched = agent.expanded
                            || visible
                            || self.sessions.iter().any(|s| s.agent_id == agent.id);
                        if !watched && !failed {
                            continue;
                        }
                        out.push(Effect::LoadSessions {
                            agent_id: agent.id.clone(),
                            environment_id: env.id.clone(),
                            path: (w, p, e, a),
                        });
                    }
                }
            }
        }
        out
    }

    /// The environments to fetch before anyone touches a key, on a backboard
    /// that predates `myCloudAgents`.
    ///
    /// Only where the answer is needed immediately: the prompt's target, which
    /// New Session reads to decide whether it has an agent to work on, and the
    /// default project, whose agents the tree wants to lead with. Everything
    /// else loads when its row is expanded.
    ///
    /// This used to be every environment in every project in every workspace,
    /// which is one request each. That is fine on a small account and hundreds
    /// of requests on a large one — enough to rate limit the caller before the
    /// tree had finished drawing, and enough backend load per launch to be
    /// worth not doing. The cost of loading less is that a project's agent
    /// count appears when you open it rather than immediately.
    pub fn initial_environments(&mut self) -> Vec<Effect> {
        let mut wanted: Vec<String> = self.known_environments.clone();
        if let Some(target) = self.target.as_ref() {
            wanted.push(target.environment_id.clone());
        }
        // The default project's environments, whose agents the tree leads
        // with and where a launch with no target lands.
        if let Some(project_id) = self.default_project.clone() {
            for ws in &self.tree {
                for project in &ws.projects {
                    if project.id != project_id {
                        continue;
                    }
                    wanted.extend(project.envs.iter().map(|env| env.id.clone()));
                }
            }
        }

        let mut out = Vec::new();
        for w in 0..self.tree.len() {
            for p in 0..self.tree[w].projects.len() {
                for e in 0..self.tree[w].projects[p].envs.len() {
                    let env = &mut self.tree[w].projects[p].envs[e];
                    if env.agents != Load::NotLoaded || !wanted.contains(&env.id) {
                        continue;
                    }
                    env.agents = Load::Loading;
                    out.push(Effect::LoadAgents {
                        environment_id: env.id.clone(),
                        path: (w, p, e),
                    });
                }
            }
        }
        out
    }

    /// After the next load, open this agent so the session that was just
    /// started is visible underneath it — which is where it belongs, and where
    /// someone will look for it.
    pub fn expand_agent_after_load(&mut self, agent_id: String) {
        self.pending_expand = Some(agent_id);
    }

    fn sessions_next_name(&self, session: &super::session::Session) -> String {
        session.durable_name.clone()
    }

    /// Put the cursor on a session row once it exists, and drop the agent-row
    /// fallback that was holding the place.
    fn select_pending_session(&mut self) {
        let Some(name) = self.pending_select_session.clone() else {
            return;
        };
        let rows = self.rows();
        for (i, row) in rows.iter().enumerate() {
            // By the session's own name, not the row label — the label carries
            // the agent's name too.
            let RowKind::Session(w, p, e, a, idx) = row.kind else {
                continue;
            };
            if self
                .console_session(w, p, e, a, idx)
                .is_some_and(|session| session.name == name)
            {
                self.cursor = i;
                self.pending_select_session = None;
                self.pending_select = None;
                return;
            }
        }
    }

    /// Honour a queued expand once the agent's row exists.
    pub fn expand_pending(&mut self) -> Option<Effect> {
        let id = self.pending_expand.clone()?;
        for w in 0..self.tree.len() {
            for p in 0..self.tree[w].projects.len() {
                for e in 0..self.tree[w].projects[p].envs.len() {
                    let Load::Loaded(agents) = &self.tree[w].projects[p].envs[e].agents else {
                        continue;
                    };
                    if let Some(a) = agents.iter().position(|agent| agent.id == id) {
                        self.pending_expand = None;
                        return self.set_agent_expanded((w, p, e, a), true);
                    }
                }
            }
        }
        None
    }

    /// Expand the path to an environment so a launched agent becomes visible.
    /// Returns the load request when its agents have not been fetched yet.
    pub fn reveal_environment(&mut self, environment_id: &str) -> Option<Effect> {
        for w in 0..self.tree.len() {
            for p in 0..self.tree[w].projects.len() {
                for e in 0..self.tree[w].projects[p].envs.len() {
                    if self.tree[w].projects[p].envs[e].id != environment_id {
                        continue;
                    }
                    self.tree[w].expanded = true;
                    self.tree[w].projects[p].expanded = true;
                    // Always refetch: the launch may have just created the
                    // agent we are about to select. What is already loaded
                    // stays on screen while the reply is on its way — blanking
                    // it would fold the group mid-poll and shove every row
                    // (and the cursor) somewhere else.
                    self.tree[w].projects[p].envs[e].expanded = true;
                    if !matches!(self.tree[w].projects[p].envs[e].agents, Load::Loaded(_)) {
                        self.tree[w].projects[p].envs[e].agents = Load::Loading;
                    }
                    return Some(Effect::LoadAgents {
                        environment_id: environment_id.to_string(),
                        path: (w, p, e),
                    });
                }
            }
        }
        None
    }

    /// The chords that work everywhere. Settings is worth one because it is
    /// only reachable by chord — and it is where the
    /// theme now cycles, which is why there is no ⌥t any more: two chords to
    /// the same preference was how they drifted apart.
    fn alt_action(&mut self, action: char) -> Option<Effect> {
        self.status.clear();
        match action {
            's' => {
                self.start_settings();
                None
            }
            'f' => {
                self.toggle_maximized();
                None
            }
            ']' => self.cycle_session(true),
            '[' => self.cycle_session(false),
            // Everything, everywhere: `r` refreshes the environment under the
            // cursor, and this is the one that answers "something changed and I
            // don't know where".
            'r' => {
                self.status = "Refreshing…".into();
                self.refresh_announce = true;
                Some(Effect::RefreshAll)
            }
            // The launchers float over the tree the launch aims at; the
            // New Session row's own prompt box covers the plain case.
            // ⌥n skips the picker: a new agent right now, on whatever
            // harness is already selected.
            'n' if self.screen == Screen::Manage => self.launch_new_agent(),
            'p' if self.screen == Screen::Manage => {
                self.manage_prompt = Some(String::new());
                self.screen = Screen::ManagePrompt;
                None
            }
            _ => None,
        }
    }

    /// Move the pane to the next or previous open session, wrapping.
    ///
    /// Scoped to panes that are already open rather than every session in the
    /// tree, and that is the whole design: waking a sleeping agent is a cold
    /// boot — seconds of wall clock and a VM that starts billing — so holding
    /// `⌥⇧]` or `⌥⇧[` must never fan out wakes across agents you were only
    /// passing through. Opening something new stays the deliberate `enter` on
    /// a row.
    fn cycle_session(&mut self, forward: bool) -> Option<Effect> {
        match self.sessions.len() {
            0 => {
                self.status = "No open sessions".to_string();
                return None;
            }
            // Nothing to move to, but silence would read as a broken key.
            1 => {
                self.status = "Only one session open".to_string();
                return None;
            }
            _ => {}
        }
        let count = self.sessions.len();
        let next = match (self.active, forward) {
            (Some(i), true) => (i + 1) % count,
            (Some(i), false) => (i + count - 1) % count,
            // No pane yet: forward starts at the front, back at the end.
            (None, true) => 0,
            (None, false) => count - 1,
        };
        self.active = Some(next);
        self.select_row_for_active();
        if let Some(name) = self.active_session().map(|s| s.agent_name.clone()) {
            self.status = format!("Session {}/{} · {name}", next + 1, count);
        }
        None
    }

    /// Put the tree cursor on the row belonging to the active pane.
    ///
    /// The inverse of `sync_active_to_cursor`, which runs after every cursor
    /// move. Cycling moves the pane directly, so without this the highlight
    /// would still sit on the previous session and the next arrow key would
    /// drag the pane straight back to it.
    fn select_row_for_active(&mut self) {
        let Some(name) = self.active_session().map(|s| s.durable_name.clone()) else {
            return;
        };
        let rows = self.rows();
        for (i, row) in rows.iter().enumerate() {
            let RowKind::Session(w, p, e, a, idx) = row.kind else {
                continue;
            };
            if self
                .console_session(w, p, e, a, idx)
                .is_some_and(|cs| cs.name == name)
            {
                self.cursor = i;
                return;
            }
        }
    }

    /// Gate a connect on the SSH key. `true` means the effect was consumed
    /// here — held behind the register question, or refused with the recipe —
    /// and the caller must not proceed with it.
    pub fn hold_for_ssh_key(&mut self, held: HeldConnect) -> bool {
        match &self.ssh_key {
            SshKeyState::NeedsRegistration(offer) => {
                self.ssh_gate = Some(SshGate {
                    offer: offer.clone(),
                    then: Some(held),
                });
                true
            }
            SshKeyState::NoLocalKeys => {
                self.toast_error("No SSH key found — run: ssh-keygen -t ed25519");
                true
            }
            SshKeyState::Ready | SshKeyState::Unknown => false,
        }
    }

    /// Offer key registration as part of setup, when the check said one is
    /// needed. No held connect: declining just ends the question.
    pub fn offer_ssh_key_setup(&mut self) {
        if self.ssh_gate.is_none()
            && let SshKeyState::NeedsRegistration(offer) = &self.ssh_key
        {
            self.ssh_gate = Some(SshGate {
                offer: offer.clone(),
                then: None,
            });
        }
    }

    /// Raise a confirmation in the corner.
    pub fn toast(&mut self, text: impl Into<String>) {
        self.raise_toast(text, true);
    }

    /// The same, for something that did not work.
    pub fn toast_error(&mut self, text: impl Into<String>) {
        self.raise_toast(text, false);
    }

    fn raise_toast(&mut self, text: impl Into<String>, ok: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            at: std::time::Instant::now(),
            ok,
        });
    }

    /// Drop it once it has had its time. Called from the event loop, which
    /// wakes for exactly this.
    pub fn expire_toast(&mut self) {
        if self.toast.as_ref().is_some_and(Toast::expired) {
            self.toast = None;
        }
    }

    /// How long the toast has left, for the loop to sleep on.
    pub fn toast_remaining(&self) -> std::time::Duration {
        self.toast
            .as_ref()
            .map(Toast::remaining)
            .unwrap_or(TOAST_LIFETIME)
    }

    /// Time until an attached pane would first count as stalled, for the loop
    /// to wake once and draw the notice. `None` when no pane is waiting on
    /// its first byte.
    pub fn stall_check_remaining(&self) -> Option<std::time::Duration> {
        self.sessions
            .iter()
            .filter_map(|session| session.stall_remaining())
            .min()
    }

    /// Give the session pane the whole screen, or give the tree back.
    ///
    /// Only where there is a session to give it to: with nothing open, a
    /// screen of empty pane is not a state worth being able to reach by
    /// accident.
    fn toggle_maximized(&mut self) {
        if self.screen != Screen::Manage {
            return;
        }
        if self.maximized {
            self.maximized = false;
            self.status = String::new();
            return;
        }
        if self.active.is_none() {
            self.status = "Connect to a session first — ⌥f gives it the screen".into();
            return;
        }
        self.maximized = true;
        // Nothing else is on screen, so nothing else should have the keyboard.
        self.focus = ManageFocus::Session;
    }

    /// Is the cursor on the pinned New Session row? While it is, the session
    /// pane shows the launcher.
    pub fn launcher_selected(&self) -> bool {
        self.focus == ManageFocus::Tree
            && self
                .selected_row()
                .is_some_and(|row| row.kind == RowKind::NewSession)
    }

    /// [`Self::launcher_selected`], with the prompt holding the keyboard.
    /// False on the same row after Esc — the "home" state, where letters are
    /// keys again and `q` quits.
    pub fn new_session_selected(&self) -> bool {
        self.launcher_selected() && self.prompt_focused
    }

    /// Keys while the cursor stands on New Session: the old menu's prompt box,
    /// without the separate screen. Everything unhandled falls back to the
    /// tree, so ↑/↓ still walk away from the row.
    fn on_key_new_session(&mut self, key: KeyEvent) -> Result<Option<Effect>, KeyEvent> {
        match key.code {
            // Typing is refused while shell is selected — the box says so —
            // rather than cleared: a draft typed for a real agent survives
            // cycling past shell.
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !self.shell_selected() =>
            {
                self.prompt_insert(c);
                Ok(None)
            }
            KeyCode::Backspace if !self.shell_selected() => {
                self.prompt_backspace();
                Ok(None)
            }
            // The cursor moves through the draft; a typo is fixed in place
            // rather than by erasing back to it.
            KeyCode::Left if !self.shell_selected() => {
                self.prompt_cursor = self.prompt_cursor.saturating_sub(1);
                Ok(None)
            }
            KeyCode::Right if !self.shell_selected() => {
                self.prompt_cursor = (self.prompt_cursor + 1).min(self.prompt.chars().count());
                Ok(None)
            }
            KeyCode::Home if !self.shell_selected() => {
                self.prompt_cursor = 0;
                Ok(None)
            }
            KeyCode::End if !self.shell_selected() => {
                self.prompt_cursor = self.prompt.chars().count();
                Ok(None)
            }
            KeyCode::BackTab => {
                self.harness = (self.harness + 1) % HARNESSES.len();
                Ok(None)
            }
            // ⇧enter is the newline every text field owes a paragraph-sized
            // prompt; plain enter still launches. ALT is accepted alongside
            // SHIFT because most terminals cannot say "shifted Enter" at all
            // (the kitty disambiguate mode exempts Enter by design) — the
            // ones configured to send it, Claude Code's own /terminal-setup
            // included, bind it to `ESC CR`, which parses as alt+enter. A
            // terminal that reports neither sends a plain Enter, so the chord
            // degrades to launching — never to a lost draft.
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
                    && !self.shell_selected() =>
            {
                self.prompt_insert('\n');
                Ok(None)
            }
            KeyCode::Enter => Ok(self.launch_from_prompt()),
            // Esc walks up, one step per press: a draft is cleared first, so
            // a stray Esc mid-typing doesn't throw the session away…
            KeyCode::Esc if !self.prompt.is_empty() => {
                self.clear_prompt();
                Ok(None)
            }
            // …then the prompt lets go of the keyboard. That is the top of
            // the chain — home — where letters are keys again and `q` quits.
            KeyCode::Esc => {
                self.prompt_focused = false;
                Ok(None)
            }
            _ => Err(key),
        }
    }

    /// The byte offset of the prompt cursor, for splicing into the string.
    fn prompt_byte(&self) -> usize {
        self.prompt
            .char_indices()
            .nth(self.prompt_cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.prompt.len())
    }

    fn prompt_insert(&mut self, c: char) {
        let at = self.prompt_byte();
        self.prompt.insert(at, c);
        self.prompt_cursor += 1;
    }

    fn prompt_insert_str(&mut self, text: &str) {
        let at = self.prompt_byte();
        self.prompt.insert_str(at, text);
        self.prompt_cursor += text.chars().count();
    }

    fn prompt_backspace(&mut self) {
        if self.prompt_cursor == 0 {
            return;
        }
        self.prompt_cursor -= 1;
        let at = self.prompt_byte();
        self.prompt.remove(at);
    }

    pub fn clear_prompt(&mut self) {
        self.prompt.clear();
        self.prompt_cursor = 0;
    }

    /// Launch what the prompt box holds — onto a brand-new agent, the same
    /// default the dashboard has: each task gets a VM of its own. A blank
    /// prompt launches the session bare: the harness comes up empty, ready
    /// for its first prompt inside.
    fn launch_from_prompt(&mut self) -> Option<Effect> {
        self.launch_new_agent()
    }

    /// A new agent in the target project, carrying the prompt draft if one is
    /// written: `n`'s picker, ⌥n's quick create, and the prompt box all end
    /// here.
    fn launch_new_agent(&mut self) -> Option<Effect> {
        if self.target.is_none() {
            self.start_target_pick();
            self.status = "Pick where this should run".into();
            return None;
        }
        self.launch(None, true)
    }

    fn on_key_manage(&mut self, key: KeyEvent) -> Option<Effect> {
        // A held action owns the keyboard until it is answered. Anything other
        // than yes cancels: a mistyped key must never be taken as consent.
        if let Some(pending) = self.confirm.clone() {
            self.confirm = None;
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.ops
                        .insert(pending.agent_id.clone(), pending.op.pending_label());
                    Some(Effect::Agent {
                        op: pending.op,
                        agent_id: pending.agent_id,
                        environment_id: pending.environment_id,
                    })
                }
                _ => {
                    self.status = "Cancelled".into();
                    None
                }
            };
        }

        // The overlay is a look-up, not a mode: the next key puts it away and
        // is otherwise ignored, so nothing happens by surprise while reading.
        if self.keys_open {
            self.keys_open = false;
            return None;
        }

        // On the New Session row the keyboard writes the prompt; only what
        // the prompt doesn't take (↑ ↓, tab, mouse chords) falls through to
        // the tree below.
        let key = if self.new_session_selected() {
            match self.on_key_new_session(key) {
                Ok(effect) => return effect,
                Err(key) => key,
            }
        } else {
            key
        };

        let row = self.selected_row();
        match key.code {
            KeyCode::Char('?') => {
                self.keys_open = true;
                None
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            // Full screen, before the plain Enter arm below claims the key.
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.full_screen_current()
            }
            KeyCode::Char('f') => self.full_screen_current(),
            // The command to reach this exact session from another terminal —
            // the same one the dashboard hands out.
            KeyCode::Char('c') => {
                let RowKind::Session(w, p, e, a, i) = row?.kind else {
                    self.status = "Select a session to copy its ssh command".into();
                    return None;
                };
                let session_name = self.console_session(w, p, e, a, i)?.name.clone();
                let (agent_id, _) = self.agent_at(w, p, e, a)?;
                Some(Effect::CopySsh {
                    agent_id,
                    environment_id: self.tree[w].projects[p].envs[e].id.clone(),
                    session_name,
                })
            }
            KeyCode::Right | KeyCode::Char('l') => self.set_expanded(row?.kind, true),
            KeyCode::Left | KeyCode::Char('h') => self.set_expanded(row?.kind, false),
            KeyCode::Enter => {
                let kind = row?.kind;
                match kind {
                    // From home, Enter steps back into the prompt.
                    RowKind::NewSession => {
                        self.prompt_focused = true;
                        None
                    }
                    RowKind::Agent(w, p, e, a) => self.connect_agent_row(w, p, e, a),
                    RowKind::Session(..) => self.reattach_row(kind),
                    // A toggle: open drills straight through to the first
                    // environment's agents — what someone opening a project is
                    // looking for — and a second enter folds the whole thing
                    // back up rather than going dead once everything under it
                    // is already open. The cursor stays on the project row so
                    // the second enter lands where the first one did.
                    RowKind::Project(w, p) => {
                        let open = self
                            .tree
                            .get(w)
                            .and_then(|ws| ws.projects.get(p))
                            .is_some_and(|project| project.expanded);
                        if open {
                            self.set_expanded(RowKind::Project(w, p), false)
                        } else {
                            self.set_expanded(RowKind::Project(w, p), true);
                            self.set_expanded(RowKind::Environment(w, p, 0), true)
                        }
                    }
                    // Workspaces and environments toggle the same way: enter
                    // on something already open closes it.
                    RowKind::Workspace(w) => {
                        let open = self.tree.get(w).is_some_and(|ws| ws.expanded);
                        self.set_expanded(RowKind::Workspace(w), !open)
                    }
                    RowKind::Environment(w, p, e) => {
                        let open = self
                            .tree
                            .get(w)
                            .and_then(|ws| ws.projects.get(p))
                            .and_then(|project| project.envs.get(e))
                            .is_some_and(|env| env.expanded);
                        self.set_expanded(RowKind::Environment(w, p, e), !open)
                    }
                    other => self.set_expanded(other, true),
                }
            }
            // `n` is the dashboard's New Agent button: pick the harness,
            // then a fresh agent — in the row's own project when the cursor
            // names one (the footer advertises row-local behavior), falling
            // back to the prompt's target from the launcher and the tail.
            // On an agent (or one of its threads) the box already exists, so
            // the pick starts a new session ON it instead.
            KeyCode::Char('n') => {
                if let Some(path) = row.as_ref().map(|r| r.kind).and_then(|k| self.env_of(k))
                    && let Some(target) = self.target_at(path)
                {
                    self.target = Some(target);
                }
                self.harness_pick_agent = match row.as_ref().map(|r| r.kind) {
                    Some(RowKind::Agent(w, p, e, a) | RowKind::Session(w, p, e, a, _)) => {
                        self.agent_at(w, p, e, a).map(|(id, _)| id)
                    }
                    _ => None,
                };
                self.harness_pick = Some(self.harness);
                self.screen = Screen::HarnessPick;
                None
            }
            KeyCode::Char('t') => {
                let path = self.env_of(row?.kind)?;
                self.target = self.target_at(path);
                self.status = match &self.target {
                    Some(t) => format!("Target set to {}", t.label()),
                    None => String::new(),
                };
                None
            }
            // `tab` walks the three panes; `^o` (see `on_key`) is the way back
            // out of a focused session.
            KeyCode::Tab => {
                self.cycle_focus();
                None
            }
            // `x` ends the highlighted session — on the agent, not just here.
            // Connected or not: the session lives on the VM either way.
            KeyCode::Char('x') => {
                let kind = row?.kind;
                let RowKind::Session(w, p, e, a, i) = kind else {
                    // On an agent, close our window onto it without ending
                    // anything; ending is `d`, and it asks first.
                    return match self.pane_for_row(kind) {
                        Some(index) => Some(Effect::CloseSession { index }),
                        None => {
                            self.status = "Select a session to end".into();
                            None
                        }
                    };
                };
                let name = self.console_session(w, p, e, a, i)?.name.clone();
                let (agent_id, _) = self.agent_at(w, p, e, a)?;
                let environment_id = self.tree[w].projects[p].envs[e].id.clone();
                self.ending.insert(name.clone());
                Some(Effect::KillSession {
                    agent_id,
                    environment_id,
                    session_name: name,
                })
            }
            // Lifecycle. Sleep and wake are reversible and act immediately;
            // delete takes the disk with it, so it asks first.
            KeyCode::Char('s') => self.agent_op(AgentOp::Sleep),
            KeyCode::Char('w') => self.agent_op(AgentOp::Wake),
            KeyCode::Char('d') => self.agent_op(AgentOp::Delete),
            // Startup loads only what a keypress needs, so this is how an agent
            // in a project you haven't opened gets found.
            KeyCode::Char('R') => Some(Effect::ScanEverywhere),
            KeyCode::Char('r') => {
                let (w, p, e) = self.env_of(row?.kind)?;
                let env = self.tree.get_mut(w)?.projects.get_mut(p)?.envs.get_mut(e)?;
                // What is already loaded stays put while the reply is on its
                // way: blanking it would fold the group and move every row.
                if !matches!(env.agents, Load::Loaded(_)) {
                    env.agents = Load::Loading;
                }
                Some(Effect::LoadAgents {
                    environment_id: env.id.clone(),
                    path: (w, p, e),
                })
            }
            // Esc walks back up the chain, one step per press, and never
            // quits: a maximized pane gets the tree back first…
            KeyCode::Esc if self.maximized => {
                self.maximized = false;
                None
            }
            // …then any thread returns to the launcher; on the launcher's
            // unfocused home (the focused case cleared the draft and let go
            // of the keyboard above) there is nowhere further up, and `q` is
            // the way out.
            KeyCode::Esc => {
                if self.cursor != 0 {
                    self.cursor = 0;
                    self.prompt_focused = true;
                }
                None
            }
            KeyCode::Char('q') => Some(Effect::Quit),
            _ => None,
        }
    }

    /// What `n` makes, which depends on what is selected.
    ///
    /// On an agent (or one of its sessions) another session on that same
    /// agent — the agent is already there, and a second one would be a second
    /// VM nobody asked for. On a project, an environment, or a group, a whole
    /// new agent, because that is the only thing "new" can mean there.
    /// Anywhere else — the tail header, an empty tree — it falls back to the
    /// target: the empty state advertises `n`, so `n` has to work from where
    /// the cursor starts.
    /// [`Self::new_here`], carrying a prompt from the ⌥p composer. `None`
    /// keeps `n`'s behavior exactly: the agent-row launch sends no prompt,
    /// and the create-an-agent paths fall back to the launcher box's draft.
    fn new_here_prompted(&mut self, prompt: Option<String>) -> Option<Effect> {
        let kind = self.selected_row().map(|row| row.kind);
        match kind {
            Some(RowKind::Agent(w, p, e, a)) | Some(RowKind::Session(w, p, e, a, _)) => {
                let (agent_id, agent_name) = self.agent_at(w, p, e, a)?;
                self.target = self.target_at((w, p, e));
                let target = self.target.clone()?;
                Some(Effect::Launch(LaunchRequest {
                    project_id: target.project_id,
                    environment_id: target.environment_id,
                    agent_id: Some(agent_id),
                    session_name: None,
                    force_new: false,
                    new_session: true,
                    harness: self.harness_name().to_string(),
                    prompt,
                    label: format!("{agent_name} · new session"),
                    base: Default::default(),
                }))
            }
            Some(RowKind::Environment(w, p, e)) => {
                self.target = self.target_at((w, p, e));
                match prompt {
                    Some(p) => self.launch_prompted(None, true, Some(p)),
                    None => self.launch(None, true),
                }
            }
            Some(RowKind::Project(w, p)) => {
                // A project is not a place an agent can live; its first
                // environment is the only unambiguous reading, and the status
                // line says which one it picked.
                let env = self.tree.get(w)?.projects.get(p)?.envs.first()?;
                let name = env.name.clone();
                self.target = self.target_at((w, p, 0));
                self.status = format!("New agent in {name}");
                match prompt {
                    Some(p) => self.launch_prompted(None, true, Some(p)),
                    None => self.launch(None, true),
                }
            }
            _ if self.target.is_some() => match prompt {
                Some(p) => self.launch_prompted(None, true, Some(p)),
                None => self.launch(None, true),
            },
            _ => {
                self.start_target_pick();
                self.status = "Pick where this should run".into();
                None
            }
        }
    }

    /// Keys while the ⌥n picker is up: choose the agent, then the same new
    /// session `n` would have made where the cursor points.
    fn on_key_harness_pick(&mut self, key: KeyEvent) -> Option<Effect> {
        let cursor = self.harness_pick?;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.harness_pick = Some(cursor.saturating_sub(1));
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.harness_pick = Some((cursor + 1).min(HARNESSES.len() - 1));
                None
            }
            KeyCode::Enter => {
                self.harness = cursor.min(HARNESSES.len() - 1);
                self.harness_pick = None;
                self.screen = Screen::Manage;
                // Picked from an agent's row: a new session on that box, not
                // a new box.
                match self.harness_pick_agent.take() {
                    Some(agent_id) => self.launch(Some(agent_id), true),
                    None => self.launch_new_agent(),
                }
            }
            KeyCode::Esc => {
                self.harness_pick = None;
                self.harness_pick_agent = None;
                self.screen = Screen::Manage;
                None
            }
            _ => None,
        }
    }

    /// Keys while the ⌥p composer is up: the launcher prompt box's contract —
    /// type, shift+tab cycles the agent, enter sends, esc closes. And the
    /// launcher box's shell contract too: typing goes inert, and enter
    /// launches the session bare, since a shell has nothing to hand a prompt
    /// to.
    fn on_key_manage_prompt(&mut self, key: KeyEvent) -> Option<Effect> {
        let mut draft = self.manage_prompt.take()?;
        match key.code {
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !self.shell_selected() =>
            {
                draft.push(c);
                self.manage_prompt = Some(draft);
                None
            }
            KeyCode::Backspace if !self.shell_selected() => {
                draft.pop();
                self.manage_prompt = Some(draft);
                None
            }
            KeyCode::BackTab => {
                self.harness = (self.harness + 1) % HARNESSES.len();
                self.manage_prompt = Some(draft);
                None
            }
            // Same newline chord as the launcher's box — the two are the
            // same control. ALT rides along for the same reason it does
            // there: most terminals deliver ⇧enter as `ESC CR`, alt+enter.
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
                    && !self.shell_selected() =>
            {
                draft.push('\n');
                self.manage_prompt = Some(draft);
                None
            }
            KeyCode::Enter if self.shell_selected() => {
                self.screen = Screen::Manage;
                self.new_here_prompted(None)
            }
            KeyCode::Enter if !draft.trim().is_empty() => {
                self.screen = Screen::Manage;
                self.new_here_prompted(Some(draft.trim().to_string()))
            }
            // Enter on an empty draft is a slip, not a request for an
            // unprompted session; the box stays up.
            KeyCode::Enter => {
                self.manage_prompt = Some(draft);
                None
            }
            KeyCode::Esc => {
                self.screen = Screen::Manage;
                None
            }
            _ => {
                self.manage_prompt = Some(draft);
                None
            }
        }
    }

    /// Start a lifecycle action on the agent under the cursor.
    ///
    /// Refuses the no-ops rather than sending them: waking a running agent or
    /// sleeping a sleeping one would spend a round-trip to change nothing, and
    /// the status line explains why the key did nothing.
    fn agent_op(&mut self, op: AgentOp) -> Option<Effect> {
        let row = self.selected_row()?;
        // A session belongs to an agent, so acting on it from a session row is
        // unambiguous — and it is where the cursor usually is.
        let (w, p, e, a) = match row.kind {
            RowKind::Agent(w, p, e, a) | RowKind::Session(w, p, e, a, _) => (w, p, e, a),
            _ => {
                self.status = "Select an agent first".into();
                return None;
            }
        };
        let env = &self.tree[w].projects[p].envs[e];
        let agent = env.agents_vec().get(a)?;
        if self.ops.contains_key(&agent.id) {
            return None;
        }
        match (op, agent.status.as_str()) {
            (AgentOp::Sleep, "sleeping") => {
                self.status = format!("{} is already asleep", agent.name);
                return None;
            }
            (AgentOp::Wake, "running") => {
                self.status = format!("{} is already running", agent.name);
                return None;
            }
            _ => {}
        }

        let pending = PendingConfirm {
            op,
            agent_id: agent.id.clone(),
            agent_name: agent.name.clone(),
            environment_id: env.id.clone(),
        };
        if op == AgentOp::Delete {
            self.confirm = Some(pending);
            return None;
        }
        self.ops
            .insert(pending.agent_id.clone(), op.pending_label());
        Some(Effect::Agent {
            op,
            agent_id: pending.agent_id,
            environment_id: pending.environment_id,
        })
    }

    /// A kill finished. The row is refetched either way; this only clears the
    /// label and reports a failure.
    pub fn session_killed(&mut self, session_name: &str, error: Option<String>) {
        match error {
            // It is still there, so put it back rather than leave a row hidden
            // for a session that is very much alive.
            Some(err) => {
                self.ending.remove(session_name);
                self.status = format!("Couldn't end {session_name}: {err}");
            }
            None => self.status = format!("Ended {session_name}"),
        }
    }

    /// Every environment that has not been fetched, as load requests.
    ///
    /// The whole-account scan, which is what `shift+r` asks for. Startup no
    /// longer does this: it is one request per environment, so it costs a large
    /// account hundreds of them. As a deliberate action the cost is the user's
    /// to spend, and a rate limit stops it partway rather than pressing on.
    pub fn scan_environments(&mut self) -> Vec<Effect> {
        let mut out = Vec::new();
        for w in 0..self.tree.len() {
            for p in 0..self.tree[w].projects.len() {
                for e in 0..self.tree[w].projects[p].envs.len() {
                    let env = &mut self.tree[w].projects[p].envs[e];
                    if env.agents != Load::NotLoaded {
                        continue;
                    }
                    env.agents = Load::Loading;
                    out.push(Effect::LoadAgents {
                        environment_id: env.id.clone(),
                        path: (w, p, e),
                    });
                }
            }
        }
        out
    }

    /// The environments a refresh should ask about again, one request each.
    ///
    /// Only for callers that cannot use the account-wide query — see
    /// [`Self::account_query_unavailable`]. Scoped to environments that already
    /// have an answer: those are the ones with rows on screen that could now be
    /// wrong. Environments that have never loaded are left to `shift+r`, since
    /// asking about all of them is the request-per-environment cost this TUI is
    /// careful about. One still `Loading` is already on its way.
    pub fn environments_to_refresh(&self) -> Vec<Effect> {
        let mut out = Vec::new();
        for (w, ws) in self.tree.iter().enumerate() {
            for (p, project) in ws.projects.iter().enumerate() {
                for (e, env) in project.envs.iter().enumerate() {
                    if !matches!(env.agents, Load::Loaded(_) | Load::Failed(_)) {
                        continue;
                    }
                    out.push(Effect::LoadAgents {
                        environment_id: env.id.clone(),
                        path: (w, p, e),
                    });
                }
            }
        }
        out
    }

    /// A background fetch was refused for rate limiting.
    ///
    /// Anything still in flight is put back to "not loaded" rather than left
    /// spinning: the request is not coming, and a row that never resolves reads
    /// as a hung UI. Expanding it later retries, which is the right amount of
    /// work for the user to have to do.
    pub fn rate_limited(&mut self, retry_after_secs: Option<u64>) {
        // A rate-limited batch is abandoned, so its in-flight marks would
        // otherwise stick and stop the fast poll forever.
        self.thread_polls.clear();
        for ws in &mut self.tree {
            for project in &mut ws.projects {
                for env in &mut project.envs {
                    if env.agents == Load::Loading {
                        env.agents = Load::NotLoaded;
                    }
                    if let Load::Loaded(agents) = &mut env.agents {
                        for agent in agents {
                            if agent.sessions == LoadSessions::Loading {
                                agent.sessions = LoadSessions::NotLoaded;
                            }
                        }
                    }
                }
            }
        }
        // Hold off the automatic refresh for as long as we were told, or a
        // minute when we were told nothing: answering a 429 by polling on
        // schedule is how a rate limit becomes a longer one. A keypress still
        // asks — that is the user's request to spend, and by then the window may
        // have passed.
        self.refresh_paused_until = Some(
            std::time::Instant::now()
                + std::time::Duration::from_secs(retry_after_secs.unwrap_or(60)),
        );
        self.refreshing = false;
        // The toast below is the answer to whatever asked; a pending "up to
        // date" line must not be claimed by the next refresh to succeed.
        self.refresh_announce = false;
        self.toast_error(match retry_after_secs {
            Some(secs) if secs > 90 => format!(
                "Rate limited — try again in about {} minutes",
                secs.div_ceil(60)
            ),
            Some(secs) => format!("Rate limited — try again in {secs}s"),
            None => "Rate limited by the API".to_string(),
        });
        self.status = "Rate limited. Open a row to load it, or r to retry".into();
    }

    /// A lifecycle mutation came back.
    ///
    /// Accepted is not arrived: for sleep and wake the platform reports the old
    /// status until the VM actually gets there, so the label stays up and the
    /// environment is polled until it does. An error ends it immediately —
    /// there is nothing to wait for.
    pub fn agent_op_finished(
        &mut self,
        agent_id: &str,
        environment_id: &str,
        op: AgentOp,
        error: Option<String>,
    ) {
        if let Some(err) = error {
            self.ops.remove(agent_id);
            self.watching.remove(agent_id);
            self.status = err;
            return;
        }
        let (want, patience) = match op {
            AgentOp::Wake => ("running", WAKE_PATIENCE),
            AgentOp::Sleep => ("sleeping", SLEEP_PATIENCE),
            // The row is about to disappear; there is no state to settle into.
            AgentOp::Delete => {
                self.ops.remove(agent_id);
                self.watching.remove(agent_id);
                return;
            }
        };
        self.watching.insert(
            agent_id.to_string(),
            AgentWatch {
                want,
                environment_id: environment_id.to_string(),
                until: std::time::Instant::now() + patience,
            },
        );
    }

    /// Is anything still on its way? Drives the polling in the event loop.
    pub fn watching_agents(&self) -> bool {
        !self.watching.is_empty()
    }

    /// Agents this TUI holds an open, unfinished pane on, and how long the
    /// youngest such pane has been open.
    ///
    /// An ended pane does not count: the connection is gone, so it is no longer
    /// evidence of anything. The age is what lets [`displayed_status`] tell the
    /// projection's normal lag apart from an agent that is stuck.
    fn agents_with_live_panes(&self) -> std::collections::HashMap<&str, std::time::Duration> {
        let mut open: std::collections::HashMap<&str, std::time::Duration> =
            std::collections::HashMap::new();
        for session in self.sessions.iter().filter(|s| !s.ended()) {
            let age = session.open_for();
            open.entry(session.agent_id.as_str())
                .and_modify(|held| {
                    if age < *held {
                        *held = age;
                    }
                })
                .or_insert(age);
        }
        open
    }

    /// Ask again. One environment per tick — in practice there is one agent
    /// waking at a time, and the loop comes back here in a moment anyway.
    pub fn watch_tick(&mut self) -> Option<Effect> {
        self.give_up_on_stale_watches();
        let environment_id = self.watching.values().next()?.environment_id.clone();
        self.reveal_environment(&environment_id)
    }

    /// How long until the tree should ask the platform for everything again, or
    /// `None` while an automatic refresh would be wrong.
    ///
    /// The loop arms a timer with this, so `None` means an idle TUI goes back to
    /// blocking on the keyboard rather than waking on a schedule to decide there
    /// was nothing to do.
    ///
    /// What suppresses it, and why:
    ///
    /// - A refresh already in flight, or a rate limit we were told to wait out.
    /// - A launch: the pipeline is mid-provision and reports its own progress,
    ///   and its environment gets refetched when the session opens.
    /// - A wake or a sleep in progress: [`WATCH_TICK`] is already asking that
    ///   environment every 1.5s, which is both faster and narrower.
    /// - A card owning the screen — first-run setup, the settings card, the ssh
    ///   key question, a delete confirmation. None of them show the tree, and
    ///   rows moving under a `y/N` question is how the wrong thing gets deleted.
    /// - A maximized pane, where the tree is folded away entirely. It refreshes
    ///   when it comes back.
    pub fn auto_refresh_in(&self) -> Option<std::time::Duration> {
        if self.refreshing
            || self.loading.active
            || self.watching_agents()
            || self.confirm.is_some()
            || self.ssh_gate.is_some()
            || self.wizard.is_some()
            || self.settings.is_some()
            || self.pane_is_full()
        {
            return None;
        }
        let now = std::time::Instant::now();
        if let Some(until) = self.refresh_paused_until {
            if until > now {
                return Some(until - now);
            }
        }
        let Some(last) = self.last_refresh else {
            // Nothing has refreshed yet, which means the startup fetch is still
            // on its way; give it the interval before asking again.
            return Some(AUTO_REFRESH_EVERY);
        };
        Some(AUTO_REFRESH_EVERY.saturating_sub(now.duration_since(last)))
    }

    /// Is a refresh due right now? The timer can fire early — it is re-armed
    /// from a shortened remainder every time round the loop — so the decision is
    /// made here rather than by the fact of waking up.
    pub fn auto_refresh_due(&self) -> bool {
        self.auto_refresh_in() == Some(std::time::Duration::ZERO)
    }

    /// Time until the watched threads should be re-asked about — the fast lane
    /// behind the sidebar's live labels, much tighter than the account
    /// refresh: a prompt lands and its row should say so in seconds, not at
    /// the 25s tick. Narrow by construction — [`Self::sessions_to_refresh`]
    /// names only running agents someone can see — so the cost is one
    /// per-agent query per tick, and the gate transcript dials behind it are
    /// cached until a thread actually reports something new.
    ///
    /// `None` when there is nothing to poll for, or a rate limit said to wait.
    pub fn thread_refresh_in(&self) -> Option<std::time::Duration> {
        if self.screen != Screen::Manage || self.sessions_to_refresh().is_empty() {
            return None;
        }
        let now = std::time::Instant::now();
        if let Some(until) = self.refresh_paused_until
            && until > now
        {
            return Some(until - now);
        }
        let Some(last) = self.last_thread_refresh else {
            return Some(THREAD_REFRESH_EVERY);
        };
        Some(THREAD_REFRESH_EVERY.saturating_sub(now.duration_since(last)))
    }

    /// Note that the fast thread poll ran, arming the next tick.
    pub fn thread_refresh_started(&mut self) {
        self.last_thread_refresh = Some(std::time::Instant::now());
    }

    /// The fast tick's work: what [`Self::sessions_to_refresh`] names, minus
    /// agents whose previous ask is still in flight, marked as in flight.
    pub fn threads_to_poll(&mut self) -> Vec<Effect> {
        self.thread_refresh_started();
        let effects: Vec<Effect> = self
            .sessions_to_refresh()
            .into_iter()
            .filter(|effect| match effect {
                Effect::LoadSessions { agent_id, .. } => !self.thread_polls.contains(agent_id),
                _ => true,
            })
            .collect();
        for effect in &effects {
            if let Effect::LoadSessions { agent_id, .. } = effect {
                self.thread_polls.insert(agent_id.clone());
            }
        }
        effects
    }

    /// Note that a refresh has started, so the next automatic one is a full
    /// interval away and a second one cannot be started on top of it.
    pub fn refresh_started(&mut self) {
        self.refreshing = true;
        self.last_refresh = Some(std::time::Instant::now());
    }

    /// A refresh finished, whatever it found.
    pub fn refresh_finished(&mut self) {
        self.refreshing = false;
        self.last_refresh = Some(std::time::Instant::now());
    }

    /// Report a finished account-wide refresh — but only when someone asked for
    /// one.
    ///
    /// A keypress needs an answer: silence after ⌥r is indistinguishable from a
    /// chord that isn't bound. The automatic refresh needs the opposite, and the
    /// tree itself is its report — a status line rewritten every 25s would
    /// scrub whatever was there, and turn an idle screen into something that
    /// looks busy.
    pub fn refreshed(&mut self, agents: usize) {
        if !std::mem::take(&mut self.refresh_announce) {
            return;
        }
        self.status = match agents {
            0 => "Up to date — no agents on this account".into(),
            1 => "Up to date — 1 agent".into(),
            n => format!("Up to date — {n} agents"),
        };
    }

    /// Stop waiting for what is not coming, and put the real status back rather
    /// than leaving "waking…" on the row forever.
    fn give_up_on_stale_watches(&mut self) {
        let now = std::time::Instant::now();
        let expired: Vec<String> = self
            .watching
            .iter()
            .filter(|(_, watch)| watch.until <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.watching.remove(&id);
            self.ops.remove(&id);
            self.status = "That agent is taking longer than expected — r refreshes".into();
        }
    }

    /// Clear the watch for any agent that has arrived, or that has gone.
    fn settle_watched_agents(&mut self) {
        if self.watching.is_empty() {
            return;
        }
        let mut arrived: Vec<String> = Vec::new();
        for (id, watch) in &self.watching {
            let status = self.status_of_agent(id);
            match status {
                // Gone from the list entirely: deleted elsewhere, or never
                // there. Either way nothing is coming.
                None => arrived.push(id.clone()),
                Some(status) if status == watch.want => arrived.push(id.clone()),
                Some(_) => {}
            }
        }
        for id in arrived {
            self.watching.remove(&id);
            self.ops.remove(&id);
        }
    }

    /// An agent's status as the tree currently has it, wherever it lives.
    fn status_of_agent(&self, agent_id: &str) -> Option<String> {
        self.tree.iter().find_map(|ws| {
            ws.projects.iter().find_map(|project| {
                project.envs.iter().find_map(|env| {
                    env.agents_vec()
                        .iter()
                        .find(|agent| agent.id == agent_id)
                        .map(|agent| agent.status.clone())
                })
            })
        })
    }

    fn set_expanded(&mut self, kind: RowKind, open: bool) -> Option<Effect> {
        match kind {
            // A group is always open; there is nothing to do in either
            // direction, and quietly folding agents away would be the old
            // tree's problem reintroduced on purpose.
            RowKind::OtherProjects => {
                self.others_expanded = Some(open);
            }
            RowKind::Workspace(w) => {
                self.tree.get_mut(w)?.expanded = open;
            }
            RowKind::Project(w, p) => {
                self.tree.get_mut(w)?.projects.get_mut(p)?.expanded = open;
            }
            RowKind::Environment(w, p, e) => {
                if open {
                    let effect = self.expand_env((w, p, e));
                    self.clamp_cursor();
                    return effect;
                }
                self.tree
                    .get_mut(w)?
                    .projects
                    .get_mut(p)?
                    .envs
                    .get_mut(e)?
                    .expanded = false;
            }
            RowKind::Agent(w, p, e, a) if open => {
                let effect = self.set_agent_expanded((w, p, e, a), true);
                self.clamp_cursor();
                return effect;
            }
            // Collapsing walks out one level at a time: an expanded agent
            // closes itself, and only a closed one closes its environment.
            RowKind::Agent(w, p, e, a)
                if self
                    .tree
                    .get(w)
                    .and_then(|ws| ws.projects.get(p))
                    .and_then(|proj| proj.envs.get(e))
                    .map(|env| env.agents_vec().get(a).is_some_and(|ag| ag.expanded))
                    .unwrap_or(false) =>
            {
                self.set_agent_expanded((w, p, e, a), false);
                self.clamp_cursor();
                return None;
            }
            // A session row collapses the agent it belongs to, and the cursor
            // follows it up.
            RowKind::Session(w, p, e, a, _) if !open => {
                self.set_agent_expanded((w, p, e, a), false);
                if let Some(i) = self
                    .rows()
                    .iter()
                    .position(|r| r.kind == RowKind::Agent(w, p, e, a))
                {
                    self.cursor = i;
                }
                self.clamp_cursor();
                return None;
            }
            // A closed agent is as far out as `h` goes: the threads list is
            // flat, so there is no parent row above it to walk to.
            _ => {}
        }
        self.clamp_cursor();
        None
    }
}

/// A rule between the tree's sections: under the pinned New Session row, and
/// between the agent groups and the projects tail — so each section reads as
/// its own thing rather than as one more group.
fn separator_row() -> Row {
    Row {
        depth: 0,
        kind: RowKind::Separator,
        label: String::new(),
        note: String::new(),
        status: None,
        expanded: None,
        dimmed: true,
    }
}

fn note_row(w: usize, p: usize, e: usize, depth: usize, text: &str) -> Row {
    Row {
        depth,
        kind: RowKind::Note(w, p, e),
        label: text.to_string(),
        note: String::new(),
        status: None,
        expanded: None,
        dimmed: false,
    }
}

/// Display order for a group's agents: running first, waking next, sleeping
/// last, then by name. Indices for the same reason as [`App::agent_groups`].
///
/// Ranks on [`displayed_status`], not the raw one, so an agent with a pane open
/// does not sit in the waking band while its row reads `running`.
fn sorted_agents(
    agents: &[Agent],
    attached: &std::collections::HashMap<&str, std::time::Duration>,
) -> Vec<usize> {
    let rank = |agent: &Agent| match displayed_status(agent, attached) {
        "running" => 0,
        "starting" => 1,
        _ => 2,
    };
    let mut order: Vec<usize> = (0..agents.len()).collect();
    order.sort_by(|&a, &b| {
        rank(&agents[a]).cmp(&rank(&agents[b])).then_with(|| {
            agents[a]
                .name
                .to_lowercase()
                .cmp(&agents[b].name.to_lowercase())
        })
    });
    order
}

/// The status to show for an agent, which is not always the one the platform
/// last reported.
///
/// A pane we are attached to and that has not ended is direct evidence the
/// agent is up — there is a shell running on it. The platform's status lags
/// that: a shell routes as soon as the container exists, while `RUNNING`
/// additionally waits on route publication, a projection hop, and a poll
/// interval, which is exactly why the launcher stopped gating attach on it
/// (#1098). So a freshly created agent spends its first seconds reported as
/// `starting` while its session is already open and taking keystrokes, and the
/// tree calls that "waking" — about an agent the user is typing into.
///
/// Only `starting` is overridden, and only for [`STARTING_GRACE`] after the
/// pane opened. Two reasons for the bound:
///
/// - A pane can outlive the thing it is attached to, so `sleeping`, `crashed`,
///   `failed` or `deleting` alongside a live pane is news the user needs
///   rather than a lag to paper over.
/// - The lag is small. Measured over ten `railway code --claude --new` runs,
///   status reached RUNNING 5.3–14.2s after create — before the harness
///   answered, every time. An agent still `starting` well past that is not
///   lagging, it is stuck, and the row saying `running` would hide the only
///   signal the user has. That case is real: it is what prompted this fix.
///
/// So the override covers the window the lag actually lives in, and past it
/// the platform's own answer stands.
fn displayed_status<'a>(
    agent: &'a Agent,
    attached: &std::collections::HashMap<&str, std::time::Duration>,
) -> &'a str {
    let open_for = attached.get(agent.id.as_str());
    match open_for {
        Some(open_for) if agent.status == "starting" && *open_for < STARTING_GRACE => "running",
        _ => &agent.status,
    }
}

/// How long after a pane opens a `starting` status is still treated as the
/// projection catching up. Generously past the measured lag (worst observed
/// 14.2s from create, and a pane opens partway into that), so a normal boot is
/// never shown as stuck — but bounded, so a stuck one stops being shown as
/// healthy.
const STARTING_GRACE: std::time::Duration = std::time::Duration::from_secs(45);

/// Display order for a workspace's projects: the ones with agents first, then
/// alphabetically within each group.
///
/// Returns indices rather than sorting the tree, so `RowKind` keeps pointing at
/// the same project as counts arrive and the order shifts under it — the cursor
/// is restored by identity, not by position.
fn sorted_projects(ws: &WorkspaceNode, default_project: Option<&str>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..ws.projects.len()).collect();
    order.sort_by(|a, b| {
        let (pa, pb) = (&ws.projects[*a], &ws.projects[*b]);
        let is_default = |p: &ProjectNode| Some(p.id.as_str()) == default_project;
        // The default first, then the ones with agents, then alphabetical.
        is_default(pb)
            .cmp(&is_default(pa))
            .then_with(|| (project_agent_count(pb) > 0).cmp(&(project_agent_count(pa) > 0)))
            .then_with(|| pa.name.to_lowercase().cmp(&pb.name.to_lowercase()))
    });
    order
}

/// Agents known to be in a project, across the environments that have answered.
fn project_agent_count(project: &ProjectNode) -> usize {
    project
        .envs
        .iter()
        .filter_map(|env| match &env.agents {
            Load::Loaded(agents) => Some(agents.len()),
            _ => None,
        })
        .sum()
}

/// Map a keystroke to an Alt-chord action letter.
///
/// Two ways in. The real one is `KeyModifiers::ALT`, which is what a terminal
/// sends when Option is configured as Meta (iTerm2's default; Terminal.app
/// needs "Use Option as Meta key"). Without that, macOS composes Option+letter
/// into a character, so those are matched as well — except Option+N, which is a
/// dead key for `~` and emits nothing at all until the next keystroke. That one
/// is unreachable without Meta, which is why every card also keeps its bare
/// letter while the cards have focus.
///
/// The brackets are the non-letters, and they are not equals. `ESC ]` is OSC,
/// which terminals effectively never send, so under Meta ⌥] is safe
/// everywhere. ⌥[ under Meta is the two bytes `ESC [` — byte-identical to the
/// CSI prefix that starts every arrow key — so a legacy terminal can never
/// deliver it; the parser eats the bytes as an unfinished escape sequence and
/// no key event exists to match, and a terminal or window manager that binds
/// ⌥[ itself never lets it through at all. That is why the ADVERTISED chords
/// are the shifted pair: ⌥⇧[ is `ESC {`, which no escape sequence begins with
/// and nothing conventionally binds, so it arrives everywhere Meta does. The
/// unshifted forms stay in the table as silent aliases for the terminals that
/// can say them unambiguously: under the kitty keyboard protocol (pushed at
/// startup) they arrive as genuine ALT+bracket events, and composed-mode
/// macOS folds both shift states into the same curly quotes anyway.
/// Everywhere else the unshifted chord is simply absent — it can go dead, but
/// never misfire.
fn alt_chord(key: &KeyEvent) -> Option<char> {
    const ACTIONS: &[char] = &['f', 's', 'n', 'p', 'r', ']', '['];
    if key.modifiers.contains(KeyModifiers::ALT) {
        if let KeyCode::Char(c) = key.code {
            let c = match c.to_ascii_lowercase() {
                // ⌥⇧[ arrives as ALT+`{` where shift reaches us as the
                // character; fold the braces onto the bracket chords, the
                // same way the letters fold their capitals.
                '{' => '[',
                '}' => ']',
                c => c,
            };
            return ACTIONS.contains(&c).then_some(c);
        }
        return None;
    }
    // macOS Option-composed characters, for the terminals that send them.
    // ⌥n has no composed form: Option+N is the dead key for `~` and emits
    // nothing until the next keystroke, so without Meta it simply doesn't
    // exist — the same trade ⌥[ documents above.
    match key.code {
        KeyCode::Char('ƒ') => Some('f'),
        KeyCode::Char('ß') => Some('s'),
        KeyCode::Char('π') => Some('p'),
        // Option+r composes to ®, Option+shift+R to ‰ — one chord as far as
        // anyone pressing it is concerned, the way the bracket pair below folds
        // its shifted forms and the Meta path folds its capitals.
        KeyCode::Char('®') | KeyCode::Char('‰') => Some('r'),
        // Option+] composes to a left curly single quote, Option+shift+] to
        // the right one; Option+[ does the same with the double quotes. Each
        // pair is one chord as far as anyone pressing it is concerned,
        // matching how the letters fold their shifted forms.
        KeyCode::Char('\u{2018}') | KeyCode::Char('\u{2019}') => Some(']'),
        KeyCode::Char('\u{201C}') | KeyCode::Char('\u{201D}') => Some('['),
        _ => None,
    }
}

/// Fold a fresh agent list into the one on screen.
///
/// The platform owns membership, order and status; the TUI owns whether a row
/// is open and what its sessions are. Replacing the vector wholesale — which is
/// what every refresh used to do — threw the second half away, because
/// `fetch_agents` can only build rows that are collapsed with no sessions. The
/// visible cost was an expanded agent folding shut on every `r`, and on every
/// 1.5s poll tick while an agent woke; the invisible one was a session list
/// dropped and immediately refetched, one request per agent. Carrying it across
/// is what makes refreshing cheap enough to do on a timer.
///
/// Matched by id, so a rename or a status change lands and an agent that has
/// gone from the fresh list is gone from the tree.
fn merge_agents(previous: Vec<Agent>, fresh: Vec<Agent>) -> Vec<Agent> {
    let mut kept: HashMap<String, (bool, LoadSessions)> = previous
        .into_iter()
        .map(|agent| (agent.id, (agent.expanded, agent.sessions)))
        .collect();
    fresh
        .into_iter()
        .map(|mut agent| {
            if let Some((expanded, sessions)) = kept.remove(&agent.id) {
                agent.expanded = expanded;
                // Sessions live on the machine, and a machine that is not
                // running has none — carrying the old list across a sleep
                // kept rows for sessions that no longer exist, forever,
                // because the refresh cadence only asks running agents.
                // NotLoaded rather than an empty list: when the agent runs
                // again the prefetch re-asks, and anything that survived the
                // wake comes back on its own. A pane we are still attached
                // to is folded back in by `adopt_pane_sessions`.
                agent.sessions = if agent.status == "running" {
                    sessions
                } else {
                    LoadSessions::NotLoaded
                };
            }
            agent
        })
        .collect()
}

impl EnvNode {
    fn agents_vec(&self) -> &[Agent] {
        match &self.agents {
            Load::Loaded(a) => a,
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn tree() -> Vec<WorkspaceNode> {
        vec![WorkspaceNode {
            id: "ws_1".into(),
            name: "Railway".into(),
            expanded: false,
            projects: vec![ProjectNode {
                id: "proj_1".into(),
                name: "devtools".into(),
                expanded: false,
                envs: vec![
                    EnvNode {
                        id: "env_prod".into(),
                        name: "production".into(),
                        expanded: false,
                        agents: Load::NotLoaded,
                    },
                    EnvNode {
                        id: "env_stg".into(),
                        name: "staging".into(),
                        expanded: false,
                        agents: Load::NotLoaded,
                    },
                ],
            }],
        }]
    }

    fn app() -> App {
        App::new(tree(), None, Some("claude"), None, None, true)
    }

    /// One workspace, four projects, deliberately out of alphabetical order.
    fn ordering_app() -> App {
        let env = |id: &str, name: &str| EnvNode {
            id: id.into(),
            name: name.into(),
            expanded: false,
            agents: Load::NotLoaded,
        };
        let project = |id: &str, name: &str| ProjectNode {
            id: id.into(),
            name: name.into(),
            expanded: false,
            envs: vec![env(&format!("{id}-prod"), "production")],
        };
        let tree = vec![WorkspaceNode {
            id: "ws_1".into(),
            name: "Railway".into(),
            expanded: true,
            projects: vec![
                project("p1", "zebra"),
                project("p2", "Alpha"),
                project("p3", "mono"),
                project("p4", "beta"),
            ],
        }];
        App::new(tree, None, Some("claude"), None, None, true)
    }

    fn project_order(a: &App) -> Vec<String> {
        a.rows()
            .into_iter()
            .filter(|r| matches!(r.kind, RowKind::Project(..)))
            .map(|r| r.label)
            .collect()
    }

    /// A target-less app with nothing loaded yet: the pinned launcher first,
    /// ruled off from the rest, then — with no agents to lead with — the
    /// projects tail as the whole tree, open, behind the search hint.
    #[test]
    fn opens_with_the_projects_tail_open() {
        let a = app();
        let rows = a.rows();
        assert_eq!(rows.len(), 5, "{rows:#?}");
        assert_eq!(rows[0].label, "New Agent");
        assert!(rows[0].selectable());
        assert!(matches!(rows[1].kind, RowKind::Separator));
        assert_eq!(rows[2].label, "looking for cloud agents…");
        assert!(!rows[2].selectable());
        assert_eq!(rows[3].label, "projects");
        assert_eq!(rows[4].label, "devtools");
        // The cursor starts on the launcher: the prompt is the first thing.
        assert_eq!(a.cursor, 0);
    }

    #[test]
    fn expanding_an_environment_requests_its_agents_once() {
        let mut a = app();
        a.screen = Screen::Manage;
        a.cursor = 4; // devtools
        assert_eq!(a.on_key(key(KeyCode::Right)), None);
        a.cursor = 5; // production
        let effect = a.on_key(key(KeyCode::Right)).unwrap();
        assert_eq!(
            effect,
            Effect::LoadAgents {
                environment_id: "env_prod".into(),
                path: (0, 0, 0)
            }
        );
        // Collapsing and re-expanding must not refetch — the result is cached.
        a.on_key(key(KeyCode::Left));
        assert_eq!(a.on_key(key(KeyCode::Right)), None);
    }

    #[test]
    fn a_loading_environment_shows_a_note_that_cannot_be_selected() {
        let mut a = app();
        a.screen = Screen::Manage;
        a.cursor = 4; // devtools
        a.on_key(key(KeyCode::Right));
        a.cursor = 5; // production
        a.on_key(key(KeyCode::Right));
        let rows = a.rows();
        let note = rows.iter().find(|r| r.label == "loading…").unwrap();
        assert!(!note.selectable());

        // Down from the environment skips the note and lands on staging.
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.selected_row().unwrap().label, "staging");
    }

    fn loaded_app() -> App {
        let mut a = app();
        a.screen = Screen::Manage;
        a.tree[0].projects[0].expanded = true;
        a.tree[0].projects[0].envs[0].expanded = true;
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![agent("ca_1", "nimble-otter", "running")]),
        );
        a
    }

    #[test]
    fn enter_on_an_agent_connects_to_that_agent_and_retargets() {
        let mut a = loaded_app();
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        let Effect::Launch(req) = a.on_key(key(KeyCode::Enter)).unwrap() else {
            panic!("expected a launch");
        };
        assert_eq!(req.agent_id.as_deref(), Some("ca_1"));
        assert!(!req.force_new);
        assert_eq!(req.environment_id, "env_prod");
        assert_eq!(req.project_id, "proj_1");
        assert_eq!(a.target.unwrap().label(), "devtools/production");
    }

    /// Connecting to an agent that already has a running session reattaches
    /// to it, rather than minting another durable session — which, for a
    /// harness like railway's, is just another window onto the same
    /// conversation under a new name.
    #[test]
    fn connecting_to_an_agent_reattaches_to_its_running_session() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]);
        }
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        let effect = a.on_key(key(KeyCode::Enter));
        assert!(
            matches!(
                effect,
                Some(Effect::Reattach { ref session_name, .. }) if session_name == "claude-one"
            ),
            "a plain connect must not mint a session: {effect:?}"
        );

        // A drafted prompt is new work, and does get a session of its own.
        a.prompt = "fix the tests".into();
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        let Some(Effect::Launch(req)) = a.on_key(key(KeyCode::Enter)) else {
            panic!("a prompt seeds a fresh session");
        };
        assert_eq!(req.prompt.as_deref(), Some("fix the tests"));
        assert!(req.wants_new_session());
    }

    /// A tab names the session, not the agent: the seeded task once the
    /// platform lists it, the durable name until then.
    #[test]
    fn tabs_are_named_by_their_sessions() {
        let mut a = loaded_app();
        let mut pane = session("ca_1", "nimble-otter");
        pane.durable_name = "railway-t5soqz".into();
        a.attach_session(pane, "ca_1".into());
        assert_eq!(a.session_tab_label(0), "railway-t5soqz", "not yet listed");

        a.sessions_loaded(
            (0, 0, 0, 0),
            "ca_1",
            Ok(vec![ConsoleSession {
                name: "merry-daisy-ld9".into(),
                kind: "SHELL".into(),
                command: Some(
                    "export RAILWAY_CODE_AUTOSTARTED=1; railway-agent-tui --session \
                     \"$RAILWAY_DURABLE_SESSION_NAME\" 'ship the release notes today'; printf 'x'"
                        .into(),
                ),
                running: true,
                attached: true,
                created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0),
                    snapshot: None,
            }]),
        );
        assert_eq!(
            a.session_tab_label(0),
            "railway-ld9",
            "the tab takes the listed name, folded short"
        );
    }

    /// The pane's title reads like a project reference — project / agent /
    /// session — using the listed session's short name once the platform has
    /// it, and degrading gracefully while it doesn't.
    #[test]
    fn the_pane_title_is_a_project_path() {
        let mut a = loaded_app();
        let mut pane = session("ca_1", "nimble-otter");
        pane.durable_name = "merry-daisy-ld9".into();
        // Before the listing lands: the durable name stands in.
        assert_eq!(
            a.pane_breadcrumb(&pane),
            "devtools / nimble-otter / merry-daisy-ld9"
        );

        a.sessions_loaded(
            (0, 0, 0, 0),
            "ca_1",
            Ok(vec![ConsoleSession {
                name: "merry-daisy-ld9".into(),
                kind: "SHELL".into(),
                command: Some(
                    "export RAILWAY_CODE_AUTOSTARTED=1; railway-agent-tui --session \
                     \"$RAILWAY_DURABLE_SESSION_NAME\" 'ship it'; printf 'x'"
                        .into(),
                ),
                running: true,
                attached: true,
                created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0),
                    snapshot: None,
            }]),
        );
        assert_eq!(
            a.pane_breadcrumb(&pane),
            "devtools / nimble-otter / railway-ld9",
            "the listed session's short name takes over"
        );

        // An agent the tree doesn't know: no project leg, no hole.
        let stray = session("ca_unknown", "wandering-ox");
        assert_eq!(a.pane_breadcrumb(&stray), "wandering-ox / test");
    }

    /// The maximized header's tabs switch panes on a click.
    #[test]
    fn clicking_a_header_tab_switches_the_pane() {
        let mut a = with_sessions(&["ca_1", "ca_2"]);
        a.maximized = true;
        a.focus = ManageFocus::Session;
        a.active = Some(0);
        a.panes.tabs[0] = PaneBox {
            x: 24,
            y: 1,
            w: 10,
            h: 1,
        };
        a.panes.tabs[1] = PaneBox {
            x: 35,
            y: 1,
            w: 10,
            h: 1,
        };
        assert_eq!(a.on_mouse(MouseAction::Down, 38, 1), None);
        assert_eq!(a.active, Some(1), "the click picked the second tab");
        assert!(a.maximized, "and the layout stayed put");
    }

    /// One click on a session's row selects it and keeps the keyboard in the
    /// tree — the sidebar is threads now, and a single click that handed the
    /// keyboard over made the list unbrowsable from a focused pane. A double
    /// click steps into the pane; ^o (or Esc) is the way back, after which x
    /// acts on the highlighted row instead of typing into it.
    #[test]
    fn clicking_the_list_takes_the_keyboard_back_from_a_session() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]);
        }
        let mut pane = session("ca_1", "nimble-otter");
        pane.durable_name = "claude-one".into();
        a.sessions = vec![pane];
        a.active = Some(0);
        a.focus = ManageFocus::Session;
        a.panes = panes_fixture();

        let row = a
            .rows()
            .iter()
            .position(|r| r.label == "[S] claude-one")
            .unwrap() as u16;
        a.on_mouse(MouseAction::Down, 5, 3 + row);
        assert_eq!(a.focus, ManageFocus::Tree, "one click selects the row");
        // A second click inside the double-click window steps in.
        a.on_mouse(MouseAction::Down, 5, 3 + row);
        assert_eq!(
            a.focus,
            ManageFocus::Session,
            "a double click means type-into-the-session"
        );

        // ^o hands the keyboard back to the list…
        a.on_key(ctrl('o'));
        assert_eq!(a.focus, ManageFocus::Tree, "^o returns to the rows");

        // …and x now ends the highlighted session instead of typing into it.
        let effect = a.on_key(key(KeyCode::Char('x')));
        assert!(
            matches!(effect, Some(Effect::KillSession { ref session_name, .. }) if session_name == "claude-one"),
            "{effect:?}"
        );
    }

    /// The relay registers a session under its own name, not the one the
    /// pane was minted with — so when the listing arrives, the pane adopts
    /// the listed name and the placeholder collapses into the real row,
    /// rather than both standing as duplicates.
    #[test]
    fn a_platform_listing_renames_the_pane_instead_of_duplicating() {
        let mut a = loaded_app();
        let mut pane = session("ca_1", "nimble-otter");
        pane.durable_name = "railway-t5soqz".into();
        a.attach_session(pane, "ca_1".into());
        assert_eq!(a.selected_row().unwrap().label, "[S] railway-t5soqz");

        // The platform lists the same session under its own petname.
        a.sessions_loaded(
            (0, 0, 0, 0),
            "ca_1",
            Ok(vec![ConsoleSession {
                name: "merry-daisy-ld9".into(),
                kind: "SHELL".into(),
                command: Some(
                    "export RAILWAY_CODE_AUTOSTARTED=1; railway-agent-tui --session \
                     \"$RAILWAY_DURABLE_SESSION_NAME\" 'My firs test'; printf 'x'"
                        .into(),
                ),
                running: true,
                attached: true,
                created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0),
                    snapshot: None,
            }]),
        );

        assert_eq!(
            a.sessions[0].durable_name, "merry-daisy-ld9",
            "the pane took the listed name"
        );
        let rows = a.rows();
        let threads: Vec<&Row> = rows
            .iter()
            .filter(|r| matches!(r.kind, RowKind::Session(..)))
            .collect();
        assert_eq!(threads.len(), 1, "one session, one row: {rows:#?}");
        assert_eq!(threads[0].label, "[S] railway-ld9", "the name leads");
        assert!(threads[0].note.is_empty(), "the orb and name are the row");
        assert_eq!(
            threads[0].status.as_deref(),
            Some("connected"),
            "and the pane counts as attached to it"
        );
        assert_eq!(
            a.selected_row().unwrap().label,
            "[S] railway-ld9",
            "the cursor followed the rename"
        );
    }

    /// A running agent with one listed, running session — the shape startup
    /// auto-connect acts on.
    fn app_with_listed_session() -> App {
        let mut a = loaded_app();
        a.sessions_loaded(
            (0, 0, 0, 0),
            "ca_1",
            Ok(vec![ConsoleSession {
                name: "merry-daisy-ld9".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0),
                    snapshot: None,
            }]),
        );
        a
    }

    /// Coming up, every listed session on a running agent gets a background
    /// attach: returned once, spinner on while it is in flight, and the pane
    /// joins without stealing the keyboard when it opens.
    #[test]
    fn startup_auto_connects_listed_sessions_once() {
        let mut a = app_with_listed_session();
        assert_eq!(
            a.take_auto_connects(),
            vec![AutoConnect {
                agent_id: "ca_1".into(),
                agent_name: "nimble-otter".into(),
                environment_id: "env_prod".into(),
                session_name: "merry-daisy-ld9".into(),
            }]
        );
        let rows = a.rows();
        let row = rows
            .iter()
            .find(|r| matches!(r.kind, RowKind::Session(..)))
            .unwrap();
        assert_eq!(
            row.status.as_deref(),
            Some("connecting"),
            "the spinner is on while the attach is in flight"
        );
        assert!(
            a.take_auto_connects().is_empty(),
            "one attempt per session per run"
        );

        // The pane opens quietly: the spinner becomes the connected dot, and
        // focus, cursor and the welcome pane all stay put.
        let cursor = a.cursor;
        let mut pane = session("ca_1", "nimble-otter");
        pane.durable_name = "merry-daisy-ld9".into();
        a.attach_session_background(pane, "ca_1".into());
        assert!(a.connecting.is_empty());
        assert_eq!(a.focus, ManageFocus::Tree, "no focus steal");
        assert_eq!(a.active, None, "the pane waits to be picked");
        assert_eq!(a.cursor, cursor, "no cursor jump");
        let rows = a.rows();
        let row = rows
            .iter()
            .find(|r| matches!(r.kind, RowKind::Session(..)))
            .unwrap();
        assert_eq!(row.status.as_deref(), Some("connected"));
    }

    /// A sleeping agent's sessions are neither auto-attached nor ever shown
    /// green — a session can't be better off than the agent it runs on. The
    /// same for an agent mid-operation: `sleeping…` on the orb means hands
    /// off.
    #[test]
    fn a_sleeping_agents_sessions_are_never_green_or_auto_connected() {
        let mut a = app_with_listed_session();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].status = "sleeping".into();
        }
        assert!(a.take_auto_connects().is_empty());
        // Even a pane still open onto it does not make the row green.
        let mut pane = session("ca_1", "nimble-otter");
        pane.durable_name = "merry-daisy-ld9".into();
        a.sessions = vec![pane];
        let rows = a.rows();
        let row = rows
            .iter()
            .find(|r| matches!(r.kind, RowKind::Session(..)))
            .unwrap();
        assert_eq!(row.status, None, "not green while the agent sleeps");

        // A pending op holds auto-connect off too, even while the platform
        // still says running.
        let mut b = app_with_listed_session();
        b.ops.insert("ca_1".into(), AgentOp::Sleep.pending_label());
        assert!(b.take_auto_connects().is_empty());
    }

    /// Auto-connect stays quiet where a connect couldn't: `railway code` came
    /// for its one session, and an unregistered key would land every pane on
    /// the relay's signup screen.
    #[test]
    fn auto_connect_waits_for_the_gates() {
        let mut a = app_with_listed_session();
        a.quit_when_done = true;
        assert!(a.take_auto_connects().is_empty());

        let mut b = app_with_listed_session();
        b.ssh_key = SshKeyState::NeedsRegistration(offer());
        assert!(b.take_auto_connects().is_empty());
        // The gate clearing lets the next pass start it.
        b.ssh_key = SshKeyState::Ready;
        assert_eq!(b.take_auto_connects().len(), 1);
    }

    /// `x` closes our window onto a session; the next sweep must not reopen
    /// what was deliberately closed.
    #[test]
    fn a_closed_pane_is_not_reopened_by_auto_connect() {
        let mut a = app_with_listed_session();
        let mut pane = session("ca_1", "nimble-otter");
        pane.durable_name = "merry-daisy-ld9".into();
        a.attach_session(pane, "ca_1".into());
        assert!(a.take_auto_connects().is_empty(), "already attached");
        drop(a.take_session(0));
        assert!(a.take_auto_connects().is_empty(), "closed means closed");
    }

    /// Enter on a session whose attach is already in flight must not race a
    /// second ssh onto the same session — and a failed background attempt
    /// hands the row back to manual connects instead of retrying in a loop.
    #[test]
    fn a_connecting_session_refuses_a_second_attach() {
        let mut a = app_with_listed_session();
        assert_eq!(a.take_auto_connects().len(), 1);
        a.cursor = a
            .rows()
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session(..)))
            .unwrap();
        assert_eq!(a.on_key(key(KeyCode::Enter)), None);
        assert!(a.status.starts_with("Connecting"), "{}", a.status);

        a.auto_connect_failed("merry-daisy-ld9", "relay said no");
        assert!(a.connecting.is_empty());
        assert!(
            a.take_auto_connects().is_empty(),
            "a failure is not retried in a loop"
        );
        let effect = a.on_key(key(KeyCode::Enter));
        assert!(
            matches!(effect, Some(Effect::Reattach { .. })),
            "but connecting by hand still works: {effect:?}"
        );
    }

    /// Startup fan-out is bounded: only [`AUTO_CONNECT_INFLIGHT`] attaches
    /// run at once, and the rest start as those land — never a thundering
    /// herd of ssh connections on a fleet-sized account.
    #[test]
    fn auto_connect_fan_out_is_capped() {
        let mut a = loaded_app();
        let sessions: Vec<ConsoleSession> = (0..5)
            .map(|i| ConsoleSession {
                name: format!("merry-daisy-{i}"),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: chrono::DateTime::from_timestamp(1_700_000_000 + i, 0),
                    snapshot: None,
            })
            .collect();
        a.sessions_loaded((0, 0, 0, 0), "ca_1", Ok(sessions));

        assert_eq!(a.take_auto_connects().len(), 3, "first batch fills the cap");
        assert!(
            a.take_auto_connects().is_empty(),
            "nothing more while the cap is full"
        );
        // One lands (or fails): exactly one slot's worth starts next.
        a.auto_connect_failed("merry-daisy-0", "relay said no");
        assert_eq!(a.take_auto_connects().len(), 1);
    }

    /// A listed session whose attach is in flight is not adoption material:
    /// renaming a freshly minted pane onto it would leave two panes sharing
    /// one durable name the moment the auto-connect lands.
    #[test]
    fn adoption_skips_sessions_with_an_attach_in_flight() {
        let mut a = loaded_app();
        // Listed as attached-and-running — normally prime adoption material —
        // but its background attach is still in flight.
        a.sessions_loaded(
            (0, 0, 0, 0),
            "ca_1",
            Ok(vec![ConsoleSession {
                name: "merry-daisy-ld9".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: true,
                created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0),
                    snapshot: None,
            }]),
        );
        assert_eq!(a.take_auto_connects().len(), 1, "the attach is in flight");

        // A fresh launch opens its own pane under a minted name.
        let mut pane = session("ca_1", "nimble-otter");
        pane.durable_name = "railway-fresh1".into();
        a.attach_session(pane, "ca_1".into());
        assert_eq!(
            a.sessions[0].durable_name, "railway-fresh1",
            "the minted pane must not steal the in-flight name"
        );

        // Once nothing is in flight for it, adoption works as before.
        a.connecting.clear();
        a.adopt_pane_sessions();
        assert_eq!(a.sessions[0].durable_name, "merry-daisy-ld9");
    }

    /// `n` targets the row under the cursor, as the footer advertises: a new
    /// agent lands in that row's project, not wherever the prompt happened
    /// to be aimed.
    #[test]
    fn n_retargets_to_the_cursor_row() {
        let mut a = loaded_app();
        a.target = None;
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        assert_eq!(a.on_key(key(KeyCode::Char('n'))), None);
        assert_eq!(a.screen, Screen::HarnessPick);
        assert_eq!(
            a.target.as_ref().map(|t| t.label()),
            Some("devtools/production".to_string()),
            "the new agent goes where the cursor points"
        );
    }

    /// The cursor opens on New Session, so `t` there is a letter, not a
    /// command. Retargeting is Ctrl-T everywhere.
    #[test]
    fn t_types_in_the_prompt_and_ctrl_t_retargets() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('t')));
        a.on_key(key(KeyCode::Char('e')));
        assert_eq!(a.prompt, "te");
        assert_eq!(a.screen, Screen::Manage);

        a.on_key(ctrl('t'));
        assert_eq!(a.screen, Screen::TargetPick);
        assert_eq!(a.prompt, "te", "retargeting must not eat the draft");
    }

    /// A paste lands in the prompt as text — dictation tools insert whole
    /// utterances this way. Newlines stay newlines (the box holds paragraphs,
    /// same as ⇧enter types), but a line break is never taken as Enter: a
    /// paste must not launch mid-thought.
    #[test]
    fn a_paste_lands_in_the_prompt_with_its_newlines() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('f')));
        assert_eq!(a.on_paste("ix the login bug\r\nthen deploy".into()), None);
        assert_eq!(a.prompt, "fix the login bug\nthen deploy");
        assert_eq!(a.screen, Screen::Manage, "a pasted newline is not Enter");
    }

    /// ⇧enter puts a newline in the draft; plain enter still launches. The
    /// same chord works in the ⌥p composer — the two boxes are one control.
    #[test]
    fn shift_enter_breaks_the_line_in_the_prompt_boxes() {
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);

        let mut a = app();
        a.on_key(key(KeyCode::Char('f')));
        a.on_key(key(KeyCode::Char('i')));
        a.on_key(key(KeyCode::Char('x')));
        assert_eq!(a.on_key(shift_enter), None);
        a.on_key(key(KeyCode::Char('g')));
        a.on_key(key(KeyCode::Char('o')));
        assert_eq!(a.prompt, "fix\ngo");
        assert_eq!(a.screen, Screen::Manage, "⇧enter is not a launch");

        let mut b = loaded_app();
        b.screen = Screen::ManagePrompt;
        b.manage_prompt = Some("fix".into());
        assert_eq!(b.on_key(shift_enter), None);
        assert_eq!(b.manage_prompt.as_deref(), Some("fix\n"));
        assert_eq!(b.screen, Screen::ManagePrompt, "the composer stays up");

        // Most terminals can't say "shifted Enter" — the configured ones
        // (Claude Code's /terminal-setup included) send `ESC CR`, which
        // parses as alt+enter. Same newline.
        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        let mut c = app();
        c.on_key(key(KeyCode::Char('f')));
        assert_eq!(c.on_key(alt_enter), None);
        assert_eq!(c.prompt, "f\n");
        assert_eq!(c.screen, Screen::Manage, "⌥enter in the box is a newline");
    }

    /// Where typing is refused, pasting is too; and off the prompt a paste
    /// must not be sprayed across the hotkeys.
    #[test]
    fn a_paste_respects_prompt_focus_and_shell() {
        let mut a = app();
        a.cursor = 3; // the projects tail header, not the launcher
        assert_eq!(a.on_paste("stray".into()), None);
        assert_eq!(a.prompt, "", "tree rows are not a text box");

        a.cursor = 0;
        a.harness = HARNESSES.len() - 1;
        assert!(a.shell_selected());
        assert_eq!(a.on_paste("stray".into()), None);
        assert_eq!(
            a.prompt, "",
            "the box says typing is refused; so is a paste"
        );
    }

    /// The target chooser is the setup flow's project card, not a trip through
    /// the management tree: one list of places, enter, back to the prompt.
    #[test]
    fn picking_a_target_returns_to_the_tree_with_it_set() {
        let mut a = loaded_app();
        a.on_key(ctrl('t'));
        assert_eq!(a.screen, Screen::TargetPick);

        let rows = a.target_pick.as_ref().unwrap().rows(None);
        assert!(
            rows.iter()
                .any(|(label, _)| label == "devtools (production)"),
            "{rows:?}"
        );

        let index = rows
            .iter()
            .position(|(label, _)| label == "devtools (production)")
            .unwrap();
        for _ in 0..index {
            a.on_key(key(KeyCode::Down));
        }
        // Picking here is picking a default, not pointing this one run: the
        // choice is remembered, and the tree leads with it from now on.
        let Some(Effect::SaveDefaultProject(saved)) = a.on_key(key(KeyCode::Enter)) else {
            panic!("the choice should be saved");
        };
        assert_eq!(saved.project_id, "proj_1");
        assert_eq!(a.screen, Screen::Manage);
        assert!(a.target_pick.is_none());
        assert_eq!(a.default_project.as_deref(), Some("proj_1"));
        assert_eq!(a.target.unwrap().label(), "devtools/production");
    }

    /// A session whose agent is handling the mouse itself: the click goes to
    /// the agent, which is what makes its own clickable output work.
    /// Unix only: this needs a mode-setting escape sequence to survive the trip
    /// through the pty, and Windows' ConPTY interprets those for itself instead
    /// of passing them along, so the emulator never sees the mode change. Plain
    /// text round-trips fine, which is why the rest of these run everywhere.
    #[cfg(unix)]
    #[test]
    fn clicks_reach_an_agent_that_is_using_the_mouse() {
        let mut a = mouse_aware_app();

        // The first click focuses the pane and is ours — otherwise clicking
        // into a session would poke whatever is under the pointer.
        a.focus = ManageFocus::Tree;
        assert_eq!(a.on_mouse(MouseAction::Down, 40, 4), None);
        assert_eq!(a.focus, ManageFocus::Session);
        assert!(a.selection.is_some(), "the focusing click is still ours");

        // The next one goes to the agent: no selection, nothing to copy.
        a.on_mouse(MouseAction::Up, 40, 4);
        assert_eq!(a.on_mouse(MouseAction::Down, 40, 4), None);
        assert!(a.selection.is_none(), "the click went to the agent");
        a.on_mouse(MouseAction::Drag, 44, 4);
        assert!(a.selection.is_none(), "and so did the drag");
        assert_eq!(a.on_mouse(MouseAction::Up, 44, 4), None);
        assert!(a.pending_copy.is_none(), "nothing was selected to copy");
    }

    /// Shift is the terminal's own "this click is mine": it takes the mouse
    /// back for a selection even while the agent is using it.
    /// Unix only: this needs a mode-setting escape sequence to survive the trip
    /// through the pty, and Windows' ConPTY interprets those for itself instead
    /// of passing them along, so the emulator never sees the mode change. Plain
    /// text round-trips fine, which is why the rest of these run everywhere.
    #[cfg(unix)]
    #[test]
    fn shift_takes_a_click_back_from_the_agent() {
        let mut a = mouse_aware_app();
        a.focus = ManageFocus::Session;

        assert_eq!(a.on_mouse_shifted(MouseAction::Down, 40, 4, true), None);
        assert!(a.selection.is_some(), "shift selects locally");
        a.on_mouse_shifted(MouseAction::Drag, 48, 4, true);
        a.on_mouse_shifted(MouseAction::Up, 48, 4, true);
        assert!(a.pending_copy.is_some(), "and copies");
    }

    /// A link still wins: opening it is more use than anything the agent will
    /// do with the click, and it is the case this was built for.
    /// Unix only: this needs a mode-setting escape sequence to survive the trip
    /// through the pty, and Windows' ConPTY interprets those for itself instead
    /// of passing them along, so the emulator never sees the mode change. Plain
    /// text round-trips fine, which is why the rest of these run everywhere.
    #[cfg(unix)]
    #[test]
    fn a_link_is_opened_rather_than_handed_to_the_agent() {
        let mut a = mouse_aware_app();
        a.focus = ManageFocus::Session;
        let session = a.sessions.get_mut(0).unwrap();
        session.send(b"\x1b[10;1Hsee https://railway.com/deploy now\r\n");
        for _ in 0..60 {
            if a.sessions[0].url_at(9, 8).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let (col, row) = (34 + 8, 3 + 9);
        assert_eq!(a.on_mouse(MouseAction::Down, col, row), None);
        assert_eq!(
            a.on_mouse(MouseAction::Up, col, row),
            Some(Effect::OpenUrl("https://railway.com/deploy".into()))
        );
    }

    /// An app with a session whose agent has mouse reporting on.
    #[cfg(unix)]
    fn mouse_aware_app() -> App {
        let mut a = loaded_app();
        let mut session = super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap();
        session.resize(20, 60);
        session.send(b"\x1b[?1002h\x1b[?1006h\r\n");
        for _ in 0..60 {
            if session.wants_mouse() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(session.wants_mouse(), "the fixture should want the mouse");
        a.attach_session(session, "ca_1".into());
        a.panes.session = PaneBox {
            x: 34,
            y: 3,
            w: 60,
            h: 20,
        };
        a.panes.session_outer = PaneBox {
            x: 33,
            y: 2,
            w: 62,
            h: 22,
        };
        a
    }

    /// The pane owns the mouse, so the terminal's own link handling never sees
    /// the click — opening it ourselves is what puts it back.
    #[test]
    fn clicking_a_link_in_a_session_opens_it() {
        let mut a = loaded_app();
        let mut session = super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap();
        session.resize(6, 60);
        session.send(b"see https://railway.com/deploy now\r\n");
        for _ in 0..40 {
            if session
                .with_screen(|s| s.contents_between(0, 0, 0, u16::MAX))
                .is_some_and(|line| line.contains("railway.com"))
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        a.attach_session(session, "ca_1".into());
        a.panes.session = PaneBox {
            x: 34,
            y: 3,
            w: 60,
            h: 6,
        };
        a.panes.session_outer = PaneBox {
            x: 33,
            y: 2,
            w: 62,
            h: 8,
        };

        // Press and release without moving: that is a click.
        let (col, row) = (34 + 8, 3);
        assert_eq!(a.on_mouse(MouseAction::Down, col, row), None);
        assert_eq!(
            a.on_mouse(MouseAction::Up, col, row),
            Some(Effect::OpenUrl("https://railway.com/deploy".into()))
        );
        assert!(a.selection.is_none(), "opening a link is not a selection");
        assert!(a.pending_copy.is_none(), "and it is not a copy");
    }

    /// Dragging from a link selects it instead. Copying a URL and opening one
    /// are both things people do; the pointer moving is what tells them apart.
    #[test]
    fn dragging_from_a_link_copies_rather_than_opening_it() {
        let mut a = loaded_app();
        let mut session = super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap();
        session.resize(6, 60);
        session.send(b"see https://railway.com/deploy now\r\n");
        for _ in 0..40 {
            if session
                .with_screen(|s| s.contents_between(0, 0, 0, u16::MAX))
                .is_some_and(|line| line.contains("railway.com"))
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        a.attach_session(session, "ca_1".into());
        a.panes.session = PaneBox {
            x: 34,
            y: 3,
            w: 60,
            h: 6,
        };
        a.panes.session_outer = PaneBox {
            x: 33,
            y: 2,
            w: 62,
            h: 8,
        };

        a.on_mouse(MouseAction::Down, 34 + 8, 3);
        a.on_mouse(MouseAction::Drag, 34 + 20, 3);
        assert_eq!(a.on_mouse(MouseAction::Up, 34 + 20, 3), None);
        assert!(a.pending_copy.is_some(), "a drag is a selection");
    }

    /// A double click on ordinary output is not a link, and must not be treated
    /// as one.
    #[test]
    fn clicking_plain_text_opens_nothing() {
        let mut a = loaded_app();
        let mut session = super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap();
        session.resize(6, 60);
        session.send(b"just some output\r\n");
        std::thread::sleep(std::time::Duration::from_millis(80));
        a.attach_session(session, "ca_1".into());
        a.panes.session = PaneBox {
            x: 34,
            y: 3,
            w: 60,
            h: 6,
        };
        a.panes.session_outer = PaneBox {
            x: 33,
            y: 2,
            w: 62,
            h: 8,
        };

        let (col, row) = (34 + 2, 3);
        a.on_mouse(MouseAction::Down, col, row);
        assert_eq!(a.on_mouse(MouseAction::Up, col, row), None);
    }

    /// The prompt box is clickable: wherever the cursor has wandered in the
    /// tree, clicking the box puts the keyboard back in the prompt.
    #[test]
    fn clicking_the_prompt_box_takes_the_keyboard_back() {
        let mut a = app();
        a.cursor = 3; // off the launcher, into the tree
        a.panes.prompt = PaneBox {
            x: 8,
            y: 10,
            w: 74,
            h: 6,
        };

        assert_eq!(a.on_mouse(MouseAction::Down, 20, 12), None);
        assert_eq!(a.cursor, 0, "the click lands on the New Session row");
        assert_eq!(a.focus, ManageFocus::Tree);

        // And typing goes into it, rather than being read as a tree hotkey.
        a.on_key(key(KeyCode::Char('h')));
        assert_eq!(a.prompt, "h");
    }

    /// Clicking the launcher pane must not hand focus to a session pane that
    /// isn't there — the keyboard stays with the tree, where the prompt reads
    /// it.
    #[test]
    fn clicking_the_launcher_pane_keeps_the_keyboard() {
        let mut a = app();
        a.panes.session_outer = PaneBox {
            x: 34,
            y: 2,
            w: 64,
            h: 20,
        };
        assert_eq!(a.on_mouse(MouseAction::Down, 40, 5), None);
        assert_eq!(a.focus, ManageFocus::Tree, "no session to type into");
        a.on_key(key(KeyCode::Char('h')));
        assert_eq!(a.prompt, "h");
    }

    /// ⌥f gives the session pane the screen, and gives it back.
    #[test]
    fn alt_f_maximizes_the_session_pane_and_restores_it() {
        let mut a = loaded_app();
        a.attach_session(
            super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );
        assert!(a.active.is_some());

        assert_eq!(a.on_key(alt('f')), None);
        assert!(a.maximized);
        assert_eq!(
            a.focus,
            ManageFocus::Session,
            "nothing else is on screen to have the keyboard"
        );

        assert_eq!(a.on_key(alt('f')), None);
        assert!(!a.maximized);
    }

    /// Including from inside the session, which is where you are when you want
    /// the room. Every other key still belongs to the agent.
    #[test]
    fn alt_f_reaches_the_layout_from_inside_a_focused_session() {
        let mut a = loaded_app();
        a.attach_session(
            super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );
        a.focus = ManageFocus::Session;

        assert_eq!(a.on_key(alt('f')), None);
        assert!(a.maximized);

        // ⌥s is not intercepted there — it belongs to whatever is running.
        a.on_key(alt('s'));
        assert!(a.settings.is_none());
    }

    /// The harness exiting ends its pane. Under `railway code` — one session,
    /// `quit_when_done` — the TUI leaves with it, back to the local prompt;
    /// this is what ctrl-c out of the agent lands on now that the pane's
    /// remote command has no shell fallback to strand you in.
    #[test]
    fn an_ended_session_under_railway_code_quits_the_tui() {
        let mut a = loaded_app();
        a.quit_when_done = true;
        a.attach_session(
            super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );

        // Still running: nothing to reap.
        assert_eq!(a.reap_ended_sessions(), None);
        assert_eq!(a.sessions.len(), 1);

        a.sessions[0].end_for_test();
        assert_eq!(a.reap_ended_sessions(), Some(Effect::Quit));
        assert!(a.sessions.is_empty());
        let note = a.exit_note.as_deref().expect("says the agent still runs");
        assert!(note.contains("nimble-otter"), "{note}");
    }

    /// From bare `railway ca`, an ended pane closes back to the tree — the
    /// browser was asked for, so it stays.
    #[test]
    fn an_ended_session_from_the_tree_closes_back_to_it() {
        let mut a = loaded_app();
        a.attach_session(
            super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );
        a.sessions[0].end_for_test();

        assert_eq!(a.reap_ended_sessions(), None);
        assert!(a.sessions.is_empty());
        assert_eq!(a.focus, ManageFocus::Tree);
        assert!(a.toast.is_some(), "the close is announced, not silent");
        assert!(a.exit_note.is_none());
    }

    /// An ssh that exits nonzero is a dropped connection or a failed connect,
    /// not a finished session: its pane holds the only account of what
    /// happened and the recovery keys can reconnect it, so it stays up — even
    /// under `railway code`.
    #[test]
    fn a_dropped_connection_keeps_its_pane() {
        let mut a = loaded_app();
        a.quit_when_done = true;
        a.attach_session(
            super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );
        a.sessions[0].end_dropped_for_test();

        assert_eq!(a.reap_ended_sessions(), None);
        assert_eq!(a.sessions.len(), 1, "the pane stays for recovery");
    }

    /// The session the user was typing into finishing WITHOUT them asking —
    /// no recent input, not a boot crash — starts a replacement on the same
    /// agent, with the same harness and no prompt draft riding along.
    #[test]
    fn an_unasked_finish_of_the_focused_pane_respawns() {
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        assert_eq!(a.focus, ManageFocus::Session, "attach focused the pane");
        a.prompt = "half a sentence".into();
        a.sessions[0].backdate_spawn_for_test(std::time::Duration::from_secs(60));
        a.sessions[0].end_for_test();

        let Some(Effect::Launch(req)) = a.reap_ended_sessions() else {
            panic!("expected a replacement launch");
        };
        assert_eq!(req.agent_id.as_deref(), Some("ca_1"));
        assert!(
            req.force_new,
            "the old durable session exited with its harness"
        );
        assert_eq!(req.prompt, None, "a respawn must not submit the draft");
        let toast = a.toast.as_ref().expect("announced").text.clone();
        assert!(toast.contains("starting a new one"), "{toast}");
    }

    /// Input just before the end is the user ending it — ctrl-c, `/exit` —
    /// and a finish that soon after connecting is a harness that cannot
    /// start. Neither earns a replacement, and nor does a pane the user
    /// was not focused on.
    #[test]
    fn deliberate_fast_or_background_finishes_do_not_respawn() {
        // Typed moments before the exit: the user asked for this.
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        a.sessions[0].backdate_spawn_for_test(std::time::Duration::from_secs(60));
        a.sessions[0].touch_input_for_test();
        a.sessions[0].end_for_test();
        assert_eq!(a.reap_ended_sessions(), None, "ctrl-c must stay ended");

        // Died within the settle window: respawning would loop on a harness
        // that cannot boot.
        let mut b = loaded_app();
        b.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        b.sessions[0].end_for_test();
        assert_eq!(b.reap_ended_sessions(), None, "a boot crash must not loop");

        // The keyboard was in the tree: an unwatched pane ending must not
        // conjure a session under the user's hands.
        let mut c = loaded_app();
        c.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        c.focus = ManageFocus::Tree;
        c.sessions[0].backdate_spawn_for_test(std::time::Duration::from_secs(60));
        c.sessions[0].end_for_test();
        assert_eq!(c.reap_ended_sessions(), None, "background panes just close");
    }

    /// A drop of the pane ON SCREEN pulls the keyboard to it — the banner
    /// says "r reconnects", and r has to mean that without a click first —
    /// but only once: stepping back out with Esc sticks, however many times
    /// the reap runs over the same corpse.
    #[test]
    fn a_dropped_connection_takes_focus_once() {
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        // The user is browsing the tree, cursor off the launcher, with the
        // pane still showing, when its connection dies.
        a.focus = ManageFocus::Tree;
        a.cursor = a
            .rows()
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session(0, 0, 0, 0, _)))
            .unwrap();
        assert_eq!(a.active, Some(0), "the attached pane is on screen");
        a.sessions[0].end_dropped_for_test();

        assert_eq!(a.reap_ended_sessions(), None);
        assert_eq!(a.focus, ManageFocus::Session, "the drop took the keyboard");

        // r now means reconnect, immediately.
        let effect = a.on_key(key(KeyCode::Char('r')));
        assert!(
            matches!(effect, Some(Effect::Reattach { .. })),
            "r reconnects the dropped pane: {effect:?}"
        );

        // The same drop noticed again must not wrestle Esc for the keyboard.
        let mut b = loaded_app();
        b.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        b.sessions[0].end_dropped_for_test();
        assert_eq!(b.reap_ended_sessions(), None);
        assert_eq!(b.on_key(key(KeyCode::Esc)), None);
        assert_eq!(b.focus, ManageFocus::Tree, "esc stepped out");
        assert_eq!(b.reap_ended_sessions(), None);
        assert_eq!(b.focus, ManageFocus::Tree, "and it stays stepped out");
    }

    /// A drop never takes the keyboard from someone who wasn't looking at
    /// the pane: not from the launcher prompt mid-sentence, and not for a
    /// background pane (an auto-connect they never opened). The banner
    /// waits on the pane instead.
    #[test]
    fn a_background_drop_does_not_steal_the_keyboard() {
        // Mid-typing at the launcher: cursor 0, prompt focused, pane showing
        // the welcome screen — keystrokes must keep landing in the draft.
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        a.focus = ManageFocus::Tree;
        a.cursor = 0;
        a.prompt_focused = true;
        a.prompt = "half a thought".into();
        a.sessions[0].end_dropped_for_test();
        assert_eq!(a.reap_ended_sessions(), None);
        assert_eq!(
            a.focus,
            ManageFocus::Tree,
            "typing at the launcher is never interrupted"
        );

        // A pane that is not the one on screen: no steal either.
        let mut b = loaded_app();
        b.attach_session(session("ca_1", "one"), "ca_1".into());
        b.attach_session(session("ca_1", "two"), "ca_1".into());
        b.focus = ManageFocus::Tree;
        b.active = Some(1);
        b.cursor = b
            .rows()
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session(0, 0, 0, 0, _)))
            .unwrap();
        b.sessions[0].end_dropped_for_test();
        assert_eq!(b.reap_ended_sessions(), None);
        assert_eq!(
            b.focus,
            ManageFocus::Tree,
            "a background pane's drop waits on its row"
        );
    }

    /// Every bare Escape belongs to the harness — even two quick ones. The
    /// old double-tap release cost the user their running turn (a single Esc
    /// cancels Claude mid-run, and the first tap always went through).
    /// ⌥⇧esc releases without sending anything.
    #[test]
    fn bare_escapes_belong_to_the_harness_and_alt_shift_esc_releases() {
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        assert_eq!(a.focus, ManageFocus::Session);

        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert_eq!(
            a.focus,
            ManageFocus::Session,
            "two quick escapes are the harness's, not a release"
        );

        // ⌥⇧esc steps out — and unfolds a maximized layout, since focus on
        // an invisible tree helps nobody.
        a.maximized = true;
        let chord = KeyEvent::new(KeyCode::Esc, KeyModifiers::ALT | KeyModifiers::SHIFT);
        assert_eq!(a.on_key(chord), None);
        assert_eq!(a.focus, ManageFocus::Tree, "⌥⇧esc steps out");
        assert!(!a.maximized, "and brings the tree back");
    }

    /// A drop never steals the keyboard out from under a dialog — a y/n
    /// confirm answered into the wrong place is how sessions get killed by
    /// accident.
    #[test]
    fn a_drop_does_not_take_focus_from_a_dialog() {
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        a.focus = ManageFocus::Tree;
        a.confirm = Some(PendingConfirm {
            op: AgentOp::Delete,
            agent_id: "ca_1".into(),
            environment_id: "env_prod".into(),
            agent_name: "nimble-otter".into(),
        });
        a.sessions[0].end_dropped_for_test();
        assert_eq!(a.reap_ended_sessions(), None);
        assert_eq!(a.focus, ManageFocus::Tree, "the dialog keeps the keyboard");
    }

    /// Ended but with the exit status still on its way: no call is made yet —
    /// the loop's poll (`awaiting_exit_status`) asks again — and the pane is
    /// not closed on a guess.
    #[test]
    fn a_pane_awaiting_its_exit_status_is_left_alone() {
        let mut a = loaded_app();
        a.attach_session(
            super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );
        // `cat` is still running, so try_wait has nothing yet.
        a.sessions[0].mark_ended();

        assert_eq!(a.reap_ended_sessions(), None);
        assert_eq!(a.sessions.len(), 1);
        assert!(a.awaiting_exit_status(), "the loop should keep polling");
    }

    /// `railway code` collapses the tree from the first frame, before there is
    /// a session to collapse it around — the loading pane stands in for one.
    /// Without this the tree would be drawn for the whole boot and then vanish,
    /// which is the flicker the collapsed layout exists to avoid.
    #[test]
    fn a_launch_in_flight_fills_the_pane_on_its_own() {
        let mut a = loaded_app();
        a.maximized = true;
        assert!(!a.pane_is_full(), "nothing to show yet");

        a.start_loading(&launch_req());
        assert!(a.pane_is_full(), "the loading pane has the screen");

        // A launch that fails leaves neither: the tree comes back rather than
        // the screen going blank.
        a.launch_failed("no".into());
        assert!(!a.pane_is_full());
    }

    fn with_sessions(names: &[&str]) -> App {
        let mut a = loaded_app();
        for name in names {
            a.attach_session(
                super::super::session::Session::for_test(name, name).unwrap(),
                (*name).to_string(),
            );
        }
        a
    }

    /// ⌥] walks the open panes and wraps, and ⌥[ walks them the other way.
    #[test]
    fn alt_bracket_cycles_forward_through_open_sessions_and_wraps() {
        let mut a = with_sessions(&["ca_1", "ca_2", "ca_3"]);
        assert_eq!(a.active, Some(2), "the newest attach is active");

        assert_eq!(a.on_key(alt(']')), None);
        assert_eq!(a.active, Some(0), "wraps past the end");
        assert_eq!(a.on_key(alt(']')), None);
        assert_eq!(a.active, Some(1));
        assert_eq!(a.on_key(alt(']')), None);
        assert_eq!(a.active, Some(2), "back where it started");
    }

    /// The reverse chord retraces the forward one exactly, wrap included.
    #[test]
    fn alt_left_bracket_cycles_backward_and_wraps() {
        let mut a = with_sessions(&["ca_1", "ca_2", "ca_3"]);
        assert_eq!(a.active, Some(2), "the newest attach is active");

        assert_eq!(a.on_key(alt('[')), None);
        assert_eq!(a.active, Some(1));
        assert_eq!(a.on_key(alt('[')), None);
        assert_eq!(a.active, Some(0));
        assert_eq!(a.on_key(alt('[')), None);
        assert_eq!(a.active, Some(2), "wraps past the front");
    }

    /// The chord has to work from inside the pane — that is where you are when
    /// you want the next one. ⌥s must still fall through to the agent.
    #[test]
    fn alt_bracket_cycles_from_inside_a_focused_session() {
        let mut a = with_sessions(&["ca_1", "ca_2"]);
        a.focus = ManageFocus::Session;

        assert_eq!(a.on_key(alt(']')), None);
        assert_eq!(a.active, Some(0));
        assert_eq!(a.focus, ManageFocus::Session, "cycling does not detach");

        // ⌥[ is taken from the agent too — going back is the same moment as
        // going forward.
        assert_eq!(a.on_key(alt('[')), None);
        assert_eq!(a.active, Some(1), "retraces the forward step");
        assert_eq!(a.focus, ManageFocus::Session, "cycling does not detach");

        // Still only ⌥f, ⌥] and ⌥[ are taken from the agent.
        let screen = a.screen;
        a.on_key(alt('s'));
        assert_eq!(a.screen, screen, "⌥s belongs to whatever is running");
    }

    /// Cycling is scoped to panes that already exist: a keystroke must never
    /// turn into a cold boot on an agent you were passing through.
    #[test]
    fn alt_bracket_says_so_rather_than_no_opping() {
        let mut a = loaded_app();
        assert_eq!(a.on_key(alt(']')), None);
        assert_eq!(a.active, None);
        assert!(a.status.contains("No open sessions"), "{}", a.status);

        let mut b = with_sessions(&["ca_1"]);
        assert_eq!(b.on_key(alt(']')), None);
        assert_eq!(b.active, Some(0), "the only pane stays put");
        assert!(b.status.contains("Only one session"), "{}", b.status);
    }

    /// Without Option-as-Meta macOS composes the bracket chords into curly
    /// quotes — singles for ⌥], doubles for ⌥[ — and each pair folds its
    /// shifted form into the same chord.
    #[test]
    fn option_composed_curly_quotes_are_the_bracket_chords() {
        for composed in ['\u{2018}', '\u{2019}'] {
            let mut a = with_sessions(&["ca_1", "ca_2"]);
            assert_eq!(a.on_key(key(KeyCode::Char(composed))), None);
            assert_eq!(a.active, Some(0), "composed {composed:?} should cycle");
            assert!(a.prompt.is_empty(), "a chord is not text");
        }
        for composed in ['\u{201C}', '\u{201D}'] {
            let mut a = with_sessions(&["ca_1", "ca_2"]);
            assert_eq!(a.on_key(key(KeyCode::Char(composed))), None);
            assert_eq!(a.active, Some(0), "composed {composed:?} should cycle");
            assert!(a.prompt.is_empty(), "a chord is not text");
        }
    }

    /// ⌥[ only exists where the terminal can say it unambiguously — as a real
    /// ALT event under the kitty protocol, or as macOS's composed quote. In a
    /// legacy Meta terminal it is the CSI prefix `ESC [` and never reaches the
    /// key handler at all, so recognizing the clean forms risks nothing.
    #[test]
    fn alt_left_bracket_is_the_reverse_chord() {
        assert_eq!(alt_chord(&alt('[')), Some('['));
        assert_eq!(
            alt_chord(&key(KeyCode::Char('\u{201C}'))),
            Some('['),
            "the composed ⌥[ quote is the same chord"
        );
    }

    /// ⌥⇧[ and ⌥⇧] are the advertised cycle chords — `ESC {` collides with
    /// nothing, where `ESC [` is the CSI prefix half the world has claimed.
    /// Shift reaches us as the brace character, folded onto the same chords.
    #[test]
    fn alt_shift_brackets_are_the_same_chords() {
        assert_eq!(alt_chord(&alt('{')), Some('['));
        assert_eq!(alt_chord(&alt('}')), Some(']'));
        // macOS composes ⌥⇧[ to the closing curly double quote; same chord.
        assert_eq!(alt_chord(&key(KeyCode::Char('\u{201D}'))), Some('['));
        assert_eq!(alt_chord(&key(KeyCode::Char('\u{2019}'))), Some(']'));
    }

    /// A screen of empty pane is not a state worth reaching by accident.
    #[test]
    fn alt_f_does_nothing_without_a_session() {
        let mut a = loaded_app();
        assert_eq!(a.on_key(alt('f')), None);
        assert!(!a.maximized);
        assert!(a.status.contains("Connect to a session"), "{}", a.status);

        // And on the menu it is not a layout at all.
        let mut b = app();
        b.on_key(alt('f'));
        assert!(!b.maximized);
    }

    /// Ending the last session takes the layout with it, rather than leaving a
    /// full screen of nothing.
    #[test]
    fn closing_the_last_session_restores_the_tree() {
        let mut a = loaded_app();
        a.attach_session(
            super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );
        a.on_key(alt('f'));
        assert!(a.maximized);

        a.take_session(0);
        assert!(!a.maximized);
        assert_eq!(a.focus, ManageFocus::Tree);
    }

    /// A pane whose ssh has died must offer a way back in: `r` drops the dead
    /// pane and dials the same durable session again — the whole recovery
    /// path a dropped connection used to be missing.
    #[test]
    fn a_dead_pane_reconnects_with_r() {
        let mut a = with_sessions(&["ca_1"]);
        a.focus = ManageFocus::Session;
        a.sessions[0].mark_ended();

        let effect = a.on_key(key(KeyCode::Char('r')));
        assert_eq!(
            effect,
            Some(Effect::Reattach {
                agent_id: "ca_1".into(),
                agent_name: "ca_1".into(),
                // for_test connects as agent:test:test — the environment is
                // read back out of the relay target.
                environment_id: "test".into(),
                session_name: "test".into(),
            })
        );
        assert!(a.sessions.is_empty(), "the dead pane is gone");
        assert!(a.status.contains("Reconnecting"), "{}", a.status);
    }

    /// Keystrokes into a dead pane go nowhere on purpose — a pty nothing
    /// reads must not eat typing — and the recovery keys do their jobs.
    #[test]
    fn a_dead_pane_swallows_typing_and_answers_the_recovery_keys() {
        let mut a = with_sessions(&["ca_1"]);
        a.focus = ManageFocus::Session;
        a.sessions[0].mark_ended();

        assert_eq!(a.on_key(key(KeyCode::Char('h'))), None, "typing vanishes");
        assert_eq!(
            a.on_key(key(KeyCode::Char('x'))),
            Some(Effect::CloseSession { index: 0 })
        );
        // Enter means reconnect here — it is the reflex after "connection
        // closed", and there is nothing else it could mean.
        assert!(matches!(
            a.on_key(key(KeyCode::Enter)),
            Some(Effect::Reattach { .. })
        ));
    }

    /// Escape on a dead pane releases to the tree: the agent that would have
    /// received it is not on the other end any more.
    #[test]
    fn escape_on_a_dead_pane_returns_to_the_tree() {
        let mut a = with_sessions(&["ca_1"]);
        a.focus = ManageFocus::Session;
        a.sessions[0].mark_ended();

        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert_eq!(a.focus, ManageFocus::Tree);
    }

    /// "Connect" must never resolve to a corpse: an agent whose only pane has
    /// died reads as not connected, and the dead pane is cleared out of the
    /// way of the fresh connection.
    #[test]
    fn a_dead_pane_does_not_count_as_connected() {
        let mut a = with_sessions(&["ca_1"]);
        a.sessions[0].mark_ended();

        assert!(!a.activate_session("ca_1"));
        assert!(a.sessions.is_empty(), "the dead pane was cleared");
    }

    /// Enter on a session row whose pane has died reconnects instead of
    /// focusing the corpse. The old behaviour was a trap: the dead pane
    /// blocked every reattach to its session until it was closed from the
    /// agent row, which nothing advertised.
    #[test]
    fn enter_on_a_session_row_with_a_dead_pane_reattaches() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                // The durable name for_test sessions carry.
                name: "test".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]);
        }
        a.attach_session(
            super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );
        a.sessions[0].mark_ended();
        a.focus = ManageFocus::Tree;
        a.cursor = a.rows().iter().position(|r| r.label == "[S] test").unwrap();

        let effect = a.on_key(key(KeyCode::Enter));
        assert_eq!(
            effect,
            Some(Effect::Reattach {
                agent_id: "ca_1".into(),
                agent_name: "nimble-otter".into(),
                environment_id: "env_prod".into(),
                session_name: "test".into(),
            })
        );
        assert!(a.sessions.is_empty(), "the dead pane went first");
    }

    /// A live pane keeps its keys: recovery must only take over once the
    /// connection is actually gone.
    #[test]
    fn a_live_pane_still_owns_the_keyboard() {
        let mut a = with_sessions(&["ca_1"]);
        a.focus = ManageFocus::Session;

        assert_eq!(a.on_key(key(KeyCode::Char('r'))), None);
        assert_eq!(a.on_key(key(KeyCode::Char('x'))), None);
        assert_eq!(
            a.sessions.len(),
            1,
            "r and x were typing, not pane commands"
        );
        assert_eq!(a.focus, ManageFocus::Session);
    }

    /// So does Esc from the tree: with the pane maximized it restores the
    /// layout first, and quits only from the restored screen.
    #[test]
    fn escaping_a_maximized_pane_restores_the_tree() {
        let mut a = loaded_app();
        a.attach_session(
            super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".into(),
        );
        a.on_key(alt('f'));
        a.focus = ManageFocus::Tree;
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert_eq!(a.screen, Screen::Manage);
        assert!(!a.maximized);
        // With nothing maximized, Esc keeps walking up the chain instead of
        // leaving: a thread hands the cursor back to the launcher.
        a.cursor = a
            .rows()
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session(0, 0, 0, 0, _)))
            .unwrap();
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert_eq!(a.cursor, 0);
    }

    /// Cancelling saves nothing — the default is only changed by choosing one.
    #[test]
    fn cancelling_the_target_picker_saves_nothing() {
        let mut a = loaded_app();
        a.default_project = Some("proj_9".into());
        a.on_key(ctrl('t'));
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert_eq!(a.default_project.as_deref(), Some("proj_9"));
    }

    /// Escape leaves the target alone rather than clearing it.
    #[test]
    fn cancelling_the_target_picker_keeps_the_current_target() {
        let mut a = loaded_app();
        a.target = Some(Target {
            project_id: "proj_1".into(),
            project_name: "devtools".into(),
            environment_id: "env_prod".into(),
            environment_name: "production".into(),
        });
        a.on_key(ctrl('t'));
        // And it opens on the target already set, so enter changes nothing.
        let picker = a.target_pick.as_ref().unwrap();
        assert_eq!(
            picker.options[picker.cursor].environment_id, "env_prod",
            "the picker opens on the current target"
        );

        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.screen, Screen::Manage);
        assert_eq!(a.target.unwrap().label(), "devtools/production");
    }

    /// Enter with no target can't launch into nowhere; it asks for one instead.
    #[test]
    fn launching_without_a_target_asks_for_one() {
        let mut a = app();
        a.prompt = "fix the tests".into();
        assert_eq!(a.on_key(key(KeyCode::Enter)), None);
        assert_eq!(a.screen, Screen::TargetPick);
        assert_eq!(a.prompt, "fix the tests");
    }

    #[test]
    fn enter_on_the_prompt_launches_into_the_target_with_the_text() {
        let mut a = app();
        a.target = Some(Target {
            project_id: "proj_1".into(),
            project_name: "devtools".into(),
            environment_id: "env_prod".into(),
            environment_name: "production".into(),
        });
        for c in "fix the tests".chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
        let Effect::Launch(req) = a.on_key(key(KeyCode::Enter)).unwrap() else {
            panic!("expected a launch");
        };
        assert_eq!(req.prompt.as_deref(), Some("fix the tests"));
        assert_eq!(req.harness, "claude");
        assert_eq!(req.agent_id, None);
        assert!(req.force_new, "each prompt gets a fresh agent");
    }

    /// A blank enter launches the session bare — the harness comes up empty,
    /// ready for its first prompt inside — rather than nagging for a prompt.
    /// Whitespace never rides along as one.
    #[test]
    fn a_blank_prompt_launches_a_bare_session() {
        let mut a = app();
        a.target = Some(Target {
            project_id: "p".into(),
            project_name: "p".into(),
            environment_id: "e".into(),
            environment_name: "e".into(),
        });
        a.prompt = "   ".into();
        let Some(Effect::Launch(req)) = a.on_key(key(KeyCode::Enter)) else {
            panic!("a blank enter launches bare");
        };
        assert_eq!(req.prompt, None, "whitespace is not a prompt");
        assert!(req.force_new, "each launch is its own agent");

        // Shell too: its box says enter launches bare.
        a.harness = HARNESSES.len() - 1;
        let Effect::Launch(req) = a.on_key(key(KeyCode::Enter)).unwrap() else {
            panic!("expected a launch");
        };
        assert_eq!(req.prompt, None);
        assert!(req.force_new, "each launch is its own agent");
    }

    #[test]
    fn shift_tab_cycles_the_harness_on_the_prompt() {
        let mut a = app();
        assert_eq!(a.harness_name(), "claude");
        a.on_key(key(KeyCode::BackTab));
        assert_eq!(a.harness_name(), "codex");
        a.on_key(key(KeyCode::BackTab));
        assert_eq!(a.harness_name(), "grok");
        a.on_key(key(KeyCode::BackTab));
        assert_eq!(a.harness_name(), "shell", "shell closes the cycle");
        a.on_key(key(KeyCode::BackTab));
        assert_eq!(a.harness_name(), "railway", "cycling wraps");
    }

    /// The order is a contract: `railway` is the unconfigured default, and
    /// `shell` sits after every real agent on every surface that lists them.
    #[test]
    fn railway_leads_the_harnesses_and_shell_closes_them() {
        assert_eq!(HARNESSES.first(), Some(&"railway"));
        assert_eq!(HARNESSES.last(), Some(&"shell"));
        assert_eq!(default_harnesses(), &HARNESSES[..HARNESSES.len() - 1]);
        assert!(
            !default_harnesses().contains(&"shell"),
            "shell is not a saveable default"
        );
    }

    /// The shell option takes no prompt: the menu box refuses keystrokes while
    /// it is selected, keeps a draft typed for a real agent, and launches
    /// without one.
    #[test]
    fn the_prompt_goes_inert_on_shell() {
        let mut a = app();
        a.target = Some(Target {
            project_id: "p".into(),
            project_name: "p".into(),
            environment_id: "e".into(),
            environment_name: "e".into(),
        });
        a.on_key(key(KeyCode::Char('h')));
        a.on_key(key(KeyCode::Char('i')));
        assert_eq!(a.prompt, "hi");
        while a.harness_name() != "shell" {
            a.on_key(key(KeyCode::BackTab));
        }
        a.on_key(key(KeyCode::Char('x')));
        a.on_key(key(KeyCode::Backspace));
        assert_eq!(a.prompt, "hi", "typing is refused, the draft survives");
        let Effect::Launch(req) = a.on_key(key(KeyCode::Enter)).unwrap() else {
            panic!("expected a launch");
        };
        assert_eq!(req.harness, "shell");
        assert_eq!(req.prompt, None, "a leftover draft must not ride along");
        while a.harness_name() != "claude" {
            a.on_key(key(KeyCode::BackTab));
        }
        a.on_key(key(KeyCode::Char('!')));
        assert_eq!(a.prompt, "hi!", "typing resumes on a real agent");
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    /// Alt-chords are the point: they work from the prompt, where a bare letter
    /// is text.
    #[test]
    fn alt_chords_work_while_typing() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('s')));
        assert_eq!(a.prompt, "s", "a bare letter is still text");
        assert_eq!(a.on_key(alt('s')), None);
        assert_eq!(a.screen, Screen::Settings, "⌥s opens settings in place");
        assert_eq!(a.prompt, "s", "the chord must not touch the draft");
    }

    /// Terminals that compose Option+letter instead of sending Meta still get
    /// the chords there are.
    #[test]
    fn macos_composed_option_characters_are_accepted() {
        let mut a = app();
        assert_eq!(a.on_key(key(KeyCode::Char('ß'))), None);
        assert_eq!(a.screen, Screen::Settings);
        assert!(a.prompt.is_empty(), "a chord is not text");
    }

    /// ⌥t went with the theme chord: the theme now cycles on the settings
    /// card, and the key falls through like any other unclaimed letter.
    #[test]
    fn alt_t_is_no_longer_a_chord() {
        let mut a = app();
        let first = a.theme.slug;
        assert_eq!(a.on_key(alt('t')), None);
        assert_eq!(a.theme.slug, first, "the theme is ⌥s territory now");
        assert_eq!(a.screen, Screen::Manage);

        // And its composed form is plain text again, like any other
        // Option-composed character the TUI has no claim on.
        let mut b = app();
        let theme = b.theme.slug;
        b.on_key(key(KeyCode::Char('†')));
        assert_eq!(b.theme.slug, theme);
        assert_eq!(b.prompt, "†", "unclaimed, the character is text");
    }

    /// ⌥r asks for everything again, from wherever you are: the tree, the menu
    /// prompt where a bare `r` is text, and its macOS composed form.
    #[test]
    fn alt_r_refreshes_from_anywhere() {
        let mut a = loaded_app();
        assert_eq!(a.on_key(alt('r')), Some(Effect::RefreshAll));
        assert!(a.refresh_announce, "a keypress deserves an answer");

        let mut b = app();
        b.on_key(key(KeyCode::Char('r')));
        assert_eq!(b.prompt, "r", "a bare letter is still text");
        assert_eq!(b.on_key(alt('r')), Some(Effect::RefreshAll));
        assert_eq!(b.prompt, "r", "the chord must not touch the draft");

        // ⌥R, and the characters a stock macOS terminal composes for both.
        let mut c = app();
        assert_eq!(
            c.on_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::ALT)),
            Some(Effect::RefreshAll)
        );
        let mut d = app();
        assert_eq!(d.on_key(key(KeyCode::Char('®'))), Some(Effect::RefreshAll));
        assert!(d.prompt.is_empty(), "a chord is not text");
        let mut e = app();
        assert_eq!(e.on_key(key(KeyCode::Char('‰'))), Some(Effect::RefreshAll));
    }

    /// And from inside a session, where the tree is what you cannot see. A chord
    /// that silently did nothing half the time you pressed it would be worse
    /// than one the agent never gets.
    #[test]
    fn alt_r_refreshes_from_a_focused_session() {
        let mut a = with_sessions(&["ca_1"]);
        a.focus = ManageFocus::Session;
        assert_eq!(a.on_key(alt('r')), Some(Effect::RefreshAll));
        // A bare `r` still belongs to whatever is running in there.
        assert_eq!(a.on_key(key(KeyCode::Char('r'))), None);
    }

    /// `r` stays the cheap one — the environment under the cursor, one request —
    /// and the two must not collapse into each other.
    #[test]
    fn bare_r_still_refreshes_only_this_environment() {
        let mut a = loaded_app();
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        assert_eq!(
            a.on_key(key(KeyCode::Char('r'))),
            Some(Effect::LoadAgents {
                environment_id: "env_prod".into(),
                path: (0, 0, 0)
            })
        );
    }

    /// ^t keeps the target picker to itself now that ⌥t is gone — the two
    /// were different chords on the same letter.
    #[test]
    fn ctrl_t_still_opens_the_target_picker() {
        let mut a = app();
        let theme = a.theme.slug;
        a.on_key(ctrl('t'));
        assert_eq!(a.theme.slug, theme, "^t must not change the theme");
        assert_eq!(a.screen, Screen::TargetPick);
    }

    /// Finishing the wizard marks the app configured, so a re-run is offered
    /// through ⌥s rather than pushed at every start.
    #[test]
    fn finishing_setup_marks_the_app_configured() {
        let mut a = app();
        a.configured = false;
        a.start_wizard(false);
        assert_eq!(a.screen, Screen::Setup);
        let wizard = a.wizard.as_mut().unwrap();
        wizard.step = crate::commands::cloud_agent::tui::wizard::Step::Theme;
        assert!(matches!(
            a.on_key(key(KeyCode::Enter)),
            Some(Effect::SaveSetup(_))
        ));
        assert!(a.configured);
        assert_eq!(a.screen, Screen::Manage, "setup lands back on the tree");
    }

    /// Settings keeps the chord setup had, from either focus and mid-prompt.
    #[test]
    fn alt_s_opens_settings_from_anywhere() {
        let mut a = app();
        a.prompt = "fix the tests".into();
        assert_eq!(a.on_key(alt('s')), None);
        assert_eq!(a.screen, Screen::Settings);
        assert_eq!(a.prompt, "fix the tests", "the draft survives");
    }

    /// The settings card edits in place: changing the agent is one keypress
    /// and one save, not a walk through the flow.
    #[test]
    fn settings_cycles_the_agent_and_saves() {
        let mut a = app();
        a.on_key(alt('s'));
        let Some(Effect::SaveSettings(outcome)) = a.on_key(key(KeyCode::Right)) else {
            panic!("expected a save");
        };
        assert_eq!(outcome.agent, "codex");
        assert_eq!(
            outcome.theme, a.theme.slug,
            "the rest rides along unchanged"
        );
    }

    /// The theme applies to the whole screen as it cycles — a colour scheme
    /// is picked by looking at it, exactly like the wizard's theme step.
    #[test]
    fn settings_previews_the_theme_live() {
        let mut a = app();
        a.on_key(alt('s'));
        for _ in 0..3 {
            a.on_key(key(KeyCode::Down)); // down to the theme row
        }
        let first = a.theme.slug;
        let Some(Effect::SaveSettings(outcome)) = a.on_key(key(KeyCode::Right)) else {
            panic!("expected a save");
        };
        assert_ne!(a.theme.slug, first, "the whole screen follows");
        assert_eq!(outcome.theme, a.theme.slug);
    }

    /// The card opens showing the saved default project, not a blank.
    #[test]
    fn settings_opens_on_the_saved_default_project() {
        let mut a = app();
        a.default_project = Some("proj_1".into());
        a.target = Some(Target {
            project_id: "proj_1".into(),
            project_name: "devtools".into(),
            environment_id: "env_prod".into(),
            environment_name: "production".into(),
        });
        a.on_key(alt('s'));
        let settings = a.settings.as_ref().unwrap();
        assert_eq!(
            settings.project.as_ref().unwrap().project_name,
            "devtools",
            "seeded from the target, which holds the saved default"
        );
    }

    /// The last row hands over to the wizard, skipping its intro — choosing
    /// it has already answered "set up?".
    #[test]
    fn settings_can_replay_first_run_setup() {
        let mut a = app();
        a.on_key(alt('s'));
        for _ in 0..5 {
            a.on_key(key(KeyCode::Down)); // down to the last row
        }
        assert_eq!(a.on_key(key(KeyCode::Enter)), None);
        assert_eq!(a.screen, Screen::Setup);
        assert!(a.settings.is_none(), "the card handed over");
        assert_eq!(
            a.wizard.as_ref().unwrap().step,
            crate::commands::cloud_agent::tui::wizard::Step::Target
        );
    }

    /// Esc closes the card without ceremony: every change already saved.
    #[test]
    fn settings_escape_just_closes() {
        let mut a = app();
        a.on_key(alt('s'));
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert_eq!(a.screen, Screen::Manage);
        assert!(a.settings.is_none());
    }

    /// ←/→ move through the draft, and editing happens at the cursor — a typo
    /// mid-sentence is fixed in place, not by erasing back to it.
    #[test]
    fn arrow_keys_move_the_prompt_cursor() {
        let mut a = app();
        for c in "fix bug".chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
        for _ in 0..3 {
            a.on_key(key(KeyCode::Left));
        }
        a.on_key(key(KeyCode::Char('a')));
        assert_eq!(a.prompt, "fix abug");
        a.on_key(key(KeyCode::Backspace));
        assert_eq!(a.prompt, "fix bug");
        a.on_key(key(KeyCode::Home));
        a.on_key(key(KeyCode::Right));
        a.on_key(key(KeyCode::Backspace));
        assert_eq!(a.prompt, "ix bug");
        a.on_key(key(KeyCode::End));
        a.on_key(key(KeyCode::Char('!')));
        assert_eq!(a.prompt, "ix bug!");
        // The cursor never runs past either end.
        for _ in 0..20 {
            a.on_key(key(KeyCode::Right));
        }
        assert_eq!(a.prompt_cursor, a.prompt.chars().count());
    }

    /// A paste lands at the cursor, like typing does.
    #[test]
    fn a_paste_lands_at_the_cursor() {
        let mut a = app();
        for c in "fix the".chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
        for _ in 0..3 {
            a.on_key(key(KeyCode::Left));
        }
        a.on_paste("up ".into());
        assert_eq!(a.prompt, "fix up the");
    }

    /// Sessions are named universally by `harness-<random>`: the harness is
    /// read off the launch line's binary, and a relay petname folds down to
    /// its random suffix. No prompt parsing — the task lives in the detail
    /// card's command line, not the name.
    #[test]
    fn session_names_are_harness_led() {
        let minted = ConsoleSession {
            name: "claude-one".into(),
            kind: "SHELL".into(),
            command: Some(
                "export PATH=/x; export RAILWAY_CODE_AUTOSTARTED=1; \
                 claude 'fix the login bug'; printf 'x'"
                    .into(),
            ),
            running: true,
            attached: false,
            created_at: None,
                    snapshot: None,
        };
        assert_eq!(
            minted.short_name(),
            "claude-one",
            "a minted name already leads with its harness — and the prompt \
             stays out of it"
        );

        // The relay names sessions itself; the harness is prefixed onto the
        // petname's random suffix, flags and prompt ignored alike.
        let renamed = ConsoleSession {
            name: "merry-daisy-ld9".into(),
            kind: "SHELL".into(),
            command: Some(
                "export RAILWAY_CODE_AUTOSTARTED=1; railway-agent-tui --session \
                 \"$RAILWAY_DURABLE_SESSION_NAME\" 'ship the release'; printf 'x'"
                    .into(),
            ),
            running: true,
            attached: false,
            created_at: None,
                    snapshot: None,
        };
        assert_eq!(renamed.short_name(), "railway-ld9");

        // No command on record: the name stands as it is.
        let unknown = ConsoleSession {
            name: "quiet-depot-x7v".into(),
            kind: "SHELL".into(),
            command: None,
            running: true,
            attached: false,
            created_at: None,
                    snapshot: None,
        };
        assert_eq!(unknown.short_name(), "quiet-depot-x7v");
    }

    #[test]
    fn esc_walks_home_and_q_quits_there() {
        let mut a = app();
        a.prompt = "half a thought".into();
        // One step per press: the draft first…
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert!(a.prompt.is_empty());
        // …then the prompt lets go of the keyboard…
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert!(!a.prompt_focused, "home: letters are keys again");
        // …and Esc never quits — q does, now that it is a key.
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert_eq!(a.on_key(key(KeyCode::Char('q'))), Some(Effect::Quit));
    }

    /// From a thread, Esc returns to the launcher with the prompt focused —
    /// the same place the screen opens on.
    #[test]
    fn esc_from_a_thread_returns_to_the_launcher() {
        let mut a = loaded_app();
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert_eq!(a.cursor, 0, "back on the launcher");
        assert!(a.new_session_selected(), "with the prompt focused");
    }

    /// The loading screen has to show the task, not just a spinner — that is
    /// the whole reason the wait moved inside the TUI.
    #[test]
    fn launching_shows_the_task_and_its_steps() {
        let mut a = app();
        let req = LaunchRequest {
            project_id: "p".into(),
            environment_id: "e".into(),
            agent_id: None,
            session_name: None,
            force_new: false,
            new_session: false,
            harness: "claude".into(),
            prompt: Some("fix the tests".into()),
            label: "devtools/production".into(),
            base: Default::default(),
        };
        a.start_loading(&req);
        assert!(a.loading.active, "the pane shows the wait");
        assert_eq!(
            a.screen,
            Screen::Manage,
            "the tree stays visible while it starts"
        );
        assert_eq!(a.loading.prompt.as_deref(), Some("fix the tests"));
        assert_eq!(a.loading.target, "devtools/production");

        a.loading_step("Creating a cloud agent".into());
        a.loading_step("Creating a cloud agent".into());
        a.loading_step("Provisioning claude".into());
        assert_eq!(a.loading.steps.len(), 2, "repeats read as a stall");

        // The tree is still usable while an agent boots.
        assert_eq!(a.on_key(key(KeyCode::Down)), None);
    }

    #[test]
    fn a_failed_launch_lands_in_manage_with_the_reason() {
        let mut a = app();
        a.launch_failed("no codex sign-in".into());
        assert_eq!(a.screen, Screen::Manage);
        // The reason rides an error toast, not the header status line.
        let toast = a.toast.as_ref().expect("an error toast");
        assert!(!toast.ok);
        assert!(toast.text.contains("no codex sign-in"), "{}", toast.text);
    }

    /// Revealing an environment expands its ancestors and refetches — the agent
    /// may have just been created, so a cached list would not contain it.
    #[test]
    fn revealing_an_environment_expands_and_refetches() {
        let mut a = app();
        a.agents_loaded((0, 0, 0), Ok(vec![]));
        let effect = a.reveal_environment("env_prod").unwrap();
        assert_eq!(
            effect,
            Effect::LoadAgents {
                environment_id: "env_prod".into(),
                path: (0, 0, 0)
            }
        );
        assert!(a.tree[0].expanded);
        assert!(a.tree[0].projects[0].expanded);
        assert!(a.tree[0].projects[0].envs[0].expanded);
        assert!(a.reveal_environment("nope").is_none());
    }

    /// The agent a launch opened should be under the cursor once its row shows
    /// up, however late the fetch lands.
    #[test]
    fn a_pending_selection_lands_when_the_rows_arrive() {
        let mut a = app();
        a.tree[0].projects[0].expanded = true;
        a.tree[0].projects[0].envs[0].expanded = true;
        a.pending_select = Some("ca_2".into());
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![
                agent("ca_1", "first", "sleeping"),
                agent("ca_2", "second", "running"),
            ]),
        );
        assert_eq!(a.selected_row().unwrap().label, "second");
        assert!(a.pending_select.is_none(), "consumed once it lands");
    }

    /// Delete asks first; anything but `y` cancels. A mistyped key must never
    /// be read as consent to destroy a disk.
    #[test]
    fn delete_requires_a_yes() {
        let mut a = loaded_app();
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();

        assert_eq!(a.on_key(key(KeyCode::Char('d'))), None);
        let confirm = a.confirm.clone().expect("delete should ask");
        assert_eq!(confirm.op, AgentOp::Delete);
        assert!(confirm.question().contains("nimble-otter"));

        // A stray key cancels and runs nothing.
        assert_eq!(a.on_key(key(KeyCode::Char('j'))), None);
        assert!(a.confirm.is_none());
        assert!(a.ops.is_empty());

        a.on_key(key(KeyCode::Char('d')));
        let effect = a.on_key(key(KeyCode::Char('y'))).unwrap();
        assert_eq!(
            effect,
            Effect::Agent {
                op: AgentOp::Delete,
                agent_id: "ca_1".into(),
                environment_id: "env_prod".into()
            }
        );
        assert_eq!(a.ops.get("ca_1").copied(), Some("deleting…"));
    }

    /// Sleep and wake are reversible, so they run without a prompt — but not
    /// when they would do nothing.
    #[test]
    fn sleep_and_wake_skip_the_no_ops() {
        let mut a = loaded_app();
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();

        // The agent is running: waking is a no-op and says so.
        assert_eq!(a.on_key(key(KeyCode::Char('w'))), None);
        assert!(a.status.contains("already running"));
        assert!(a.ops.is_empty());

        let effect = a.on_key(key(KeyCode::Char('s'))).unwrap();
        assert_eq!(
            effect,
            Effect::Agent {
                op: AgentOp::Sleep,
                agent_id: "ca_1".into(),
                environment_id: "env_prod".into()
            }
        );
        assert_eq!(a.ops.get("ca_1").copied(), Some("sleeping…"));

        // While an op is in flight the row is claimed, and a second key does
        // not queue a duplicate.
        assert_eq!(a.on_key(key(KeyCode::Char('s'))), None);
        let row = a.selected_row().unwrap();
        assert_eq!(
            row.status.as_deref(),
            Some("sleeping…"),
            "the orb carries the op"
        );

        // Accepted is not arrived: the label stays up and the environment keeps
        // being asked until the agent says it is actually asleep.
        a.agent_op_finished("ca_1", "env_prod", AgentOp::Sleep, None);
        assert!(a.watching_agents());
        assert_eq!(
            a.selected_row().unwrap().status.as_deref(),
            Some("sleeping…")
        );

        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![agent("ca_1", "nimble-otter", "sleeping")]),
        );
        assert!(!a.watching_agents(), "it arrived");
        assert!(a.ops.is_empty());
        assert_eq!(
            a.selected_row().unwrap().status.as_deref(),
            Some("sleeping")
        );
    }

    /// The bug this exists for: a wake is accepted long before the VM is up,
    /// and the list keeps saying `sleeping` in the meantime. Dropping the label
    /// on the first answer made a wake that was working look like one that had
    /// failed and rolled back.
    #[test]
    fn a_wake_holds_its_label_until_the_agent_is_actually_running() {
        let mut a = loaded_app();
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![agent("ca_1", "nimble-otter", "sleeping")]),
        );

        assert!(matches!(
            a.on_key(key(KeyCode::Char('w'))),
            Some(Effect::Agent {
                op: AgentOp::Wake,
                ..
            })
        ));
        a.agent_op_finished("ca_1", "env_prod", AgentOp::Wake, None);

        // Still asleep as far as the platform is concerned. The row must not
        // flip back.
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![agent("ca_1", "nimble-otter", "sleeping")]),
        );
        assert_eq!(a.selected_row().unwrap().status.as_deref(), Some("waking…"));
        assert!(a.watching_agents(), "and it keeps asking");
        assert!(a.watch_tick().is_some(), "by refetching the environment");

        // Booting is still not up.
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![agent("ca_1", "nimble-otter", "starting")]),
        );
        assert_eq!(a.selected_row().unwrap().status.as_deref(), Some("waking…"));
        assert!(a.watching_agents());

        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![agent("ca_1", "nimble-otter", "running")]),
        );
        assert!(!a.watching_agents());
        assert!(a.ops.is_empty());
        assert_eq!(a.selected_row().unwrap().status.as_deref(), Some("running"));
    }

    /// An agent that never arrives stops being waited on, and the row goes back
    /// to reporting what the platform says rather than lying forever.
    #[test]
    fn a_wake_that_never_lands_gives_up() {
        let mut a = loaded_app();
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![agent("ca_1", "nimble-otter", "sleeping")]),
        );
        a.agent_op_finished("ca_1", "env_prod", AgentOp::Wake, None);
        a.ops.insert("ca_1".into(), "waking…");

        a.watching.get_mut("ca_1").unwrap().until = std::time::Instant::now();
        a.watch_tick();
        assert!(!a.watching_agents());
        assert!(a.ops.is_empty());
        assert!(a.status.contains("longer than expected"), "{}", a.status);
    }

    /// An agent that disappears while being waited on is not waited on.
    #[test]
    fn a_watched_agent_that_vanishes_is_dropped() {
        let mut a = loaded_app();
        a.agent_op_finished("ca_1", "env_prod", AgentOp::Wake, None);
        a.agents_loaded((0, 0, 0), Ok(Vec::new()));
        assert!(!a.watching_agents());
        assert!(a.ops.is_empty());
    }

    /// A delete has no state to settle into — the row goes away.
    #[test]
    fn a_delete_does_not_wait_for_anything() {
        let mut a = loaded_app();
        a.ops.insert("ca_1".into(), "deleting…");
        a.agent_op_finished("ca_1", "env_prod", AgentOp::Delete, None);
        assert!(!a.watching_agents());
        assert!(a.ops.is_empty());
    }

    #[test]
    fn lifecycle_keys_need_an_agent_row() {
        let mut a = loaded_app();
        // Home: letters are keys, but there is no agent under the cursor.
        a.cursor = 0;
        a.prompt_focused = false;
        assert_eq!(a.on_key(key(KeyCode::Char('d'))), None);
        assert!(a.confirm.is_none());
        assert!(a.status.contains("Select an agent"));
    }

    #[test]
    fn a_failed_op_reports_and_clears_its_label() {
        let mut a = loaded_app();
        a.ops.insert("ca_1".into(), "deleting…");
        a.agent_op_finished(
            "ca_1",
            "env_prod",
            AgentOp::Delete,
            Some("permission denied".into()),
        );
        assert!(a.ops.is_empty());
        assert!(!a.watching_agents(), "there is nothing to wait for");
        assert!(a.status.contains("permission denied"));
    }

    /// An agent as the loader builds one: no sessions fetched, collapsed.
    /// The reported bug: create a new agent, land in its session, and the tree
    /// still calls it "waking" while you are typing into it.
    ///
    /// The launcher attaches as soon as the SSH route answers, which is several
    /// seconds before the status projection reaches RUNNING (#1098). Nothing
    /// was wrong with the pane — the row was reporting a status the CLI itself
    /// had already decided not to wait for.
    #[test]
    fn an_agent_with_a_pane_open_is_not_still_waking() {
        let mut a = loaded_app();
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![agent("ca_new", "just-created", "starting")]),
        );

        let status_for = |a: &App| {
            a.rows()
                .into_iter()
                .find(|r| r.label == "just-created")
                .unwrap()
                .status
        };
        assert_eq!(
            status_for(&a).as_deref(),
            Some("starting"),
            "no pane yet: report what we know"
        );

        a.attach_session(
            super::super::session::Session::for_test("ca_new", "just-created").unwrap(),
            "ca_new".to_string(),
        );

        assert_eq!(
            status_for(&a).as_deref(),
            Some("running"),
            "a live pane is proof the agent is up, whatever the projection says"
        );
    }

    /// The override is scoped to the lag it exists for. A pane can outlive what
    /// it is attached to, and every other status alongside a live pane is news
    /// rather than latency.
    #[test]
    fn only_starting_is_overridden_by_a_live_pane() {
        let mut a = loaded_app();
        let attached: std::collections::HashMap<&str, std::time::Duration> =
            std::iter::once(("ca_1", std::time::Duration::from_secs(1))).collect();

        for status in ["sleeping", "crashed", "failed", "deleting"] {
            let agent = agent("ca_1", "nimble-otter", status);
            assert_eq!(
                displayed_status(&agent, &attached),
                status,
                "{status} must survive an attached pane"
            );
        }

        // And an ended pane stops being evidence of anything.
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![agent("ca_1", "nimble-otter", "starting")]),
        );
        let session = super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap();
        session.mark_ended();
        a.attach_session(session, "ca_1".to_string());
        assert_eq!(
            a.rows()
                .into_iter()
                .find(|r| r.label == "nimble-otter")
                .unwrap()
                .status
                .as_deref(),
            Some("starting")
        );
    }

    /// A pane opening settles the wake it was waiting on, rather than leaving
    /// `waking…` up and the 1.5s watch tick asking about an agent that is
    /// already taking keystrokes.
    #[test]
    fn attaching_settles_the_wake_it_was_waiting_for() {
        let mut a = loaded_app();
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![agent("ca_1", "nimble-otter", "starting")]),
        );
        a.ops.insert("ca_1".into(), AgentOp::Wake.pending_label());
        a.agent_op_finished("ca_1", "env_prod", AgentOp::Wake, None);
        assert!(a.watching_agents());

        a.attach_session(
            super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".to_string(),
        );

        assert!(a.ops.is_empty(), "the wake landed");
        assert!(
            !a.watching_agents(),
            "and there is nothing left to poll for"
        );
    }

    /// A sleep is not settled by a pane opening — nothing about an attach says
    /// the agent went to sleep, and dropping the label would strand the poll.
    #[test]
    fn attaching_does_not_settle_an_unrelated_op() {
        let mut a = loaded_app();
        a.ops.insert("ca_1".into(), AgentOp::Sleep.pending_label());
        a.agent_op_finished("ca_1", "env_prod", AgentOp::Sleep, None);

        a.attach_session(
            super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap(),
            "ca_1".to_string(),
        );

        assert_eq!(a.ops.get("ca_1").copied(), Some("sleeping…"));
        assert!(a.watching_agents());
    }

    /// The bound that keeps the override honest. A pane that has been open
    /// well past the measured lag is no longer evidence the agent is merely
    /// catching up — it is evidence it is stuck, and that is exactly the case
    /// the user hit. The row must go back to reporting what the platform says.
    #[test]
    fn a_long_open_pane_stops_excusing_a_starting_agent() {
        let agent = agent("ca_1", "nimble-otter", "starting");
        let attached = |secs| -> std::collections::HashMap<&str, std::time::Duration> {
            std::iter::once(("ca_1", std::time::Duration::from_secs(secs))).collect()
        };

        assert_eq!(
            displayed_status(&agent, &attached(1)),
            "running",
            "just attached: the projection is still catching up"
        );
        assert_eq!(
            displayed_status(&agent, &attached(STARTING_GRACE.as_secs() - 1)),
            "running",
            "inside the grace window"
        );
        assert_eq!(
            displayed_status(&agent, &attached(STARTING_GRACE.as_secs())),
            "starting",
            "past it, the platform's answer stands — a stuck agent must not read as running"
        );
        assert_eq!(
            displayed_status(&agent, &attached(600)),
            "starting",
            "and it stays that way"
        );
    }

    /// The grace window has to cover a real boot with room to spare, or a
    /// normal launch would flicker into looking stuck. Worst observed was
    /// 14.2s from create across ten runs, and the pane opens partway into that.
    #[test]
    fn the_grace_window_clears_a_measured_boot() {
        assert!(
            STARTING_GRACE >= std::time::Duration::from_secs(30),
            "too tight for a slow-but-healthy boot"
        );
    }

    fn agent(id: &str, name: &str, status: &str) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            status: status.into(),
            sessions: LoadSessions::NotLoaded,
            expanded: false,
        }
    }

    /// A tree pane on the left and a session pane on the right, with the
    /// borders their clicks have to land on.
    fn panes_fixture() -> PaneRects {
        let tree = PaneBox {
            x: 1,
            y: 3,
            w: 30,
            h: 10,
        };
        let session = PaneBox {
            x: 34,
            y: 3,
            w: 40,
            h: 10,
        };
        PaneRects {
            tree,
            session,
            tree_outer: PaneBox {
                x: tree.x - 1,
                y: tree.y - 1,
                w: tree.w + 2,
                h: tree.h + 2,
            },
            session_outer: PaneBox {
                x: session.x - 1,
                y: session.y - 1,
                w: session.w + 2,
                h: session.h + 2,
            },
            ..Default::default()
        }
    }

    fn session(id: &str, name: &str) -> super::super::session::Session {
        super::super::session::Session::for_test(id, name).expect("test session")
    }

    /// Opening a second session must not close the first — the whole point is
    /// several agents working at once.
    #[test]
    fn sessions_accumulate_and_the_newest_is_active() {
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        a.attach_session(session("ca_2", "quiet-harbor"), "ca_2".into());

        assert_eq!(a.sessions.len(), 2);
        assert_eq!(a.active, Some(1));
        assert_eq!(a.active_session().unwrap().agent_name, "quiet-harbor");
        assert_eq!(a.focus, ManageFocus::Session);
    }

    /// Enter on an agent that is already open switches to it instead of
    /// starting a second ssh into the same box.
    #[test]
    fn connecting_to_an_open_agent_switches_rather_than_relaunching() {
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        a.focus = ManageFocus::Tree;
        a.active = None;
        a.cursor = a
            .rows()
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session(0, 0, 0, 0, _)))
            .unwrap();

        assert_eq!(a.on_key(key(KeyCode::Enter)), None, "no launch");
        assert_eq!(a.active, Some(0));
        assert_eq!(a.focus, ManageFocus::Session);
        assert_eq!(a.sessions.len(), 1, "no duplicate session");
    }

    #[test]
    fn closing_a_session_keeps_the_active_pointer_valid() {
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "one"), "ca_1".into());
        a.attach_session(session("ca_2", "two"), "ca_2".into());

        a.take_session(1).unwrap();
        assert_eq!(a.sessions.len(), 1);
        assert_eq!(a.active, Some(0));

        a.take_session(0).unwrap();
        assert!(a.sessions.is_empty());
        assert_eq!(a.active, None);
        assert_eq!(a.focus, ManageFocus::Tree, "nothing left to focus");
        assert!(a.take_session(0).is_none(), "out of range is not a panic");
    }

    #[test]
    fn tab_moves_between_the_tree_and_the_pane() {
        let mut a = loaded_app();
        a.focus = ManageFocus::Tree;
        // Nothing open: tab has nowhere to go.
        a.on_key(key(KeyCode::Tab));
        assert_eq!(a.focus, ManageFocus::Tree);

        a.attach_session(session("ca_1", "one"), "ca_1".into());
        a.focus = ManageFocus::Tree;
        a.on_key(key(KeyCode::Tab));
        assert_eq!(a.focus, ManageFocus::Session);
    }

    /// Once the session has focus, tab belongs to the agent — coding agents
    /// bind it for completion, so intercepting it would break them. `^o` is the
    /// single reserved chord, and the only way back to the tree.
    #[test]
    fn a_focused_session_keeps_tab_and_only_ctrl_o_releases_it() {
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "one"), "ca_1".into());
        assert_eq!(a.focus, ManageFocus::Session);

        a.on_key(key(KeyCode::Tab));
        assert_eq!(a.focus, ManageFocus::Session, "tab went to the agent");
        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.focus, ManageFocus::Session, "esc went to the agent");
        a.on_key(ctrl('c'));
        assert_eq!(a.focus, ManageFocus::Session, "^c interrupts the agent");

        a.on_key(ctrl('o'));
        assert_eq!(a.focus, ManageFocus::Tree);
    }

    /// Walking onto a session row shows its pane; enter puts the keyboard in
    /// it rather than opening anything.
    #[test]
    fn moving_onto_a_session_row_switches_the_pane() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![
                ConsoleSession {
                    name: "claude-one".into(),
                    kind: "SHELL".into(),
                    command: None,
                    running: true,
                    attached: true,
                    created_at: None,
                    snapshot: None,
                },
                ConsoleSession {
                    name: "claude-two".into(),
                    kind: "SHELL".into(),
                    command: None,
                    running: true,
                    attached: true,
                    created_at: None,
                    snapshot: None,
                },
            ]);
        }
        let mut first = session("ca_1", "nimble-otter");
        first.durable_name = "claude-one".into();
        let mut second = session("ca_1", "nimble-otter");
        second.durable_name = "claude-two".into();
        a.sessions = vec![first, second];
        a.active = Some(0);
        a.focus = ManageFocus::Tree;

        let two = a
            .rows()
            .iter()
            .position(|r| r.label == "[S] claude-two")
            .unwrap();
        a.cursor = two - 1;
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.active, Some(1), "the pane follows the highlight");
        assert_eq!(a.focus, ManageFocus::Tree, "moving does not steal focus");

        // Enter drops in; it must not launch anything.
        assert_eq!(a.on_key(key(KeyCode::Enter)), None);
        assert_eq!(a.focus, ManageFocus::Session);
        assert_eq!(a.sessions.len(), 2, "no new session was opened");
    }

    /// Clicking a row with children opens it, and clicking again closes it —
    /// the one gesture every tree has.
    #[test]
    fn clicking_a_collapsible_row_toggles_it() {
        let mut a = ordering_app();
        a.screen = Screen::Manage;
        a.panes = panes_fixture();
        let alpha = a.rows().iter().position(|r| r.label == "Alpha").unwrap();

        // Closed in the fixture; a click opens it and shows its environment.
        a.on_mouse(MouseAction::Down, 5, 3 + alpha as u16);
        assert!(a.tree[0].projects[1].expanded);
        assert!(a.rows().iter().any(|r| r.label == "production"));

        // And a second click closes it again.
        a.on_mouse(MouseAction::Down, 5, 3 + alpha as u16);
        assert!(!a.tree[0].projects[1].expanded);
    }

    /// The projects tail folds and unfolds like any other branch, and its
    /// state sticks once it has been touched.
    #[test]
    fn clicking_the_tail_header_toggles_it() {
        let mut a = ordering_app();
        a.screen = Screen::Manage;
        a.panes = panes_fixture();
        let header = a
            .rows()
            .iter()
            .position(|r| matches!(r.kind, RowKind::OtherProjects))
            .unwrap();

        // Open (nothing else to show); a click folds every project away.
        a.on_mouse(MouseAction::Down, 5, 3 + header as u16);
        assert!(
            !a.rows()
                .iter()
                .any(|r| matches!(r.kind, RowKind::Project(..)))
        );

        // And a second click brings them back.
        a.on_mouse(MouseAction::Down, 5, 3 + header as u16);
        assert!(
            a.rows()
                .iter()
                .any(|r| matches!(r.kind, RowKind::Project(..)))
        );
    }

    /// Clicking a collapsed environment has to fetch its agents, the same as
    /// the keyboard does.
    #[test]
    fn clicking_an_environment_loads_its_agents() {
        let mut a = app();
        a.screen = Screen::Manage;
        a.tree[0].projects[0].expanded = true;
        a.panes = panes_fixture();
        let production = a
            .rows()
            .iter()
            .position(|r| r.label == "production")
            .unwrap();

        assert_eq!(
            a.on_mouse(MouseAction::Down, 5, 3 + production as u16),
            Some(Effect::LoadAgents {
                environment_id: "env_prod".into(),
                path: (0, 0, 0)
            })
        );
    }

    /// Clicking a session that is not connected says so rather than silently
    /// spending an ssh.
    #[test]
    fn double_clicking_a_disconnected_session_reattaches() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]);
        }
        a.panes = panes_fixture();
        let row = a
            .rows()
            .iter()
            .position(|r| r.label == "[S] claude-one")
            .unwrap();
        let effect = a.on_mouse(MouseAction::Down, 5, 3 + row as u16);
        assert!(
            matches!(effect, Some(Effect::LoadSessions { .. })),
            "the first click validates the list instead of spending an ssh: {effect:?}"
        );
        let effect = a.on_mouse(MouseAction::Down, 5, 3 + row as u16);
        assert!(
            matches!(effect, Some(Effect::Reattach { .. })),
            "a double click connects: {effect:?}"
        );
    }

    /// The reattach prompt answers the click that raised it: clicking a
    /// connected session afterwards clears it, the same as a keypress would,
    /// instead of leaving "not connected" standing over a connected pane.
    #[test]
    fn one_click_selects_and_a_double_click_connects() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![
                ConsoleSession {
                    name: "claude-one".into(),
                    kind: "SHELL".into(),
                    command: None,
                    running: true,
                    attached: true,
                    created_at: None,
                    snapshot: None,
                },
                ConsoleSession {
                    name: "claude-two".into(),
                    kind: "SHELL".into(),
                    command: None,
                    running: true,
                    attached: false,
                    created_at: None,
                    snapshot: None,
                },
            ]);
        }
        let mut pane = super::super::session::Session::for_test("ca_1", "nimble-otter").unwrap();
        pane.durable_name = "claude-one".into();
        a.sessions = vec![pane];
        a.active = Some(0);
        a.panes = panes_fixture();

        // A disconnected sibling: one click selects, says how to connect, and
        // re-asks the platform so a row that no longer exists validates away;
        // the second click (a double) reattaches.
        let two = a
            .rows()
            .iter()
            .position(|r| r.label == "[S] claude-two")
            .unwrap();
        let effect = a.on_mouse(MouseAction::Down, 5, 3 + two as u16);
        assert!(
            matches!(effect, Some(Effect::LoadSessions { .. })),
            "a click on a disconnected row validates the list: {effect:?}"
        );
        assert_eq!(a.focus, ManageFocus::Tree, "nothing to type into yet");
        assert!(a.status.contains("Double-click"), "{}", a.status);
        let effect = a.on_mouse(MouseAction::Down, 5, 3 + two as u16);
        assert!(
            matches!(effect, Some(Effect::Reattach { .. })),
            "{effect:?}"
        );

        let one = a
            .rows()
            .iter()
            .position(|r| r.label == "[S] claude-one")
            .unwrap();
        a.on_mouse(MouseAction::Down, 5, 3 + one as u16);
        assert_eq!(a.active, Some(0), "one click shows the open pane");
        assert_eq!(
            a.focus,
            ManageFocus::Tree,
            "but the keyboard stays with the tree until a double click"
        );
        a.on_mouse(MouseAction::Down, 5, 3 + one as u16);
        assert_eq!(
            a.focus,
            ManageFocus::Session,
            "a double click hands it the keyboard"
        );
    }

    /// Enter on a project toggles it: the first press opens it straight
    /// through to its first environment's agents, and a second press folds
    /// it back up instead of going dead. The cursor stays on the project so
    /// the second press lands where the first one did.
    #[test]
    fn enter_on_a_project_toggles_it() {
        let mut a = app();
        a.screen = Screen::Manage;
        a.cursor = a.rows().iter().position(|r| r.label == "devtools").unwrap();

        let effect = a.on_key(key(KeyCode::Enter));
        assert_eq!(
            effect,
            Some(Effect::LoadAgents {
                environment_id: "env_prod".into(),
                path: (0, 0, 0)
            })
        );
        assert!(a.tree[0].projects[0].expanded);
        assert!(a.tree[0].projects[0].envs[0].expanded);
        assert!(
            !a.tree[0].projects[0].envs[1].expanded,
            "only the first one"
        );
        assert_eq!(
            a.selected_row().unwrap().label,
            "devtools",
            "the cursor stays put, so enter again can close it"
        );

        assert_eq!(a.on_key(key(KeyCode::Enter)), None);
        assert!(
            !a.tree[0].projects[0].expanded,
            "the second enter folds the project"
        );
    }

    /// ⌥n floats the agent picker over the tree; enter launches the same new
    /// session `n` would have made, on the harness just chosen.
    #[test]
    fn n_picks_a_harness_then_makes_a_new_agent() {
        let mut a = app();
        a.screen = Screen::Manage;
        a.prompt_focused = false; // home: n is a key, not a letter
        a.target = Some(Target {
            project_id: "p1".into(),
            project_name: "devtools".into(),
            environment_id: "env_prod".into(),
            environment_name: "production".into(),
        });
        let opened_on = a.harness;
        assert_eq!(a.on_key(key(KeyCode::Char('n'))), None);
        assert_eq!(a.screen, Screen::HarnessPick);
        assert_eq!(
            a.harness_pick,
            Some(opened_on),
            "the picker opens on the current choice"
        );
        a.on_key(key(KeyCode::Down));
        let effect = a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Manage);
        let Some(Effect::Launch(req)) = effect else {
            panic!("expected a launch, got {effect:?}");
        };
        let picked = (opened_on + 1).min(HARNESSES.len() - 1);
        assert_eq!(
            req.harness, HARNESSES[picked],
            "the picked harness rides along"
        );
        assert!(req.force_new, "a fresh agent, like the dashboard's New");
        assert_eq!(req.agent_id, None);

        // Esc just closes it.
        a.on_key(key(KeyCode::Char('n')));
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert_eq!(a.screen, Screen::Manage);
    }

    /// ⌥n skips the picker: a new agent immediately, on whatever harness is
    /// already selected — the quick create.
    #[test]
    fn alt_n_quick_creates_a_new_agent() {
        let mut a = app();
        a.screen = Screen::Manage;
        a.target = Some(Target {
            project_id: "p1".into(),
            project_name: "devtools".into(),
            environment_id: "env_prod".into(),
            environment_name: "production".into(),
        });
        let Some(Effect::Launch(req)) = a.on_key(alt('n')) else {
            panic!("expected a launch");
        };
        assert!(req.force_new);
        assert_eq!(req.agent_id, None);
        assert_eq!(req.harness, a.harness_name());
    }

    /// ⌥p floats the menu's prompt box over the tree: type, shift+tab to
    /// change the agent, enter to spin up a session carrying the prompt.
    #[test]
    fn alt_p_composes_a_prompted_session() {
        let mut a = app();
        a.screen = Screen::Manage;
        a.target = Some(Target {
            project_id: "p1".into(),
            project_name: "devtools".into(),
            environment_id: "env_prod".into(),
            environment_name: "production".into(),
        });
        assert_eq!(a.on_key(alt('p')), None);
        assert_eq!(a.screen, Screen::ManagePrompt);
        for c in "fix it".chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
        let before = a.harness;
        a.on_key(key(KeyCode::BackTab));
        assert_eq!(a.harness, (before + 1) % HARNESSES.len());

        let effect = a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Manage);
        let Some(Effect::Launch(req)) = effect else {
            panic!("expected a launch, got {effect:?}");
        };
        assert_eq!(req.prompt.as_deref(), Some("fix it"));

        // An empty draft doesn't send: enter is a slip there, and the menu
        // box's own contract is that esc closes without launching.
        a.on_key(alt('p'));
        assert_eq!(a.on_key(key(KeyCode::Enter)), None);
        assert_eq!(a.screen, Screen::ManagePrompt, "nothing to send yet");
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert_eq!(a.screen, Screen::Manage);
    }

    /// The ⌥p composer follows the menu box's shell contract: cycling can
    /// land on shell, typing goes inert there, and enter launches the session
    /// bare instead of demanding a prompt a shell could never take.
    #[test]
    fn alt_p_launches_a_bare_shell_session() {
        let mut a = app();
        a.screen = Screen::Manage;
        a.target = Some(Target {
            project_id: "p1".into(),
            project_name: "devtools".into(),
            environment_id: "env_prod".into(),
            environment_name: "production".into(),
        });
        a.on_key(alt('p'));
        for c in "fix it".chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
        while a.harness_name() != "shell" {
            a.on_key(key(KeyCode::BackTab));
        }
        a.on_key(key(KeyCode::Char('x')));
        assert_eq!(
            a.manage_prompt.as_deref(),
            Some("fix it"),
            "typing is refused while shell is selected"
        );
        let effect = a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::Manage);
        let Some(Effect::Launch(req)) = effect else {
            panic!("expected a launch, got {effect:?}");
        };
        assert_eq!(req.harness, "shell");
        assert_eq!(req.prompt, None, "the abandoned draft must not ride along");
    }

    /// ⌥n's picker offers shell like any agent — last in the list — and
    /// picking it makes the same promptless session `n` would.
    #[test]
    fn the_picker_offers_shell_last() {
        let mut a = app();
        a.screen = Screen::Manage;
        a.prompt_focused = false; // home: n is a key, not a letter
        a.target = Some(Target {
            project_id: "p1".into(),
            project_name: "devtools".into(),
            environment_id: "env_prod".into(),
            environment_name: "production".into(),
        });
        a.on_key(key(KeyCode::Char('n')));
        for _ in 0..HARNESSES.len() {
            a.on_key(key(KeyCode::Down));
        }
        assert_eq!(
            a.harness_pick,
            Some(HARNESSES.len() - 1),
            "the cursor bottoms out on shell"
        );
        let effect = a.on_key(key(KeyCode::Enter));
        let Some(Effect::Launch(req)) = effect else {
            panic!("expected a launch, got {effect:?}");
        };
        assert_eq!(req.harness, "shell");
        assert_eq!(req.prompt, None);
        assert!(req.force_new, "the picker makes a new agent");
    }

    /// The launchers work from inside a session, which is where the thought
    /// "this needs its own session" actually arrives. The card floated over
    /// it takes the keyboard while it is up — otherwise the draft would be
    /// typed into the harness — and focus returns to the session on esc.
    #[test]
    fn alt_p_and_alt_n_reach_past_a_focused_session() {
        let mut a = loaded_app();
        a.screen = Screen::Manage;
        a.target = Some(Target {
            project_id: "p1".into(),
            project_name: "devtools".into(),
            environment_id: "env_prod".into(),
            environment_name: "production".into(),
        });
        a.sessions
            .push(super::super::session::Session::for_test("ca_1", "builder").unwrap());
        a.active = Some(0);
        a.focus = ManageFocus::Session;

        assert_eq!(a.on_key(alt('p')), None);
        assert_eq!(
            a.screen,
            Screen::ManagePrompt,
            "⌥p is not swallowed by the session"
        );
        for c in "ship it".chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            a.manage_prompt.as_deref(),
            Some("ship it"),
            "the card has the keyboard, not the harness"
        );
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert_eq!(a.screen, Screen::Manage);
        assert_eq!(
            a.focus,
            ManageFocus::Session,
            "closing the card returns to typing in the session"
        );

        // ⌥n reaches past the session too — straight to a new agent.
        assert!(matches!(a.on_key(alt('n')), Some(Effect::Launch(_))));
    }

    /// Releasing a focused session with ⇧esc also un-maximizes: focus moving
    /// to a pane that is folded away would land on something invisible.
    #[test]
    fn releasing_a_maximized_session_restores_the_tree() {
        let mut a = loaded_app();
        a.screen = Screen::Manage;
        a.sessions
            .push(super::super::session::Session::for_test("ca_1", "builder").unwrap());
        a.active = Some(0);
        a.focus = ManageFocus::Session;
        a.maximized = true;
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::SHIFT);
        assert_eq!(a.on_key(esc), None);
        assert_eq!(a.focus, ManageFocus::Tree);
        assert!(!a.maximized, "the tree pane comes back with focus");
    }

    /// Every environment is a row, not just each project's first: an agent
    /// lives in an environment, so that is what is being chosen.
    #[test]
    fn the_target_picker_lists_every_environment() {
        let a = app();
        let picker = TargetPicker::new(&a.tree, None, None);
        let labels: Vec<String> = picker.rows(None).into_iter().map(|(l, _)| l).collect();
        assert!(
            labels.contains(&"devtools (production)".to_string()),
            "{labels:?}"
        );
        assert!(
            labels.contains(&"devtools (staging)".to_string()),
            "{labels:?}"
        );
    }

    /// The default project leads the list and says so, the same as it does in
    /// the tree.
    #[test]
    fn the_target_picker_leads_with_the_default_project() {
        let a = ordering_app();
        let picker = TargetPicker::new(&a.tree, Some("p3"), None);
        let rows = picker.rows(Some("p3"));
        assert_eq!(rows[0].1, "default");
        assert!(rows[0].0.starts_with("mono"), "{rows:?}");
        assert!(
            rows[1..].iter().all(|(_, tag)| tag.is_empty()),
            "only the default is tagged"
        );
    }

    /// An environment in the tail carries no marker at all unless it is
    /// mid-load or failed — everything down there is empty, so a count would
    /// say nothing.
    #[test]
    fn tail_environments_have_no_empty_marker() {
        let mut a = app();
        let note =
            |a: &App, name: &str| a.rows().into_iter().find(|r| r.label == name).unwrap().note;
        a.tree[0].projects[0].expanded = true;
        assert_eq!(note(&a, "production"), "", "not loaded yet");

        a.agents_loaded((0, 0, 0), Ok(Vec::new()));
        assert_eq!(note(&a, "production"), "", "loaded and empty");

        a.tree[0].projects[0].envs[1].agents = Load::Loading;
        assert_eq!(note(&a, "staging"), "…");
    }

    /// The dot next to a session means "open in this UI", nothing else. The
    /// platform's own `attached` flag counts other clients too, which is why
    /// the old label flickered between attached and running.
    #[test]
    fn a_session_is_marked_connected_only_when_this_ui_has_it() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                // The agent says someone is attached; that someone is not us.
                attached: true,
                created_at: None,
                    snapshot: None,
            }]);
        }
        let row = |a: &App| {
            a.rows()
                .into_iter()
                .find(|r| r.label == "[S] claude-one")
                .unwrap()
        };
        assert_eq!(row(&a).status, None, "not open here, so no marker");
        assert!(row(&a).note.is_empty(), "the project lives in the card");

        let mut pane = session("ca_1", "nimble-otter");
        pane.durable_name = "claude-one".into();
        a.sessions = vec![pane];
        assert_eq!(row(&a).status.as_deref(), Some("connected"));
    }

    /// `x` ends the highlighted session on the agent — connected or not. It
    /// used to close only our window onto it, which looked like nothing
    /// happened.
    #[test]
    fn x_ends_the_highlighted_session() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]);
        }
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "[S] claude-one")
            .unwrap();

        assert_eq!(
            a.on_key(key(KeyCode::Char('x'))),
            Some(Effect::KillSession {
                agent_id: "ca_1".into(),
                environment_id: "env_prod".into(),
                session_name: "claude-one".into(),
            }),
            "not connected is no excuse — the session is on the VM"
        );
        // Optimistic: the row goes at once. The agent takes a second or two to
        // reap the processes, and a row that reads "running" in the meantime
        // looks like the key did nothing.
        assert!(
            !a.rows().iter().any(|r| r.label == "[S] claude-one"),
            "the session should be gone from the tree immediately"
        );
        assert!(
            a.rows().iter().any(|r| r.label == "nimble-otter"),
            "with its only thread ending, the agent row returns"
        );

        // A kill that fails puts it back rather than hiding a live session.
        a.session_killed("claude-one", Some("permission denied".into()));
        assert!(a.rows().iter().any(|r| r.label == "[S] claude-one"));
        assert!(a.status.contains("permission denied"));
    }

    /// The optimism is settled by the agent's own list: a session that is still
    /// reported stays hidden, and one that has gone stops being tracked.
    #[test]
    fn a_hidden_session_is_forgotten_once_the_agent_agrees() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
        }
        a.ending.insert("claude-one".into());

        // Still listed: keep hiding it, the kill has not landed yet.
        a.sessions_loaded(
            (0, 0, 0, 0),
            "ca_1",
            Ok(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]),
        );
        assert!(a.ending.contains("claude-one"));
        assert!(!a.rows().iter().any(|r| r.label == "[S] claude-one"));

        // Gone from the agent: stop tracking it.
        a.sessions_loaded((0, 0, 0, 0), "ca_1", Ok(Vec::new()));
        assert!(a.ending.is_empty());
    }

    /// One click on a connected session row shows the pane but keeps the
    /// keyboard in the tree — with the sidebar listing threads, most rows are
    /// connected sessions, and browsing must stay possible. A double click is
    /// what steps in to type.
    #[test]
    fn one_click_views_and_two_clicks_connect() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: true,
                created_at: None,
                    snapshot: None,
            }]);
        }
        let mut pane = session("ca_1", "nimble-otter");
        pane.durable_name = "claude-one".into();
        a.sessions = vec![pane];
        a.active = None;
        a.focus = ManageFocus::Tree;
        a.panes = panes_fixture();

        let row = a
            .rows()
            .iter()
            .position(|r| r.label == "[S] claude-one")
            .unwrap() as u16;

        a.on_mouse(MouseAction::Down, 5, 3 + row);
        assert_eq!(a.active, Some(0), "one click shows it");
        assert_eq!(a.focus, ManageFocus::Tree, "the keyboard stays in the tree");

        a.on_mouse(MouseAction::Down, 5, 3 + row);
        assert_eq!(a.focus, ManageFocus::Session, "a double click steps in");
    }

    /// A reconnect whose target session turns out not to exist launches a
    /// replacement on the same agent — same harness, stale row dropped —
    /// instead of failing. Connecting was the ask; a dead name must not
    /// answer it with an error.
    #[test]
    fn a_reconnect_to_a_gone_session_starts_a_new_one() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: Some("claude --resume".into()),
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]);
        }

        let Some(Effect::Launch(req)) =
            a.reattach_target_gone("ca_1", "nimble-otter", "claude-one")
        else {
            panic!("expected a replacement launch");
        };
        assert_eq!(req.agent_id.as_deref(), Some("ca_1"));
        assert!(req.force_new, "the dead name must not be redialed");
        assert_eq!(req.harness, "claude", "same harness the dead session ran");
        assert_eq!(req.prompt, None);
        assert!(
            !a.rows().iter().any(|r| r.label.contains("claude-one")),
            "the stale row is gone"
        );

        // The agent stopped running underneath the reconnect: nothing to
        // launch onto — the truth gets refetched instead.
        let mut b = loaded_app();
        if let Load::Loaded(agents) = &mut b.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].status = "sleeping".into();
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]);
        }
        let effect = b.reattach_target_gone("ca_1", "nimble-otter", "claude-one");
        assert!(
            matches!(effect, Some(Effect::LoadSessions { .. })),
            "{effect:?}"
        );
        assert!(!b.toast.as_ref().unwrap().ok, "announced as a failure");
    }

    /// A refresh that finds an agent no longer running drops its session
    /// list: those sessions lived on the machine, and the machine is gone.
    /// Carrying them showed reattach rows for sessions that no longer exist —
    /// forever, because the refresh cadence only re-asks running agents.
    #[test]
    fn sleep_drops_the_carried_session_list() {
        let session = ConsoleSession {
            name: "claude-one".into(),
            kind: "SHELL".into(),
            command: None,
            running: true,
            attached: false,
            created_at: None,
                    snapshot: None,
        };
        let previous = vec![Agent {
            id: "ca_1".into(),
            name: "nimble-otter".into(),
            status: "running".into(),
            sessions: LoadSessions::Loaded(vec![session]),
            expanded: true,
        }];
        let mut asleep = previous.clone();
        asleep[0].status = "sleeping".into();
        asleep[0].sessions = LoadSessions::NotLoaded;

        let merged = merge_agents(previous.clone(), asleep);
        assert_eq!(
            merged[0].sessions,
            LoadSessions::NotLoaded,
            "a sleeping machine has no console sessions to keep"
        );
        assert!(merged[0].expanded, "the fold state is still ours to keep");

        let mut still_running = previous.clone();
        still_running[0].sessions = LoadSessions::NotLoaded;
        let merged = merge_agents(previous, still_running);
        assert!(
            matches!(&merged[0].sessions, LoadSessions::Loaded(s) if s.len() == 1),
            "a running agent keeps its list until the reply replaces it"
        );
    }

    /// Clicking the session panel is itself a request to type in it.
    #[test]
    fn clicking_the_session_panel_takes_the_keyboard() {
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "one"), "ca_1".into());
        a.focus = ManageFocus::Tree;
        a.panes = panes_fixture();

        a.on_mouse(MouseAction::Down, 40, 5);
        assert_eq!(a.focus, ManageFocus::Session);
    }

    /// Four ways out of a focused session, because terminals disagree about
    /// what they report: a modified Escape needs the enhanced keyboard
    /// protocol, so `^]` and `^o` have to work without it. ⇧esc carries the
    /// pair, because macOS composes Option into a character and Escape has
    /// none to compose to, which leaves ⌥esc unsendable on a stock terminal.
    #[test]
    fn every_release_chord_works() {
        for release in [
            KeyEvent::new(KeyCode::Esc, KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        ] {
            let mut a = loaded_app();
            a.attach_session(session("ca_1", "one"), "ca_1".into());
            assert_eq!(a.focus, ManageFocus::Session);
            a.on_key(release);
            assert_eq!(a.focus, ManageFocus::Tree, "{release:?} should release");
        }

        // A bare escape still belongs to the agent — it is how you leave a
        // mode in every editor.
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "one"), "ca_1".into());
        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.focus, ManageFocus::Session);
    }

    /// `n` on an agent pins that agent, so the launch cannot wander off and
    /// create a second VM.
    #[test]
    fn n_opens_the_picker_from_any_row() {
        let mut a = loaded_app();
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        assert_eq!(a.on_key(key(KeyCode::Char('n'))), None);
        assert_eq!(a.screen, Screen::HarnessPick, "n is the New Agent button");
    }

    /// `?` opens the key list, and the next key just puts it away — reading
    /// the keys must not trigger one.
    #[test]
    fn the_key_overlay_is_a_lookup_not_a_mode() {
        let mut a = loaded_app();
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();

        assert_eq!(a.on_key(key(KeyCode::Char('?'))), None);
        assert!(a.keys_open);

        // `d` would normally ask to delete; here it only closes the overlay.
        assert_eq!(a.on_key(key(KeyCode::Char('d'))), None);
        assert!(!a.keys_open);
        assert!(a.confirm.is_none(), "a key read is not a key pressed");
    }

    /// `c` copies a command for the highlighted session, and says so when
    /// there is no session under the cursor.
    #[test]
    fn c_copies_an_ssh_command_for_the_session() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]);
        }
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "[S] claude-one")
            .unwrap();
        assert_eq!(
            a.on_key(key(KeyCode::Char('c'))),
            Some(Effect::CopySsh {
                agent_id: "ca_1".into(),
                environment_id: "env_prod".into(),
                session_name: "claude-one".into(),
            })
        );

        // Off any session row — home, where letters are keys — c has nothing
        // to copy.
        a.cursor = 0;
        a.prompt_focused = false;
        assert_eq!(a.on_key(key(KeyCode::Char('c'))), None);
        assert!(a.status.contains("Select a session"), "{}", a.status);
    }

    /// Landing on an agent opens it, so its sessions are there without a
    /// second keypress.
    #[test]
    fn highlighting_an_agent_expands_it() {
        let mut a = loaded_app();
        let agent_row = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        a.cursor = agent_row - 1;

        // Sessions unknown: arriving asks for them, which expands the row.
        let effect = a.on_key(key(KeyCode::Down));
        assert_eq!(
            effect,
            Some(Effect::LoadSessions {
                agent_id: "ca_1".into(),
                environment_id: "env_prod".into(),
                path: (0, 0, 0, 0)
            })
        );

        a.sessions_loaded(
            (0, 0, 0, 0),
            "ca_1",
            Ok(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: true,
                created_at: None,
                    snapshot: None,
            }]),
        );
        assert!(a.rows().iter().any(|r| r.label == "[S] claude-one"));
    }

    /// An agent known to have nothing running is left closed: expanding it
    /// would swap its sessions for a "no sessions" line.
    #[test]
    fn an_idle_agent_is_not_auto_expanded() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].sessions = LoadSessions::Loaded(Vec::new());
        }
        let agent_row = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        a.cursor = agent_row - 1;

        assert_eq!(a.on_key(key(KeyCode::Down)), None);
        assert!(
            !a.rows()
                .iter()
                .any(|r| r.label == "no sessions on this agent")
        );
    }

    /// Reattaching keeps the cursor on the session, not the agent above it —
    /// the session is what was opened.
    #[test]
    fn attaching_selects_the_session_row() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]);
        }
        let mut pane = session("ca_1", "nimble-otter");
        pane.durable_name = "claude-one".into();
        a.attach_session(pane, "ca_1".into());

        assert_eq!(a.selected_row().unwrap().label, "[S] claude-one");
        assert!(a.pending_select.is_none(), "the agent fallback was dropped");
    }

    /// A brand-new session doesn't wait for the platform to list it: the pane
    /// we just attached is proof, so its row appears — selected and connected
    /// — the moment the launch lands.
    #[test]
    fn a_new_session_takes_the_cursor_at_once() {
        let mut a = loaded_app();
        let mut pane = session("ca_1", "nimble-otter");
        pane.durable_name = "claude-new".into();
        a.attach_session(pane, "ca_1".into());
        let row = a.selected_row().unwrap();
        assert_eq!(
            row.label, "[S] claude-new",
            "adopted before the platform lists it"
        );
        assert_eq!(row.status.as_deref(), Some("connected"));

        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
        }
        a.sessions_loaded(
            (0, 0, 0, 0),
            "ca_1",
            Ok(vec![ConsoleSession {
                name: "claude-new".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: true,
                created_at: None,
                    snapshot: None,
            }]),
        );
        assert_eq!(a.selected_row().unwrap().label, "[S] claude-new");
    }

    /// A session with no pane reconnects directly — no provisioning flow for a
    /// box that is already set up.
    #[test]
    fn enter_on_a_closed_session_reattaches_without_provisioning() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]);
        }
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "[S] claude-one")
            .unwrap();

        assert_eq!(
            a.on_key(key(KeyCode::Enter)),
            Some(Effect::Reattach {
                agent_id: "ca_1".into(),
                agent_name: "nimble-otter".into(),
                environment_id: "env_prod".into(),
                session_name: "claude-one".into(),
            })
        );
    }

    /// A sleeping agent cannot be reattached to; say so instead of failing in
    /// ssh a second later.
    #[test]
    fn reattaching_to_a_sleeping_agent_asks_for_a_wake() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].status = "sleeping".into();
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]);
        }
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "[S] claude-one")
            .unwrap();
        assert_eq!(a.on_key(key(KeyCode::Enter)), None);
        assert!(a.status.contains("press w to wake"), "{}", a.status);
    }

    /// The count of sessions rides beside the agent's status, like a project's
    /// agent count.
    #[test]
    fn sessions_nest_under_their_agent() {
        let mut a = loaded_app();
        let rows = a.rows();
        let agent = rows.iter().find(|r| r.label == "nimble-otter").unwrap();
        assert_eq!(
            agent.status.as_deref(),
            Some("running"),
            "the orb carries the status"
        );

        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].sessions = LoadSessions::Loaded(vec![
                ConsoleSession {
                    name: "claude-one".into(),
                    kind: "SHELL".into(),
                    command: None,
                    running: true,
                    attached: false,
                    created_at: None,
                    snapshot: None,
                },
                // A finished provisioning exec is not a session anyone has.
                ConsoleSession {
                    name: "setup".into(),
                    kind: "EXEC".into(),
                    command: None,
                    running: false,
                    attached: false,
                    created_at: None,
                    snapshot: None,
                },
            ]);
        }
        let rows = a.rows();
        let agent = rows
            .iter()
            .position(|r| r.label == "nimble-otter")
            .expect("the agent heads its threads");
        let thread = rows
            .iter()
            .position(|r| r.label == "[S] claude-one")
            .expect("the running session is a child thread");
        assert_eq!(thread, agent + 1, "nested right under it: {rows:#?}");
        assert_eq!(rows[thread].depth, 1);
        assert!(
            !rows.iter().any(|r| r.label.contains("setup")),
            "a finished provisioning exec is not a thread"
        );
        assert_eq!(
            rows[agent].status.as_deref(),
            Some("running"),
            "the orb carries the status; no tag rides beside it"
        );
        assert!(rows[agent].note.is_empty());
    }

    /// An agent whose listing came back empty keeps its row and gains a
    /// dimmed "no sessions" note, so closing the last session never leaves a
    /// bare name with nothing marking it as an agent.
    #[test]
    fn an_emptied_agent_says_it_has_no_sessions() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].sessions = LoadSessions::Loaded(vec![]);
        }
        let rows = a.rows();
        let agent = rows
            .iter()
            .position(|r| r.label == "nimble-otter")
            .expect("the agent keeps its row");
        assert_eq!(
            rows[agent + 1].label, "no sessions — n starts one",
            "{rows:#?}"
        );
        assert!(!rows[agent + 1].selectable());
    }

    /// Counts appear without expanding every agent: running ones are
    /// prefetched when their environment loads, sleeping ones are not.
    #[test]
    fn running_agents_prefetch_their_sessions() {
        let mut a = app();
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![
                agent("ca_1", "awake", "running"),
                agent("ca_2", "asleep", "sleeping"),
            ]),
        );
        let effects = a.sessions_to_prefetch();
        assert_eq!(effects.len(), 1, "{effects:?}");
        assert_eq!(
            effects[0],
            Effect::LoadSessions {
                agent_id: "ca_1".into(),
                environment_id: "env_prod".into(),
                path: (0, 0, 0, 0)
            }
        );
        assert!(
            a.sessions_to_prefetch().is_empty(),
            "already in flight, not asked twice"
        );
    }

    /// A project whose last agent is gone folds itself away — an open, empty
    /// project holds space for nothing.
    #[test]
    fn an_emptied_project_collapses() {
        let mut a = app();
        a.tree[0].projects[0].expanded = true;
        a.tree[0].projects[0].envs[0].expanded = true;
        a.agents_loaded((0, 0, 0), Ok(vec![agent("ca_1", "one", "running")]));
        a.agents_loaded((0, 0, 1), Ok(Vec::new()));
        assert!(a.tree[0].projects[0].expanded, "it still has an agent");

        // The last one goes.
        a.agents_loaded((0, 0, 0), Ok(Vec::new()));
        assert!(!a.tree[0].projects[0].expanded);
        assert!(!a.tree[0].projects[0].envs[0].expanded);
    }

    /// Not while an environment is still loading: collapsing then would hide
    /// agents that are about to arrive.
    #[test]
    fn a_project_stays_open_while_an_environment_is_loading() {
        let mut a = app();
        a.tree[0].projects[0].expanded = true;
        a.tree[0].projects[0].envs[1].agents = Load::Loading;
        a.agents_loaded((0, 0, 0), Ok(Vec::new()));
        assert!(a.tree[0].projects[0].expanded);
    }

    /// The default project leads the tail and is never dimmed — it is where
    /// agents go, so it has to be findable even while empty.
    #[test]
    fn the_default_project_leads_the_tail() {
        let mut a = ordering_app();
        a.default_project = Some("p3".into());
        assert_eq!(project_order(&a), ["mono", "Alpha", "beta", "zebra"]);

        let rows = a.rows();
        let mono = rows.iter().position(|r| r.label == "mono").unwrap();
        assert_eq!(rows[mono].note, "(default)", "marked, and not dimmed");
        assert!(!rows[mono].dimmed, "the default is always findable");
    }

    /// The rule sits between the agent groups and the projects tail, and
    /// moving down skips it rather than landing on it.
    #[test]
    fn a_rule_separates_the_groups_from_the_tail() {
        let mut a = ordering_app();
        a.agents_loaded((0, 0, 0), Ok(vec![agent("ca_1", "one", "running")]));
        let rows = a.rows();
        // The last rule: the first one sits under the New Session row.
        let sep = rows
            .iter()
            .rposition(|r| matches!(r.kind, RowKind::Separator))
            .expect("a rule under the groups");
        assert!(
            matches!(rows[sep - 1].kind, RowKind::Agent(..)),
            "{rows:#?}"
        );
        assert!(matches!(rows[sep + 1].kind, RowKind::OtherProjects));
        assert!(!rows[sep].selectable(), "the rule is not a stop");

        a.screen = Screen::Manage;
        a.cursor = sep - 1;
        a.on_key(key(KeyCode::Down));
        assert!(matches!(
            a.selected_row().unwrap().kind,
            RowKind::OtherProjects
        ));
    }

    /// With no agents there are no groups, so the tail gets no rule of its
    /// own — the only one left is the launcher's, right under New Session.
    #[test]
    fn no_groups_means_no_rule() {
        let a = ordering_app();
        let seps: Vec<usize> = a
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.kind, RowKind::Separator))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(seps, vec![1], "only the launcher's rule");
    }

    /// Alphabetical, case-insensitively — a capitalised project should not
    /// sort above every lowercase one.
    #[test]
    fn projects_sort_alphabetically_when_none_have_agents() {
        let a = ordering_app();
        assert_eq!(project_order(&a), ["Alpha", "beta", "mono", "zebra"]);
    }

    /// A project that gains agents leaves the tail and leads the tree as a
    /// group — groups with something running first.
    #[test]
    fn projects_with_agents_lead_the_thread_list() {
        let mut a = ordering_app();
        // `zebra` gains a sleeping agent, `mono` a running one.
        a.agents_loaded((0, 0, 0), Ok(vec![agent("ca_1", "one", "sleeping")]));
        a.agents_loaded((0, 2, 0), Ok(vec![agent("ca_2", "two", "running")]));
        let threads: Vec<String> = a
            .rows()
            .iter()
            .filter(|r| matches!(r.kind, RowKind::Agent(..)))
            .map(|r| r.label.clone())
            .collect();
        assert_eq!(threads, ["two", "one"], "running leads");

        // Threads exist now, so the untouched tail folded itself away…
        assert_eq!(project_order(&a), Vec::<String>::new());
        // …and holds the rest once opened.
        a.others_expanded = Some(true);
        assert_eq!(project_order(&a), ["Alpha", "beta"]);

        // An environment that answers with nothing does not promote anyone.
        a.agents_loaded((0, 1, 0), Ok(Vec::new()));
        assert_eq!(project_order(&a), ["Alpha", "beta"]);
    }

    /// Empty projects are de-emphasised, and stop being so the moment they
    /// have something in them.
    #[test]
    fn projects_without_agents_are_dimmed() {
        let mut a = ordering_app();
        assert!(
            a.rows()
                .iter()
                .filter(|r| matches!(r.kind, RowKind::Project(..)))
                .all(|r| r.dimmed)
        );

        a.agents_loaded((0, 2, 0), Ok(vec![agent("ca_1", "one", "running")]));
        // With an agent, mono's thread is on the list and its project row is
        // gone from the tail.
        let rows = a.rows();
        assert!(rows.iter().any(|r| r.label == "one"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.label == "mono"));
    }

    /// Re-ordering must not move the selection to a different row: the cursor
    /// is an index, and the row under it would otherwise change.
    #[test]
    fn the_cursor_stays_on_its_row_when_the_order_changes() {
        let mut a = ordering_app();
        // `zebra` has a sleeping agent, and the cursor is on it.
        a.agents_loaded((0, 0, 0), Ok(vec![agent("ca_1", "one", "sleeping")]));
        a.cursor = a.rows().iter().position(|r| r.label == "one").unwrap();

        // `mono` gains a running agent and its group jumps above zebra's.
        a.agents_loaded((0, 2, 0), Ok(vec![agent("ca_2", "two", "running")]));
        assert_eq!(a.selected_row().unwrap().label, "one");
    }

    /// The tail header counts the projects folded under it, and says what
    /// they are other than once there are groups to be other than.
    #[test]
    fn the_tail_header_counts_its_projects() {
        let mut a = ordering_app();
        let header = |a: &App| {
            a.rows()
                .into_iter()
                .find(|r| matches!(r.kind, RowKind::OtherProjects))
                .unwrap()
        };
        assert_eq!(header(&a).label, "projects");
        assert_eq!(header(&a).note, "(4)");

        a.agents_loaded((0, 0, 0), Ok(vec![agent("ca_1", "one", "running")]));
        assert_eq!(header(&a).label, "other projects");
        assert_eq!(header(&a).note, "(3)");
    }

    /// Expanding an agent asks the platform what is running on it, every time:
    /// sessions come and go while you are looking elsewhere.
    #[test]
    fn expanding_an_agent_fetches_its_sessions() {
        let mut a = loaded_app();
        let row = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        a.cursor = row;

        let effect = a.on_key(key(KeyCode::Right)).unwrap();
        assert_eq!(
            effect,
            Effect::LoadSessions {
                agent_id: "ca_1".into(),
                environment_id: "env_prod".into(),
                path: (0, 0, 0, 0)
            }
        );
        // While the reply is on its way the agent row stays put.
        assert!(a.rows().iter().any(|r| r.label == "nimble-otter"));

        a.sessions_loaded(
            (0, 0, 0, 0),
            "ca_1",
            Ok(vec![ConsoleSession {
                name: "sess-7".into(),
                command: Some("claude".into()),
                kind: "SHELL".into(),
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]),
        );
        let rows = a.rows();
        let session_row = rows
            .iter()
            .find(|r| r.label == "[S] claude-sess-7")
            .unwrap();
        assert!(matches!(session_row.kind, RowKind::Session(0, 0, 0, 0, 0)));
        assert_eq!(session_row.depth, 1, "a child of its agent");
        assert!(
            rows.iter().any(|r| r.label == "nimble-otter"),
            "the agent still heads its threads"
        );
    }

    /// A session's label has to be readable. The platform reports the whole
    /// launch line — PATH exports, a token source, a terminal-reset printf —
    /// and only the harness invocation in the middle means anything.
    #[test]
    fn session_labels_lift_the_harness_out_of_the_launch_line() {
        let raw = concat!(
            r#"export PATH="$HOME/.local/bin:$PATH"; [ -f ~/.gh-token ] && export GH_TOKEN="$(cat ~/.gh-token)"; "#,
            "export RAILWAY_CODE_AUTOSTARTED=1; claude 'Clone the repo'; printf '\033[?25h'; exec bash -l"
        );
        let session = ConsoleSession {
            name: "claude-3habai".into(),
            kind: "SHELL".into(),
            command: Some(raw.into()),
            running: true,
            attached: true,
            created_at: None,
                    snapshot: None,
        };
        assert_eq!(
            session.short_name(),
            "claude-3habai",
            "rows are named by the session, not the launch line"
        );
        assert_eq!(session.command_summary(), "claude 'Clone the repo'");

        // Nothing recognisable: fall back to the first line, trimmed.
        let other = ConsoleSession {
            command: Some("npm run dev\nmore".into()),
            attached: false,
            created_at: None,
                    snapshot: None,
            ..session.clone()
        };
        assert_eq!(other.command_summary(), "npm run dev");

        // No command at all: the name is all there is.
        let bare = ConsoleSession {
            command: None,
            ..session.clone()
        };
        assert_eq!(bare.command_summary(), "claude-3habai");
        let blank = ConsoleSession {
            command: Some("   ".into()),
            ..session.clone()
        };
        assert_eq!(blank.command_summary(), "claude-3habai");

        // Long ones are cut rather than overflowing the pane.
        let long = ConsoleSession {
            command: Some(format!("claude '{}'", "x".repeat(400))),
            ..session
        };
        assert!(
            long.command_summary().chars().count() <= 160,
            "{}",
            long.command_summary()
        );
        assert!(long.command_summary().ends_with('…'));
    }

    /// Only what is still running is listed: finished provisioning execs, and
    /// shells that have ended — including one just killed, which should leave
    /// the list rather than linger and look like the kill failed.
    #[test]
    fn only_running_sessions_are_listed() {
        let exec = ConsoleSession {
            name: "zesty-spencer-htv".into(),
            kind: "EXEC".into(),
            command: Some("umask 077\ncat > ~/.claude-code-env".into()),
            running: false,
            attached: false,
            created_at: None,
                    snapshot: None,
        };
        assert!(!exec.is_interesting());

        let detached = ConsoleSession {
            running: true,
            ..exec.clone()
        };
        assert!(detached.is_interesting(), "a live exec is someone's work");

        let dead_shell = ConsoleSession {
            kind: "SHELL".into(),
            running: false,
            ..exec
        };
        assert!(!dead_shell.is_interesting(), "an ended shell is gone");
    }

    /// The tree hides the noise, and says so when that is all there was.
    #[test]
    fn an_agent_with_only_provisioning_execs_reads_as_empty() {
        let mut a = loaded_app();
        a.tree[0].projects[0].envs[0].expanded = true;
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(vec![ConsoleSession {
                name: "zesty-spencer-htv".into(),
                kind: "EXEC".into(),
                command: Some("umask 077".into()),
                running: false,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]);
        }
        let rows = a.rows();
        assert!(
            rows.iter().any(|r| r.label == "nimble-otter"),
            "the agent keeps its own row: {rows:#?}"
        );
        assert!(
            !rows.iter().any(|r| r.label.contains("zesty-spencer")),
            "a finished exec is not a thread"
        );
    }

    /// A second prompt is a second piece of work: it must open its own session
    /// rather than dropping you back into the first.
    #[test]
    fn a_prompt_always_wants_its_own_session() {
        let base = LaunchRequest {
            project_id: "p".into(),
            environment_id: "e".into(),
            agent_id: None,
            session_name: None,
            force_new: false,
            new_session: false,
            harness: "claude".into(),
            prompt: None,
            label: "p/e".into(),
            base: Default::default(),
        };
        // A plain connect reuses whatever is open.
        assert!(!base.wants_new_session());
        // A prompt does not.
        assert!(
            LaunchRequest {
                prompt: Some("fix the tests".into()),
                ..base.clone()
            }
            .wants_new_session()
        );
        // Nor does an explicit new agent.
        assert!(
            LaunchRequest {
                force_new: true,
                ..base.clone()
            }
            .wants_new_session()
        );
        // Reattaching is neither: it joins a named session.
        assert!(
            !LaunchRequest {
                session_name: Some("claude-ab12cd".into()),
                prompt: Some("ignored".into()),
                ..base
            }
            .wants_new_session()
        );
    }

    /// Submitting hands the task over; the box should be empty for the next
    /// one, whatever happens to the launch afterwards.
    #[test]
    fn submitting_clears_the_prompt() {
        let mut a = app();
        a.target = Some(Target {
            project_id: "p".into(),
            project_name: "p".into(),
            environment_id: "e".into(),
            environment_name: "e".into(),
        });
        for c in "fix the tests".chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
        let Effect::Launch(req) = a.on_key(key(KeyCode::Enter)).unwrap() else {
            panic!("expected a launch");
        };
        a.start_loading(&req);
        assert!(a.prompt.is_empty());
        assert_eq!(a.loading.prompt.as_deref(), Some("fix the tests"));
    }

    /// A session started from the TUI shows up under its agent without having
    /// to close and reopen the row — the platform registers it a moment after
    /// ssh connects, so the list is asked again.
    #[test]
    fn a_new_session_can_be_refreshed_into_view() {
        let mut a = loaded_app();
        // Not expanded: nobody is looking, so nothing is fetched.
        assert_eq!(a.refresh_agent_sessions("ca_1"), None);

        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
        }
        assert_eq!(
            a.refresh_agent_sessions("ca_1"),
            Some(Effect::LoadSessions {
                agent_id: "ca_1".into(),
                environment_id: "env_prod".into(),
                path: (0, 0, 0, 0)
            })
        );
        assert_eq!(a.refresh_agent_sessions("nope"), None);
    }

    /// An open pane counts as looking too. Sessions started and ended in there
    /// change the count beside the agent, and refusing to refetch a collapsed
    /// row left that count wrong until it was expanded again.
    #[test]
    fn an_agent_with_an_open_pane_refreshes_while_collapsed() {
        let mut a = with_sessions(&["ca_1"]);
        assert!(
            !a.tree[0].projects[0].envs[0].agents_vec()[0].expanded,
            "collapsed, on purpose"
        );
        assert_eq!(
            a.refresh_agent_sessions("ca_1"),
            Some(Effect::LoadSessions {
                agent_id: "ca_1".into(),
                environment_id: "env_prod".into(),
                path: (0, 0, 0, 0)
            })
        );
    }

    /// A refresh asks about the sessions of what is being looked at, and nothing
    /// else: `cloudAgentConsoleSessions` takes one agent, so a sweep would be a
    /// request per agent on every tick.
    #[test]
    fn a_refresh_only_asks_about_sessions_someone_can_see() {
        let mut a = loaded_app();
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![
                agent("ca_1", "nimble-otter", "running"),
                agent("ca_2", "quiet-one", "running"),
                agent("ca_3", "asleep", "sleeping"),
            ]),
        );
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            for agent in agents.iter_mut() {
                agent.sessions = LoadSessions::Loaded(Vec::new());
            }
            agents[0].expanded = true;
        }
        assert_eq!(
            a.sessions_to_refresh(),
            vec![Effect::LoadSessions {
                agent_id: "ca_1".into(),
                environment_id: "env_prod".into(),
                path: (0, 0, 0, 0)
            }],
            "the expanded one, not the collapsed one and not the sleeping one"
        );

        // The list is not blanked on the way out: a timer must not make the rows
        // flicker.
        assert_eq!(
            a.tree[0].projects[0].envs[0].agents_vec()[0].sessions,
            LoadSessions::Loaded(Vec::new())
        );
    }

    /// A session refresh that fails keeps the list it had, the same way the
    /// agent list does — an error where the rows were, once every 25s, would be
    /// a worse screen than a slightly old list.
    #[test]
    fn a_failed_session_refresh_keeps_the_sessions() {
        let mut a = loaded_app();
        let sessions = vec![ConsoleSession {
            name: "sess-7".into(),
            kind: "SHELL".into(),
            command: None,
            running: true,
            attached: false,
            created_at: None,
                    snapshot: None,
        }];
        a.sessions_loaded((0, 0, 0, 0), "ca_1", Ok(sessions.clone()));
        a.sessions_loaded((0, 0, 0, 0), "ca_1", Err("502 from backboard".into()));

        assert_eq!(
            a.tree[0].projects[0].envs[0].agents_vec()[0].sessions,
            LoadSessions::Loaded(sessions)
        );
        assert!(a.status.contains("502 from backboard"), "{}", a.status);
        // Quietly: a background refresh must not raise a toast per tick.
        assert!(a.toast.is_none());
    }

    /// The automatic refresh stays out of the way of everything that is already
    /// asking, or that is mid-question.
    #[test]
    fn the_auto_refresh_yields_to_everything_that_matters() {
        let mut a = loaded_app();
        a.last_refresh = Some(std::time::Instant::now() - AUTO_REFRESH_EVERY);
        assert!(a.auto_refresh_due(), "due, with nothing in the way");

        // One already in flight.
        a.refreshing = true;
        assert_eq!(a.auto_refresh_in(), None);
        a.refreshing = false;

        // A launch, which reports its own progress and refetches when it lands.
        a.loading.active = true;
        assert_eq!(a.auto_refresh_in(), None);
        a.loading.active = false;

        // A wake, which is already polling that environment every 1.5s.
        a.watching.insert(
            "ca_1".into(),
            AgentWatch {
                want: "running",
                environment_id: "env_prod".into(),
                until: std::time::Instant::now() + WAKE_PATIENCE,
            },
        );
        assert_eq!(a.auto_refresh_in(), None);
        a.watching.clear();

        // A y/N question: rows moving under it is how the wrong thing gets
        // deleted.
        a.confirm = Some(PendingConfirm {
            op: AgentOp::Delete,
            agent_id: "ca_1".into(),
            environment_id: "env_prod".into(),
            agent_name: "nimble-otter".into(),
        });
        assert_eq!(a.auto_refresh_in(), None);
        a.confirm = None;

        assert!(a.auto_refresh_due(), "and back again once they are gone");
    }

    /// A refresh that just ran is not due again for a full interval, whoever
    /// started it — pressing ⌥r pushes the automatic one out rather than having
    /// it arrive a second later.
    #[test]
    fn a_refresh_resets_the_clock() {
        let mut a = loaded_app();
        a.last_refresh = Some(std::time::Instant::now() - AUTO_REFRESH_EVERY);
        assert!(a.auto_refresh_due());

        a.on_key(alt('r'));
        a.refresh_started();
        a.refresh_finished();
        assert!(!a.auto_refresh_due());
        let remaining = a.auto_refresh_in().expect("still armed");
        assert!(
            remaining > AUTO_REFRESH_EVERY / 2,
            "nearly a full interval: {remaining:?}"
        );
    }

    /// A 429 answered by polling on schedule is how a rate limit becomes a
    /// longer rate limit: the automatic refresh waits out the Retry-After.
    #[test]
    fn a_rate_limit_pauses_the_auto_refresh() {
        let mut a = loaded_app();
        a.last_refresh = Some(std::time::Instant::now() - AUTO_REFRESH_EVERY);
        a.rate_limited(Some(90));

        assert!(!a.refreshing, "the refused refresh is over");
        let waiting = a.auto_refresh_in().expect("still armed, just later");
        assert!(
            waiting > std::time::Duration::from_secs(60),
            "waits out the window: {waiting:?}"
        );

        // Told nothing, it still waits — the alternative is asking again at once.
        let mut b = loaded_app();
        b.last_refresh = Some(std::time::Instant::now() - AUTO_REFRESH_EVERY);
        b.rate_limited(None);
        assert!(!b.auto_refresh_due());
    }

    /// Without `myCloudAgents` a refresh asks per environment — but only about
    /// the ones with rows on screen. Sweeping the account is `shift+r`, which is
    /// a deliberate act because it costs a request each.
    #[test]
    fn the_fallback_refresh_asks_only_about_answered_environments() {
        let mut a = loaded_app();
        // env_prod is loaded (loaded_app), env_stg has never been asked about.
        assert_eq!(
            a.environments_to_refresh(),
            vec![Effect::LoadAgents {
                environment_id: "env_prod".into(),
                path: (0, 0, 0)
            }]
        );

        // One still in flight is already on its way.
        a.tree[0].projects[0].envs[0].agents = Load::Loading;
        assert!(a.environments_to_refresh().is_empty());
    }

    /// ⌥enter hands the whole terminal over; `f` does the same, because
    /// plenty of terminals never send a modifier with Enter.
    #[test]
    fn full_screen_has_two_ways_in() {
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        a.focus = ManageFocus::Tree;

        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        let Some(Effect::FullScreen {
            agent_id,
            session_name,
            ..
        }) = a.on_key(alt_enter)
        else {
            panic!("expected a full-screen request");
        };
        assert_eq!(agent_id, "ca_1");
        assert_eq!(session_name, "test");

        assert!(matches!(
            a.on_key(key(KeyCode::Char('f'))),
            Some(Effect::FullScreen { .. })
        ));
    }

    /// shift+enter belongs to the harness, which reads it as the newline every
    /// text field gives you. The TUI must not claim it from either focus.
    #[test]
    fn shift_enter_is_the_harnesss_to_keep() {
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);

        // With the tree focused it is not the full-screen chord any more; the
        // row under the cursor is a workspace, so a plain Enter would expand
        // it and a claimed shift+enter would show up as something happening.
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        a.focus = ManageFocus::Tree;
        assert!(
            !matches!(a.on_key(shift_enter), Some(Effect::FullScreen { .. })),
            "shift+enter must not hand the terminal over"
        );

        // And with the session focused it falls straight through to the agent,
        // like every other key the pane does not reserve.
        let mut b = loaded_app();
        b.attach_session(session("ca_1", "nimble-otter"), "ca_1".into());
        b.focus = ManageFocus::Session;
        assert_eq!(b.on_key(shift_enter), None);
        assert_eq!(b.focus, ManageFocus::Session, "it must not release either");
    }

    /// Rows are named by the session — the seeded task stays in the detail
    /// card's command line, not the name.
    #[test]
    fn session_rows_show_the_session_name() {
        let session = ConsoleSession {
            name: "claude-3habai".into(),
            kind: "SHELL".into(),
            command: Some(
                "export PATH=x; export RAILWAY_CODE_AUTOSTARTED=1; claude 'do the thing'; exec bash -l"
                    .into(),
            ),
            running: true,
            attached: true,
            created_at: None,
                    snapshot: None,
        };
        assert_eq!(session.short_name(), "claude-3habai");
        // The command is still available, for the pane with room for it.
        assert_eq!(session.command_summary(), "claude 'do the thing'");
    }

    /// Selection is line-wise, the way a terminal selects: to the end of the
    /// first line, whole lines through the middle, and up to the cursor on the
    /// last. A rectangle would clip every line of agent output to the same
    /// columns.
    #[test]
    fn selection_is_linewise_not_rectangular() {
        let pane = PaneBox {
            x: 10,
            y: 5,
            w: 20,
            h: 6,
        };
        let selection = Selection {
            pane: ManageFocus::Session,
            anchor: (25, 5),
            cursor: (14, 7),
        };
        assert_eq!(
            selection.spans(pane),
            vec![(5, 25, 29), (6, 10, 29), (7, 10, 14)]
        );

        // Dragging backwards selects the same text.
        let backwards = Selection {
            anchor: (14, 7),
            cursor: (25, 5),
            ..selection
        };
        assert_eq!(backwards.spans(pane), selection.spans(pane));

        // One line stays one span.
        let single = Selection {
            anchor: (12, 6),
            cursor: (18, 6),
            ..selection
        };
        assert_eq!(single.spans(pane), vec![(6, 12, 18)]);

        // Rows outside the pane are dropped rather than clamped into it.
        let overflowing = Selection {
            anchor: (12, 1),
            cursor: (18, 99),
            ..selection
        };
        let spans = overflowing.spans(pane);
        assert!(
            spans.iter().all(|(y, ..)| (5..=10).contains(y)),
            "{spans:?}"
        );
    }

    /// A drag stays in the pane it began in — copying agent output must not
    /// pick up tree rows that happen to sit at the same screen rows.
    #[test]
    fn a_selection_is_confined_to_one_pane() {
        let mut a = loaded_app();
        a.panes = panes_fixture();

        // Press inside the session pane, then drag far into the tree. With no
        // session open the keyboard stays with the tree — the pane is the
        // launcher or a detail card — but the drag is still a selection.
        assert_eq!(a.on_mouse(MouseAction::Down, 40, 5), None);
        assert_eq!(a.focus, ManageFocus::Tree, "nothing there to type into");
        a.on_mouse(MouseAction::Drag, 2, 8);

        let selection = a.selection.unwrap();
        assert_eq!(selection.pane, ManageFocus::Session);
        let spans = selection.spans(a.panes.session);
        assert!(
            spans.iter().all(|(_, x0, _)| *x0 >= 34),
            "clamped to the session pane: {spans:?}"
        );

        // Releasing arms the copy for the next frame.
        a.on_mouse(MouseAction::Up, 34, 8);
        assert!(a.pending_copy.is_some());
    }

    /// Clicking another panel takes the keyboard back from a focused session —
    /// reaching for a panel with the mouse says as much as the chord does.
    #[test]
    fn clicking_away_releases_a_focused_session() {
        let mut a = loaded_app();
        a.attach_session(session("ca_1", "one"), "ca_1".into());
        a.panes = panes_fixture();
        assert_eq!(a.focus, ManageFocus::Session);

        // A click on the tree's border counts as a click on the tree.
        a.on_mouse(MouseAction::Down, 0, 2);
        assert_eq!(a.focus, ManageFocus::Tree);
    }

    #[test]
    fn clicking_the_tree_moves_the_cursor_and_focus() {
        let mut a = loaded_app();
        a.focus = ManageFocus::Session;
        a.panes = panes_fixture();
        // Row 1 is the launcher's rule, so the first row a click can land on
        // below the launcher is the group at 2.
        a.on_mouse(MouseAction::Down, 5, 5);
        assert_eq!(a.focus, ManageFocus::Tree);
        assert_eq!(a.cursor, 2, "third visible row");

        // A click that lands on nothing changes neither.
        a.on_mouse(MouseAction::Down, 200, 200);
        assert_eq!(a.focus, ManageFocus::Tree);
    }

    /// A click without a drag is not a selection; the highlight must not stick.
    #[test]
    fn a_plain_click_leaves_no_selection() {
        let mut a = loaded_app();
        a.panes = panes_fixture();
        a.on_mouse(MouseAction::Down, 5, 4);
        a.on_mouse(MouseAction::Up, 5, 4);
        assert!(a.selection.is_none());
        assert!(a.pending_copy.is_none());
    }

    /// Startup loads what a keypress needs and nothing else. Loading every
    /// environment in every project is one request each, which is what rate
    /// limited a real account.
    #[test]
    fn startup_loads_only_the_target_and_the_default_project() {
        let mut a = app();
        assert!(
            a.initial_environments().is_empty(),
            "no target and no default means nothing to load up front"
        );

        let mut a = app();
        a.target = Some(Target {
            project_id: "proj_1".into(),
            project_name: "devtools".into(),
            environment_id: "env_stg".into(),
            environment_name: "staging".into(),
        });
        let effects = a.initial_environments();
        assert_eq!(effects.len(), 1, "just the target: {effects:?}");
        assert!(matches!(
            &effects[0],
            Effect::LoadAgents { environment_id, .. } if environment_id == "env_stg"
        ));
        assert!(
            a.initial_environments().is_empty(),
            "what is already in flight is not asked for twice"
        );
    }

    /// The default project is where the tree opens and where an untargeted
    /// launch lands, so all of its environments load up front.
    #[test]
    fn startup_loads_every_environment_of_the_default_project() {
        let mut a = app();
        a.default_project = Some("proj_1".into());
        let effects = a.initial_environments();
        assert_eq!(effects.len(), 2, "production and staging: {effects:?}");
    }

    /// An agent this machine made is one you expect to find without hunting,
    /// even in a project that is neither the target nor the default.
    #[test]
    fn startup_loads_environments_this_machine_has_used() {
        let mut a = app();
        a.known_environments = vec!["env_stg".into()];
        let effects = a.initial_environments();
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0],
            Effect::LoadAgents { environment_id, .. } if environment_id == "env_stg"
        ));
    }

    /// One `myCloudAgents` reply answers for the whole account: agents land in
    /// their environments, and every other environment is loaded as empty —
    /// absence from the response means "none of yours here", not "unknown".
    #[test]
    fn my_agents_loaded_settles_every_environment() {
        let mut a = app();
        a.my_agents_loaded(
            vec![
                ("env_stg".into(), agent("ca_1", "fix-login", "running")),
                ("env_stg".into(), agent("ca_2", "bump-deps", "sleeping")),
                // An environment the tree doesn't know is ignored, not invented.
                ("env_gone".into(), agent("ca_3", "orphan", "running")),
            ],
            std::time::Instant::now(),
        );

        let stg = &a.tree[0].projects[0].envs[1].agents;
        let Load::Loaded(agents) = stg else {
            panic!("staging should be loaded: {stg:?}");
        };
        assert_eq!(
            agents.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["ca_1", "ca_2"]
        );
        assert_eq!(
            a.tree[0].projects[0].envs[0].agents,
            Load::Loaded(vec![]),
            "an environment absent from the reply has no agents of the caller's"
        );
    }

    /// A snapshot is a refresh, and a refresh has to be able to say that an
    /// agent is gone. Refusing to touch environments that already had a list is
    /// what made every full refresh a no-op, leaving relaunching the TUI as the
    /// only way to see a change made anywhere else.
    #[test]
    fn a_snapshot_replaces_what_the_tree_already_had() {
        let mut a = app();
        a.tree[0].projects[0].envs[0].agents = Load::Loaded(vec![
            agent("ca_kept", "still-here", "sleeping"),
            agent("ca_deleted", "gone-elsewhere", "running"),
        ]);
        // Still loading: its own reply has not landed to be compared, and it is
        // newer than this snapshot, so it must survive.
        a.tree[0].projects[0].envs[1].agents = Load::Loading;

        a.my_agents_loaded(
            vec![
                // Woken somewhere else, and `ca_deleted` is absent.
                ("env_prod".into(), agent("ca_kept", "still-here", "running")),
                (
                    "env_prod".into(),
                    agent("ca_new", "made-elsewhere", "running"),
                ),
                ("env_stg".into(), agent("ca_racing", "in-flight", "running")),
            ],
            std::time::Instant::now(),
        );

        let Load::Loaded(agents) = &a.tree[0].projects[0].envs[0].agents else {
            panic!("production should be loaded");
        };
        assert_eq!(
            agents.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["ca_kept", "ca_new"],
            "the snapshot decides who is there"
        );
        assert_eq!(agents[0].status, "running", "and what state they are in");
        assert_eq!(a.tree[0].projects[0].envs[1].agents, Load::Loading);
    }

    /// …but never over fresher news. A refresh in flight while an agent is
    /// deleted would otherwise put the row back — its snapshot was taken while
    /// the agent still existed — and leave it there until the next refresh.
    #[test]
    fn a_snapshot_does_not_overwrite_a_later_answer() {
        let mut a = app();
        let asked_at = std::time::Instant::now();
        // The environment answers for itself after the account-wide request went
        // out: `d` on an agent, and the refetch that proves it is gone.
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![agent("ca_kept", "the-survivor", "running")]),
        );

        a.my_agents_loaded(
            vec![
                (
                    "env_prod".into(),
                    agent("ca_kept", "the-survivor", "running"),
                ),
                (
                    "env_prod".into(),
                    agent("ca_deleted", "deleted-just-now", "running"),
                ),
            ],
            asked_at,
        );

        let Load::Loaded(agents) = &a.tree[0].projects[0].envs[0].agents else {
            panic!("production should be loaded");
        };
        assert_eq!(
            agents.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["ca_kept"],
            "the deleted agent must not come back"
        );
        // Environments that had nothing newer still take the snapshot.
        assert_eq!(a.tree[0].projects[0].envs[1].agents, Load::Loaded(vec![]));
    }

    /// The one thing a refresh must not touch: what the person looking at it
    /// has open. Every refresh used to fold expanded agents shut and drop their
    /// session lists, because the rows are rebuilt from a query that cannot know
    /// either — which is also why the 1.5s poll during a wake made the tree
    /// jump.
    #[test]
    fn a_refresh_keeps_rows_open_and_their_sessions() {
        let mut a = loaded_app();
        let (w, p, e) = (0, 0, 0);
        let sessions = LoadSessions::Loaded(vec![ConsoleSession {
            name: "claude-3s9r89".into(),
            kind: "SHELL".into(),
            command: Some("claude".into()),
            running: true,
            attached: false,
            created_at: None,
                    snapshot: None,
        }]);
        if let Load::Loaded(agents) = &mut a.tree[w].projects[p].envs[e].agents {
            agents[0].expanded = true;
            agents[0].sessions = sessions.clone();
        }
        let id = a.tree[w].projects[p].envs[e].agents_vec()[0].id.clone();

        // The same environment comes back from the platform: same agent, no idea
        // that anything is open.
        a.agents_loaded(
            (w, p, e),
            Ok(vec![Agent {
                id: id.clone(),
                name: "nimble-otter".into(),
                status: "running".into(),
                sessions: LoadSessions::NotLoaded,
                expanded: false,
            }]),
        );

        let agents = a.tree[w].projects[p].envs[e].agents_vec();
        assert!(agents[0].expanded, "the row must stay open");
        assert_eq!(agents[0].sessions, sessions, "and keep its session rows");

        // An account-wide refresh has to carry it too — that is the one that
        // runs on a timer.
        a.my_agents_loaded(
            vec![("env_prod".into(), agent(&id, "nimble-otter", "running"))],
            std::time::Instant::now(),
        );
        let agents = a.tree[w].projects[p].envs[e].agents_vec();
        assert!(agents[0].expanded);
        assert_eq!(agents[0].sessions, sessions);
    }

    /// A newcomer starts collapsed. Carrying state across ids would put one
    /// agent's sessions under another's name.
    #[test]
    fn a_refresh_does_not_lend_open_state_to_a_new_agent() {
        let mut a = loaded_app();
        if let Load::Loaded(agents) = &mut a.tree[0].projects[0].envs[0].agents {
            agents[0].expanded = true;
            agents[0].sessions = LoadSessions::Loaded(Vec::new());
        }
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![agent("ca_brand_new", "newcomer", "running")]),
        );
        let agents = a.tree[0].projects[0].envs[0].agents_vec();
        assert_eq!(agents[0].id, "ca_brand_new");
        assert!(!agents[0].expanded);
        assert_eq!(agents[0].sessions, LoadSessions::NotLoaded);
    }

    /// A sessions reply is applied to the agent it was asked about, not to
    /// whichever agent now sits at the index the request went out with — an
    /// environment refetch can insert a newer agent above it meanwhile.
    #[test]
    fn a_sessions_reply_follows_its_agent_when_the_list_shifts() {
        let mut a = loaded_app();
        // The env was refetched while ca_1's sessions were in flight: a newer
        // agent now occupies index 0.
        a.agents_loaded(
            (0, 0, 0),
            Ok(vec![
                agent("ca_new", "just-created", "starting"),
                agent("ca_1", "nimble-otter", "running"),
            ]),
        );

        a.sessions_loaded(
            (0, 0, 0, 0),
            "ca_1",
            Ok(vec![ConsoleSession {
                name: "claude-one".into(),
                kind: "SHELL".into(),
                command: None,
                running: true,
                attached: false,
                created_at: None,
                    snapshot: None,
            }]),
        );

        let Load::Loaded(agents) = &a.tree[0].projects[0].envs[0].agents else {
            panic!("loaded");
        };
        assert!(
            matches!(agents[0].sessions, LoadSessions::NotLoaded),
            "the newcomer at index 0 must not inherit ca_1's sessions"
        );
        assert!(matches!(agents[1].sessions, LoadSessions::Loaded(ref s) if s.len() == 1));

        // And a reply for an agent that no longer exists goes nowhere.
        a.sessions_loaded((0, 0, 0, 0), "ca_gone", Ok(Vec::new()));
    }

    /// One reply answers for every environment, so an account with no agents is
    /// a tree that has finished loading rather than one still waiting.
    #[test]
    fn an_empty_reply_still_settles_the_tree() {
        let mut a = app();
        a.my_agents_loaded(Vec::new(), std::time::Instant::now());
        for env in &a.tree[0].projects[0].envs {
            assert_eq!(env.agents, Load::Loaded(vec![]));
        }
    }

    /// `shift+r` is how an agent in a project nobody has opened gets found: the
    /// scan startup used to do, when the user asks for it.
    #[test]
    fn shift_r_scans_every_environment() {
        let mut a = loaded_app();
        // Off the launcher, where `R` is a letter for the prompt.
        a.on_key(key(KeyCode::Down));
        assert_eq!(
            a.on_key(key(KeyCode::Char('R'))),
            Some(Effect::ScanEverywhere)
        );

        let mut a = app();
        let effects = a.scan_environments();
        assert_eq!(effects.len(), 2, "every environment in the fixture");
        assert!(
            a.scan_environments().is_empty(),
            "a second scan must not refetch what is already in flight"
        );
    }

    /// A rate limit puts what was in flight back, so opening the row retries
    /// instead of showing a spinner that never resolves.
    #[test]
    fn a_rate_limit_releases_what_was_loading() {
        let mut a = app();
        a.default_project = Some("proj_1".into());
        assert_eq!(a.initial_environments().len(), 2);
        assert_eq!(a.tree[0].projects[0].envs[0].agents, Load::Loading);

        a.rate_limited(Some(43));
        assert_eq!(a.tree[0].projects[0].envs[0].agents, Load::NotLoaded);
        assert_eq!(a.tree[0].projects[0].envs[1].agents, Load::NotLoaded);
        let toast = a.toast.as_ref().expect("a toast");
        assert!(toast.text.contains("43s"), "{}", toast.text);
        assert!(!toast.ok, "a rate limit is not a success");

        // And asking again works, rather than being blocked by the old claim.
        assert_eq!(a.initial_environments().len(), 2);
    }

    #[test]
    fn ctrl_c_quits_from_anywhere() {
        let mut a = app();
        assert_eq!(a.on_key(ctrl('c')), Some(Effect::Quit));
        a.screen = Screen::Manage;
        assert_eq!(a.on_key(ctrl('c')), Some(Effect::Quit));
    }

    /// A response for a path that no longer exists must be dropped, not panic.
    #[test]
    fn a_stale_load_result_is_ignored() {
        let mut a = app();
        let before = a.rows();
        a.agents_loaded((9, 9, 9), Ok(vec![]));
        a.agents_loaded((0, 0, 5), Err("boom".into()));
        assert_eq!(a.rows(), before);
    }

    #[test]
    fn a_failed_load_says_so_rather_than_showing_empty() {
        let mut a = app();
        a.tree[0].projects[0].expanded = true;
        a.tree[0].projects[0].envs[0].expanded = true;
        a.agents_loaded((0, 0, 0), Err("502 from backboard".into()));
        let rows = a.rows();
        assert!(rows.iter().any(|r| r.label.contains("502 from backboard")));
        assert!(!rows.iter().any(|r| r.label == "no agents here"));
    }

    /// `r` refetches without blanking what is on screen: a group that
    /// vanished for every refresh would shove the rows — and the cursor —
    /// somewhere else once every poll tick during a wake.
    #[test]
    fn refreshing_a_group_keeps_it_on_screen() {
        let mut a = loaded_app();
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "nimble-otter")
            .unwrap();
        let effect = a.on_key(key(KeyCode::Char('r')));
        assert_eq!(
            effect,
            Some(Effect::LoadAgents {
                environment_id: "env_prod".into(),
                path: (0, 0, 0)
            })
        );
        assert!(a.rows().iter().any(|r| r.label == "nimble-otter"));
        assert_eq!(a.selected_row().unwrap().label, "nimble-otter");

        // The same when a watch polls the environment behind the scenes.
        a.reveal_environment("env_prod");
        assert!(a.rows().iter().any(|r| r.label == "nimble-otter"));
    }

    /// A refresh that fails keeps the stale list — stale beats gone — and
    /// says why in an error toast.
    #[test]
    fn a_failed_refresh_keeps_the_agents_and_reports() {
        let mut a = loaded_app();
        a.agents_loaded((0, 0, 0), Err("502 from backboard".into()));
        assert!(a.rows().iter().any(|r| r.label == "nimble-otter"));
        let toast = a.toast.as_ref().expect("an error toast");
        assert!(!toast.ok);
        assert!(toast.text.contains("502 from backboard"), "{}", toast.text);
    }

    /// A project with agents in one environment still shows its empty ones in
    /// the tail — they have to stay reachable for `n`, `t`, and `r`.
    #[test]
    fn empty_environments_of_a_grouped_project_stay_reachable() {
        let mut a = loaded_app();
        a.others_expanded = Some(true);
        let rows = a.rows();
        assert!(
            rows.iter()
                .any(|r| r.kind == RowKind::Environment(0, 0, 1) && r.label == "staging"),
            "{rows:#?}"
        );
        // The occupied environment is a group above, not a tail row too.
        assert!(!rows.iter().any(|r| r.kind == RowKind::Environment(0, 0, 0)));

        // Once every environment has agents the project leaves the tail.
        a.agents_loaded((0, 0, 1), Ok(vec![agent("ca_9", "niner", "running")]));
        assert!(
            !a.rows()
                .iter()
                .any(|r| matches!(r.kind, RowKind::Project(..)))
        );
    }

    /// The empty state advertises `n`, so `n` must work from where the cursor
    /// starts: with a target it launches there, without one it asks for one.
    #[test]
    fn a_new_agent_without_a_target_asks_for_one_first() {
        let mut a = app();
        a.screen = Screen::Manage;
        a.on_key(key(KeyCode::Down));
        a.on_key(key(KeyCode::Char('n')));
        assert_eq!(a.screen, Screen::HarnessPick);
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.screen, Screen::TargetPick, "no target: ask for one");
    }

    /// Loading an environment from the tail can promote it into a group; the
    /// cursor follows it up rather than being stranded in the folded tail.
    #[test]
    fn the_cursor_follows_an_environment_promoted_to_a_group() {
        let mut a = app();
        a.screen = Screen::Manage;
        a.cursor = a.rows().iter().position(|r| r.label == "devtools").unwrap();
        a.on_key(key(KeyCode::Right));
        a.cursor = a
            .rows()
            .iter()
            .position(|r| r.label == "production")
            .unwrap();
        a.on_key(key(KeyCode::Right));
        a.agents_loaded((0, 0, 0), Ok(vec![agent("ca_1", "one", "running")]));
        // The environment left the tail for the thread list; the cursor lands
        // on a row that still exists rather than the one that vanished.
        assert!(a.selected_row().unwrap().selectable(), "{:#?}", a.rows());
    }

    /// "None yet" is a definitive claim: the hint searches while anything is
    /// unanswered, owns up to failures, and only then declares the account
    /// empty.
    #[test]
    fn the_hint_waits_for_answers_before_claiming_empty() {
        let mut a = app();
        // The hint sits under the pinned New Session row and its rule.
        let hint = |a: &App| a.rows()[2].label.clone();
        assert_eq!(hint(&a), "looking for cloud agents…", "not loaded");

        a.tree[0].projects[0].envs[0].agents = Load::Loading;
        assert_eq!(hint(&a), "looking for cloud agents…", "still in flight");

        a.agents_loaded((0, 0, 0), Ok(Vec::new()));
        a.agents_loaded((0, 0, 1), Err("boom".into()));
        assert_eq!(hint(&a), "couldn't check every environment — r retries");

        a.agents_loaded((0, 0, 1), Ok(Vec::new()));
        assert_eq!(hint(&a), "no threads yet — the prompt above starts one");
    }

    fn offer() -> SshKeyOffer {
        SshKeyOffer {
            name: "id_ed25519".into(),
            fingerprint: "SHA256:abc".into(),
            public_key: "ssh-ed25519 AAAA test".into(),
        }
    }

    fn launch_req() -> LaunchRequest {
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

    /// An unregistered key holds the connect behind the gate; nothing is
    /// launched until the question is answered.
    #[test]
    fn an_unregistered_key_holds_the_connect() {
        let mut a = app();
        a.ssh_key = SshKeyState::NeedsRegistration(offer());
        let held = HeldConnect::Launch(launch_req());
        assert!(a.hold_for_ssh_key(held.clone()));
        assert_eq!(
            a.ssh_gate,
            Some(SshGate {
                offer: offer(),
                then: Some(held),
            })
        );
    }

    /// A registered key, and a check that never answered, both let the
    /// connect through — the launch pipeline is the better place to fail.
    #[test]
    fn ready_and_unknown_keys_proceed() {
        let mut a = app();
        a.ssh_key = SshKeyState::Ready;
        assert!(!a.hold_for_ssh_key(HeldConnect::Launch(launch_req())));
        a.ssh_key = SshKeyState::Unknown;
        assert!(!a.hold_for_ssh_key(HeldConnect::Launch(launch_req())));
        assert!(a.ssh_gate.is_none());
    }

    /// With nothing local to offer, the connect is refused with the recipe
    /// rather than held behind a question that has no good answer.
    #[test]
    fn no_local_keys_refuses_with_the_recipe() {
        let mut a = app();
        a.ssh_key = SshKeyState::NoLocalKeys;
        assert!(a.hold_for_ssh_key(HeldConnect::Launch(launch_req())));
        assert!(a.ssh_gate.is_none());
        assert!(a.toast.as_ref().is_some_and(|t| !t.ok), "{:?}", a.toast);
    }

    /// `y` answers the gate with the register effect, carrying the held
    /// connect along to resume after the mutation.
    #[test]
    fn yes_registers_and_carries_the_held_connect() {
        let mut a = app();
        a.ssh_key = SshKeyState::NeedsRegistration(offer());
        assert!(a.hold_for_ssh_key(HeldConnect::Launch(launch_req())));
        let effect = a.on_key(key(KeyCode::Char('y')));
        assert_eq!(
            effect,
            Some(Effect::RegisterSshKey {
                offer: offer(),
                then: Some(HeldConnect::Launch(launch_req())),
            })
        );
        assert!(a.ssh_gate.is_none());
    }

    /// Anything that isn't a yes cancels: a mistyped key must never register
    /// a credential on the account.
    #[test]
    fn anything_else_cancels_the_gate() {
        let mut a = app();
        a.ssh_key = SshKeyState::NeedsRegistration(offer());
        assert!(a.hold_for_ssh_key(HeldConnect::Launch(launch_req())));
        assert_eq!(a.on_key(key(KeyCode::Enter)), None);
        assert!(a.ssh_gate.is_none());
        // The state is untouched, so the next connect asks again.
        assert_eq!(a.ssh_key, SshKeyState::NeedsRegistration(offer()));
    }

    /// Setup's offer has no held connect: declining it is not an error and
    /// says nothing about a cancelled launch.
    #[test]
    fn the_setup_offer_declines_quietly() {
        let mut a = app();
        a.ssh_key = SshKeyState::NeedsRegistration(offer());
        a.offer_ssh_key_setup();
        assert!(a.ssh_gate.is_some());
        assert_eq!(a.on_key(key(KeyCode::Esc)), None);
        assert!(a.toast.is_none(), "{:?}", a.toast);

        // With the key already registered, setup offers nothing.
        a.ssh_key = SshKeyState::Ready;
        a.offer_ssh_key_setup();
        assert!(a.ssh_gate.is_none());
    }
}
