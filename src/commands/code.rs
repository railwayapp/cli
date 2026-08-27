use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use colored::Colorize;
use is_terminal::IsTerminal;

use crate::client::{GQLClient, post_graphql};
use crate::commands::cloud_agent::mcp_sync;
use crate::commands::cloud_agent::prefs::{AgentPrefs, DefaultProject};
use crate::commands::cloud_agent::skills_sync;
use crate::commands::sandbox::{resolve_project_and_env, variables_to_input};
use crate::commands::ssh::tel as ssh_tel;
use crate::commands::ssh::{
    ensure_ssh_key_quiet, probe_native_ssh, run_native_ssh_captured, run_native_ssh_with_opts,
};
use crate::config::Configs;
use crate::controllers::project::get_project;
use crate::errors::RailwayError;
use crate::gql::{mutations, queries};
use crate::macros::is_stdout_terminal;
use crate::util::progress::create_shimmer_spinner;
use crate::util::shell::shell_join;

// ---------------------------------------------------------------------------
// `railway code --codex` / `railway code --claude` / `railway code --grok` /
// `railway code --railway` — launch a coding agent on a Railway cloud agent
// VM, on the user's own plan.
//
// The VM does most of the work. `cloud-agent-base` bakes every harness
// (claude, codex, grok, cursor, droid, opencode, pi, railway-agent), and the
// `express-agent serve --agents` entrypoint reconciles their config on every
// boot: MCP servers (including Railway's own platform tools), hooks, the
// onboarding/trust flags, and the autonomy posture. So this command installs
// nothing, updates nothing, and seeds no harness config — doing any of that
// would fight the reconciler for ownership of the same files. What is left is
// the one thing only the user's laptop has: their credential.
//
// Auth shape: Codex copies the user's existing local sign-in
// (`~/.codex/auth.json`) — the flow OpenAI documents for remote machines.
// Grok does the same with `~/.grok/auth.json`. Claude uses a deliberate
// long-lived token (`claude setup-token` output, or an ANTHROPIC_API_KEY) —
// never the local sign-in's `.credentials.json`, whose refresh token two
// machines can't safely share. Anthropic documents setup-token as the mechanism
// for "CI pipelines, scripts, or other environments where interactive browser
// login isn't available", and their own claude-code-action has Pro/Max users
// mint locally, store the token, and use it on remote runners — the same shape
// as this command. Railway's own harness is the odd one out: there is no local
// sign-in to bring, because the VM already carries a server-minted LLM relay
// credential and Railway platform tools, reconciled the same way as skills and
// MCP config — see `Agent::Railway`.
//
// All three are a convenience, not a requirement. Carrying the credential saves
// signing in twice; when the local half isn't there — no `~/.codex/auth.json`,
// no `~/.grok/auth.json`, no local `claude` to mint with — the launch goes ahead
// without one and the harness asks for a sign-in on the agent, exactly as it
// would on any new machine. Every harness here has a working device/browser
// sign-in of its own, so refusing to launch would cost the user a session over
// a file they never had to have.
//
// Every credential is announced to the user, read client-side, and rides ssh
// stdin into a 0600 file on the VM: deliberately NOT a create-time variable, so
// it never appears in an argv, a Railway variable, the VM spec, an image, or
// server-side config. That is a security property and also what keeps this
// inside Anthropic's credential-use policy: the token is consumed by Claude Code
// itself, and Railway neither offers a Claude.ai login nor routes model requests
// through the user's subscription. Minting server-side, storing the token on
// Railway, or proxying inference would each cross that line.
//
// A minted setup-token is cached locally (~/.railway/claude-code-token, 0600) so
// later runs skip the OAuth round-trip, and an agent that already holds a
// credential is left alone — a setup-token is valid a year and the agent's disk
// survives sleep. `--refresh-auth` forces a re-mint; `railway logout` drops the
// cache.
//
// Lifecycle: agents are durable and have no idle timeout, so unlike a sandbox
// nothing eventually reaps one. Disconnecting leaves the agent RUNNING —
// sleeping kills every process on the VM (including durable sessions the
// platform keeps listing as reattachable), so it is a deliberate act:
// `railway ca sleep`, or `s` on the TUI tree.
// ---------------------------------------------------------------------------

/// `railway code` is the launcher: it answers "where, and which harness"
/// from flags and preferences, then opens that session. On a terminal it opens
/// it inside `railway ca`'s manage screen with the tree collapsed, so the
/// session has the whole window and the rest of the tool is one key away;
/// everywhere else it hands the terminal straight to ssh.
pub type Args = LaunchArgs;

pub async fn command(args: Args) -> Result<()> {
    // `railway code` passes its trailing arguments to the agent, so
    // `railway code setup` would silently run `setup` inside the VM. That is
    // never what someone typing it meant, and the failure is invisible — the
    // agent starts, does something odd, and nothing mentions preferences.
    if args.agent_args.first().is_some_and(|a| a == "setup") {
        bail!(
            "`railway code` passes arguments straight to the agent, so this would run `setup` on the VM.\nDid you mean `railway ca setup`?"
        );
    }
    match args.wants_pane() {
        true => crate::commands::cloud_agent::launch_in_pane(args).await,
        false => launch(args).await,
    }
}

/// Launch a coding agent on a Railway cloud agent VM
//
// `Default` is derived so the TUI can build a launch without going through
// clap: every field is an Option/Vec/bool, so the derive produces exactly the
// "nothing was passed" state clap would. `Clone` is what lets a command-line
// launch ride into the TUI as the base of a `LaunchRequest`, so flags the TUI
// has no way to ask for — `--name`, `--variable` — still reach the pipeline.
// Both kept out of the doc comment — clap renders those as `long_about` and
// they would show up in `--help`.
#[derive(Parser, Default, Clone, Debug, PartialEq, Eq)]
#[clap(
    after_help = "Examples:\n\n  railway ca                        # launch your configured default\n  railway ca setup                  # choose the default agent and skills\n  railway code --codex              # agent VM + your local Codex sign-in\n  railway code --claude             # agent VM + your Claude setup-token\n  railway code --grok               # agent VM + your local Grok sign-in\n  railway code --railway            # agent VM + Railway's own agent, no sign-in needed\n  railway code --codex --new        # force a fresh agent instead of reusing\n  railway code --codex --new --variable DB_URL=postgres.DATABASE_URL\n  railway code --codex --new --env-file .env\n  railway code --codex -- exec \"explain this codebase\"\n\nWith no agent flag, the default saved by `railway ca setup` is used\n(RAILWAY_CA_AGENT overrides it for one run). With no project or environment\nflag, this directory's linked project is used, and your default project when\nthe directory has no link.\n\nOn a terminal the session opens inside `railway ca`'s manage screen with the\ntree collapsed, so it has the whole window and the other agents are one key\naway — ⌥f brings the tree back, ⌥n starts another session. `--rm`, a `--`\npassthrough, and anything piped take the terminal directly instead; so does\n`railway ca start`, which never draws the TUI.\n\nAgents persist between runs and stay running when you disconnect, so your\nsessions survive to reattach to. `railway ca sleep <agent>` stops the compute\nbill; `railway code --rm` destroys it.\n\nClaude auth is minted once (`claude setup-token`), cached locally, and reused —\nincluding the copy already on a reused agent. `--refresh-auth` clears both\ncaches and re-mints.\n\nCarrying a sign-in from this machine is a convenience, not a requirement: with\nnothing local to copy or mint from, the agent still starts and the harness asks\nyou to sign in there.\n\nNote: requires the CLOUD_AGENTS feature to be enabled."
)]
pub struct LaunchArgs {
    /// Launch OpenAI Codex, carrying your local ChatGPT sign-in
    /// (~/.codex/auth.json) when there is one to carry
    #[clap(long)]
    codex: bool,

    /// Launch Claude Code — runs `claude setup-token` for you to mint a
    /// token for the VM (CLAUDE_CODE_OAUTH_TOKEN / ANTHROPIC_API_KEY env
    /// variables skip that when set)
    #[clap(long)]
    claude: bool,

    /// Launch Grok CLI, carrying your local sign-in (~/.grok/auth.json) when
    /// there is one to carry
    #[clap(long)]
    grok: bool,

    /// Launch Railway's own agent — no sign-in needed; it uses credentials
    /// already on the VM
    #[clap(long)]
    railway: bool,

    /// Always create a fresh agent instead of reusing this environment's
    #[clap(long)]
    pub new: bool,

    /// Accepted for compatibility; agents now always stay running on
    /// disconnect. `railway ca sleep` stops the compute bill
    #[clap(long, hide = true)]
    keep_awake: bool,

    /// Destroy this environment's agent and exit. Its disk goes with it.
    /// Superseded by `railway ca delete`, which can name any agent and asks
    /// before it destroys one
    #[clap(long)]
    rm: bool,

    /// Re-mint the Claude credential even if the agent already has a working
    /// one, clearing our local token cache first. Use after revoking a token,
    /// or when auth fails on an existing agent
    #[clap(long)]
    refresh_auth: bool,

    /// Name for a newly created agent (defaults to a generated one)
    #[clap(long)]
    name: Option<String>,

    /// Set a variable on the agent (repeatable, comma-separable). Values
    /// may reference other variables — `DB_URL=postgres.DATABASE_URL` or the
    /// full `${{postgres.DATABASE_URL}}` form — resolved server-side at
    /// create time. Applies to newly created agents (combine with --new)
    #[clap(long = "variable", value_name = "KEY=VALUE[,KEY=VALUE...]")]
    variables: Vec<String>,

    /// Load variables from a .env file (repeatable). `--variable` flags
    /// override file entries with the same key
    #[clap(long = "env-file", value_name = "PATH")]
    env_files: Vec<std::path::PathBuf>,

    /// Environment name or ID (defaults to the linked environment)
    #[clap(long, short)]
    pub environment: Option<String>,

    /// Project ID (defaults to the linked project)
    #[clap(long, short)]
    pub project: Option<String>,

    /// Extra arguments passed through to the agent (after `--`)
    #[clap(trailing_var_arg = true)]
    agent_args: Vec<String>,

    /// A task to hand the agent as it starts. Set by the TUI's prompt box, not
    /// a flag: on the command line the same thing is `-- exec "…"`, which has
    /// different semantics (it exits when the agent finishes, where a seeded
    /// session stays interactive).
    #[clap(skip)]
    pub initial_prompt: Option<String>,

    /// Use exactly this agent. Set by the TUI, which knows which row the user
    /// was on; the CLI has no flag for it because there is nothing to name an
    /// agent by on a command line.
    #[clap(skip)]
    pub agent_id: Option<String>,

    /// Launch no harness at all — just the VM's login shell. Set by the TUI's
    /// shell option, not a flag: the CLI already has a spelling for this
    /// (`railway ca ssh <agent> -- bash`), and a second one would compete
    /// with it.
    #[clap(skip)]
    pub shell: bool,

    /// Provision for an external app rather than for a session this CLI opens:
    /// seed the credential and the skills, but leave the login shell alone. Set
    /// by `railway ca desktop`; there is no flag because on its own it would
    /// prepare an agent and then do nothing with it.
    #[clap(skip)]
    pub app_mode: bool,
}

impl LaunchArgs {
    /// No flags, no arguments — the invocation that means "just open the
    /// front door" rather than "launch this exact thing".
    pub fn is_bare(&self) -> bool {
        !self.codex
            && !self.claude
            && !self.grok
            && !self.railway
            && !self.new
            && !self.keep_awake
            && !self.rm
            && !self.refresh_auth
            && self.name.is_none()
            && self.environment.is_none()
            && self.project.is_none()
            && self.initial_prompt.is_none()
            && self.variables.is_empty()
            && self.env_files.is_empty()
            && self.agent_args.is_empty()
            && !self.shell
    }

    /// Should this launch open in the TUI's session pane rather than taking
    /// the terminal for itself?
    ///
    /// Yes for the shapes a person types at a prompt, which is nearly all of
    /// them: the pane gives the session the whole window and leaves the tree,
    /// the other agents and the lifecycle keys one chord away. No for the
    /// three that a frame would break or spoil:
    ///
    /// - `--rm` destroys an agent and prints; there is no session to show.
    /// - `-- args` execs the agent and exits with its status, which is a
    ///   caller asking for an exit code, not for a window.
    /// - no terminal at all — a TUI in a pipe is gibberish, and scripted
    ///   callers reasonably expect the launcher.
    pub fn wants_pane(&self) -> bool {
        self.pane_shaped() && is_stdout_terminal()
    }

    /// The flag half of [`Self::wants_pane`], split off the terminal check so
    /// the rule is checked by tests rather than by reading it — `cargo test`
    /// captures stdout, so the whole predicate is always false under one.
    fn pane_shaped(&self) -> bool {
        !self.rm && self.agent_args.is_empty()
    }

    /// Force one harness, overriding preferences — how the TUI passes the
    /// choice made in its prompt footer.
    pub fn set_harness(&mut self, slug: &str) {
        self.claude = slug == "claude";
        self.codex = slug == "codex";
        self.grok = slug == "grok";
        self.railway = slug == "railway";
        self.shell = slug == "shell";
    }

    /// The launch the TUI asks for: an explicit project and environment, an
    /// explicit harness, and optionally a task to start with. A constructor
    /// rather than struct-update syntax so the flag fields stay private and
    /// this is the only way in from outside the module.
    pub fn for_target(
        project_id: String,
        environment_id: String,
        harness: &str,
        force_new: bool,
        prompt: Option<String>,
        agent_id: Option<String>,
    ) -> Self {
        Self::default().retargeted(
            project_id,
            environment_id,
            harness,
            force_new,
            prompt,
            agent_id,
        )
    }

    /// [`Self::for_target`] over an existing set of flags instead of an empty
    /// one — how a `railway code` invocation that opened in the pane gets its
    /// remaining flags to the pipeline.
    ///
    /// Everything the TUI decides is overwritten: it knows the target, the
    /// harness and the agent better than the command line did, because the
    /// user may have moved since typing it. Everything else survives, which is
    /// the point — `railway code --new --name api --variable K=V` creates the
    /// agent the command line described, even though no card asks for a name.
    pub fn retargeted(
        mut self,
        project_id: String,
        environment_id: String,
        harness: &str,
        force_new: bool,
        prompt: Option<String>,
        agent_id: Option<String>,
    ) -> Self {
        self.project = Some(project_id);
        self.environment = Some(environment_id);
        self.new = force_new;
        self.initial_prompt = prompt;
        self.agent_id = agent_id;
        self.set_harness(harness);
        self
    }

    /// The provision `railway ca desktop` asks for: seed this harness onto an
    /// agent and stop, leaving the sessions to an external app.
    ///
    /// Project and environment stay optional here, unlike [`for_target`]: the
    /// TUI always knows which row it was on, while this command is usually run
    /// from a linked directory and should resolve the target the same way a bare
    /// `railway code` does.
    ///
    /// [`for_target`]: Self::for_target
    pub fn for_app_mode(
        harness: &str,
        project: Option<String>,
        environment: Option<String>,
    ) -> Self {
        let mut args = Self {
            project,
            environment,
            app_mode: true,
            ..Self::default()
        };
        args.set_harness(harness);
        args
    }
}

/// The coding agent to launch, and the two things that differ between them:
/// where the local sign-in lives, and how its credential is written on the VM.
/// Installing and configuring the harness is the image's job, not ours.
///
/// `Railway` is the exception to both: it is Railway's own harness (built on
/// `railway-agent`/pi-rs), and the VM already carries everything it needs —
/// an LLM relay credential and Railway platform tools — minted server-side at
/// create time, the same way skills and MCP config are reconciled by
/// express-agent. There is no local sign-in to copy or mint, so it needs none
/// of the client-side credential machinery the other three do.
///
/// `Shell` is not a harness at all: the session is the VM's login shell and
/// nothing else starts. No credential, no autostart retarget — just the
/// machine.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Agent {
    Codex,
    Claude,
    Grok,
    Railway,
    Shell,
}

impl Agent {
    /// The remote binary name — what's actually exec'd, and what's
    /// autostarted on reconnect. Only ever used for that: anywhere this agent
    /// needs a name a person reads, use [`Self::slug`] instead. The one
    /// exception to "identical to the slug": the interactive frontend binary
    /// is `railway-agent-tui`, not `railway-agent` (that name is the headless
    /// `run`/`serve` CLI it drives).
    fn name(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::Claude => "claude",
            Agent::Grok => "grok",
            Agent::Railway => "railway-agent-tui",
            // What the session runs and what the readiness probe checks;
            // never autostarted, because `Shell` skips the autostart record.
            Agent::Shell => "bash",
        }
    }

    /// The slug persisted in `agent-prefs.json`, accepted by
    /// `RAILWAY_CA_AGENT`, and used anywhere this agent needs a short,
    /// user-facing identifier — session name prefixes, the "get back in"
    /// hint, launch messages. Identical to [`Self::name`] for every agent
    /// except Railway's own: "railway" reads better than "railway-agent-tui"
    /// in a flag, a config file, or a session name, and there is only the one
    /// harness it could mean.
    fn slug(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::Claude => "claude",
            Agent::Grok => "grok",
            Agent::Railway => "railway",
            Agent::Shell => "shell",
        }
    }

    fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "grok" => Some(Agent::Grok),
            "railway" => Some(Agent::Railway),
            "shell" => Some(Agent::Shell),
            _ => None,
        }
    }

    /// Human-facing product name for announce/error copy.
    fn display(self) -> &'static str {
        match self {
            Agent::Codex => "Codex",
            Agent::Claude => "Claude Code",
            Agent::Grok => "Grok",
            Agent::Railway => "Railway",
            Agent::Shell => "a plain shell",
        }
    }

    /// Unreachable for `Railway`: it is never asked for a credential seed —
    /// see [`Agent`]'s doc comment — so `prepare_inner` never sets
    /// `write_credential` for it.
    fn credential_seed(self) -> &'static str {
        match self {
            Agent::Codex => CODEX_SEED,
            Agent::Claude => CLAUDE_SEED,
            Agent::Grok => GROK_SEED,
            Agent::Railway | Agent::Shell => "",
        }
    }

    /// [`credential_seed`] for a script whose stdin carries more than the
    /// credential: reads exactly `len` bytes so the rest of the stream stays
    /// available to the next reader (the skills tarball on a fresh agent).
    /// Mirrors the `cat >` seeds above; the 0600 mode comes from the script's
    /// `umask 077`, with claude's explicit chmod kept as belt-and-suspenders.
    fn credential_seed_framed(self, len: usize) -> String {
        match self {
            Agent::Codex => format!("mkdir -p ~/.codex\nhead -c {len} > ~/.codex/auth.json"),
            Agent::Claude => {
                format!("head -c {len} > ~/.claude-code-env\nchmod 600 ~/.claude-code-env")
            }
            Agent::Grok => format!("mkdir -p ~/.grok\nhead -c {len} > ~/.grok/auth.json"),
            Agent::Railway | Agent::Shell => "true".to_string(),
        }
    }

    /// The local sign-in file this command copies, as `$HOME`-relative
    /// components. `None` for Claude, whose credential is minted rather than
    /// copied — sharing the local sign-in's rotating refresh token across two
    /// machines is the thing the setup-token exists to avoid — and for
    /// Railway's own harness, whose credential the VM is given at create time.
    fn local_signin_file(self) -> Option<[&'static str; 2]> {
        match self {
            Agent::Codex => Some([".codex", "auth.json"]),
            Agent::Grok => Some([".grok", "auth.json"]),
            Agent::Claude | Agent::Railway | Agent::Shell => None,
        }
    }

    /// What to tell someone whose launch carries no credential: the harness
    /// will ask them to sign in on the agent, and this is how that goes. Each
    /// one has a browser/device flow that works fine from a VM.
    ///
    /// Railway's own harness never reaches this — it resolves to
    /// [`PendingAuth::None`] before anything asks for a hint — but the arm
    /// states the reason rather than leaving the match to guess at one.
    fn sign_in_on_agent_hint(self) -> &'static str {
        match self {
            Agent::Codex => "sign in there with `codex login --device-auth`",
            Agent::Claude => "sign in there with `/login`",
            Agent::Grok => "sign in there when it asks",
            Agent::Railway => "no sign-in needed — the agent carries its own",
            Agent::Shell => "no sign-in needed — nothing starts but a shell",
        }
    }
}

/// Codex-specific VM seed: the credential arrives on stdin into a 0600 file
/// (never an argv), and that is all. Folder trust, the autonomy posture
/// (`approval_policy`/`sandbox_mode`) and the MCP servers are reconciled into
/// `~/.codex/config.toml` on every boot by express-agent, and codex's hooks
/// come from the image's policy file at `/etc/codex/requirements.toml`.
/// bubblewrap ships in the image too, so the old apt-install warning fix is
/// gone.
const CODEX_SEED: &str = r#"mkdir -p ~/.codex
cat > ~/.codex/auth.json"#;

/// Claude-specific VM seed. The credential is one `KEY=VALUE` line —
/// `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token`, or a passed-through
/// `ANTHROPIC_API_KEY` — arriving on stdin into a 0600 env file that login
/// shells and the launch prefix source (claude reads the env var; the key
/// name rides the payload so both var names work without baking either into
/// the script).
///
/// `~/.claude.json`'s `hasCompletedOnboarding` — the flag that stops claude
/// ignoring the env token and showing the first-run login picker — is
/// express-agent's to seed, along with per-project trust for `$HOME` and
/// `/app`. It runs at boot, before this command can connect.
const CLAUDE_SEED: &str = r#"cat > ~/.claude-code-env
chmod 600 ~/.claude-code-env"#;

/// Always source the carried setup-token. Claude prefers
/// `CLAUDE_CODE_OAUTH_TOKEN` over `~/.claude/.credentials.json`, so this is
/// what makes the local cache the session credential. A later `/login` on
/// the agent cannot outrank it — that path did not work correctly.
const CLAUDE_ENV_GUARD: &str =
    r#"[ -f "$HOME/.claude-code-env" ] && set -a && . "$HOME/.claude-code-env" && set +a"#;

/// Grok-specific VM seed: the credential is the user's local
/// `~/.grok/auth.json`, arriving on stdin into a 0600 file like codex. grok's
/// always-approve posture (`permission_mode = "bypassPermissions"`) and its MCP
/// servers are reconciled into `~/.grok/config.toml` at boot by express-agent,
/// and the image puts `~/.grok/bin` on PATH via `/etc/environment`, so neither
/// the old `[ui] yolo` merge nor a `/usr/local/bin` symlink is needed.
const GROK_SEED: &str = r#"mkdir -p ~/.grok
cat > ~/.grok/auth.json"#;

/// PATH for the harness binaries, needed by every command session this command
/// opens.
///
/// The image puts these dirs on PATH two ways, and a command session gets
/// NEITHER: `/root/.profile` covers login shells (the durable console), and the
/// Docker `ENV` covers the workload process and express-agent's own phases. An
/// `ssh <target> <cmd>` session is non-interactive and non-login, so it starts
/// from the default PATH and cannot see `~/.local/bin` — where claude and codex
/// actually live. Without this, `command -v claude` reports missing on an image
/// that definitely has it.
///
/// Deliberately not `. ~/.profile`: that sources `.bashrc`, whose starship/mise/
/// zoxide init writes to stdout and would corrupt the AGENT-READY marker this
/// command parses. Mirrors the image's own export line instead.
const HARNESS_PATH: &str = r#"export PATH="$HOME/.local/bin:$HOME/.opencode/bin:$HOME/.grok/bin:$HOME/.local/share/mise/shims:$PATH""#;

/// Agent-independent seeds, all idempotent:
/// - COLORTERM: the relay forwards TERM but not COLORTERM; without it TUIs
///   render a greyed/degraded palette.
/// - ~/.profile autostart: plain connects (`railway ssh agent:<env>:<id>`) run
///   bash as a login shell (verified: ~/.profile IS sourced; command sessions
///   are not), so any interactive reconnect drops into the agent recorded in
///   ~/.railway-code-agent (written per-provision, so re-running with the
///   other agent retargets reconnects too). Not `exec`, so quitting the agent
///   lands in a shell instead of closing the connection. The `[ -t 1 ]` guard
///   keeps scp-style and command sessions out. The trailing printf restores
///   terminal state a TUI can leave behind on an unclean exit (kitty keyboard
///   mode et al) — see `TERMINAL_RESET`.
/// The autostart block is versioned: the v4 marker gates the append, and the
/// sed strips any earlier version first (comment line through the closing
/// `fi` at column zero), so an agent provisioned on v2/v3 (which skipped the
/// carried token when an on-agent `/login` looked newer) picks the
/// unconditional export back up on its next provision.
///
/// v3 added the `~/.railway-app-mode` guard. `railway ca desktop` hands the
/// agent to an external app that bootstraps through the login shell, so an
/// autostart firing there would put a harness where the app expects a shell.
/// The marker file is what the two provision modes disagree about — see
/// [`provision_script`] — and it is checked here rather than simply omitted from
/// `~/.railway-code-agent`, because a later `railway code` on the same agent
/// would write that file straight back.
const COMMON_SEED: &str = r#"grep -q "^COLORTERM=" /etc/environment 2>/dev/null || echo "COLORTERM=truecolor" >> /etc/environment 2>/dev/null || true
if ! grep -q "railway-code agent autostart v4" ~/.profile 2>/dev/null; then
sed -i '/# railway-code agent autostart/,/^fi$/d' ~/.profile 2>/dev/null || true
cat >> ~/.profile <<'PROFEOF'

# railway-code agent autostart v4 (connecting drops into the agent; exit it for a shell)
if [ -z "$RAILWAY_CODE_AUTOSTARTED" ] && [ -t 1 ] && [ ! -f "$HOME/.railway-app-mode" ] && [ -s "$HOME/.railway-code-agent" ]; then
  agent="$(cat "$HOME/.railway-code-agent")"
  [ -d "$HOME/.grok/bin" ] && export PATH="$HOME/.grok/bin:$PATH"
  if command -v "$agent" >/dev/null 2>&1; then
    export RAILWAY_CODE_AUTOSTARTED=1
    [ -f "$HOME/.gh-token" ] && export GH_TOKEN="$(cat "$HOME/.gh-token")"
    [ -f "$HOME/.claude-code-env" ] && set -a && . "$HOME/.claude-code-env" && set +a
    "$agent"
    printf '\033[<u\033[<u\033[=0;1u\033[?2004l\033[?1000l\033[?1002l\033[?1003l\033[?1006l\033[?1004l\033[?25h'
  fi
fi
PROFEOF
fi"#;

/// Does the agent already hold a Claude credential?
///
/// A setup-token lasts a year and the agent's disk survives sleep, so a reused
/// agent is normally still authenticated from a previous run — re-minting would
/// spend an OAuth round-trip to overwrite a working credential. Existence is
/// all we check: validity would need an API call, and a year-long grant is
/// rarely the thing that broke. `--refresh-auth` is the escape hatch when it is.
const CLAUDE_CREDENTIAL_PROBE: &str =
    r#"[ -s ~/.claude-code-env ] && echo CRED-PRESENT || echo CRED-ABSENT"#;

/// The whole VM-side provision as ONE script over ONE connection — the
/// credential arrives on stdin (never an argv) into a 0600 file, then the
/// reconnect seeds run. One connection matters because the status marker rides
/// stdout: without it a relay-level failure is indistinguishable from a VM
/// that answered but couldn't run the script.
///
/// There is no install path and no update path. `cloud-agent-base` bakes every
/// harness and keeps them current, so a missing binary is an image problem the
/// user cannot fix by waiting — hence AGENT-MISSING rather than an install
/// attempt that would race the image's own copy.
///
/// `write_credential` is false when the agent already holds a working credential
/// and we are reusing it. The seed must then be omitted entirely rather than run
/// with empty stdin: `cat > ~/.claude-code-env` would truncate the very file we
/// decided to keep.
///
/// `app_mode` is `railway ca desktop`: the agent is being handed to an external
/// app that opens its own sessions, so the two marker files are written the
/// other way round. Both modes write both files — one as a marker, one as a
/// removal — because either can follow the other on the same agent, and a mode
/// that only ever adds its own marker would inherit the previous one's.
fn provision_script(agent: Agent, write_credential: bool, app_mode: bool) -> String {
    let seed = if write_credential {
        agent.credential_seed()
    } else {
        "true"
    };
    let name = agent.name();
    let hash_marker = skills_sync::REMOTE_HASH_MARKER;
    let hash_file = skills_sync::REMOTE_HASH_FILE;
    let mcp_marker = mcp_sync::REMOTE_HASH_MARKER;
    let mcp_file = mcp_sync::REMOTE_HASH_FILE;
    let mode_seed = mode_seed(agent, app_mode);
    format!(
        r#"umask 077
{HARNESS_PATH}
{seed}
{COMMON_SEED}
{mode_seed}
printf '{hash_marker}%s\n' "$(cat "{hash_file}" 2>/dev/null || true)"
printf '{mcp_marker}%s\n' "$(cat "{mcp_file}" 2>/dev/null || true)"
if command -v {name} >/dev/null 2>&1; then echo AGENT-READY; else echo AGENT-MISSING; fi"#
    )
}

/// [`provision_script`] plus the skills sync, one connection, for a freshly
/// created agent. A fresh VM cannot already hold the skills hash, so the
/// report-then-upload dance is a wasted relay round-trip there — the tarball
/// rides the provision stdin instead, behind the length-framed credential.
/// The AGENT-READY check prints before the sync block because that block's
/// degradation paths `exit 0`.
fn provision_script_with_skills(
    agent: Agent,
    credential_len: Option<usize>,
    app_mode: bool,
    skills_hash: &str,
) -> String {
    let seed = match credential_len {
        Some(len) => agent.credential_seed_framed(len),
        None => "true".to_string(),
    };
    let name = agent.name();
    let mode_seed = mode_seed(agent, app_mode);
    let sync = skills_sync::sync_body(skills_hash);
    format!(
        r#"umask 077
{HARNESS_PATH}
{seed}
payload="$HOME/.railway-skills-payload.tgz"
cat > "$payload"
{COMMON_SEED}
{mode_seed}
if command -v {name} >/dev/null 2>&1; then echo AGENT-READY; else echo AGENT-MISSING; fi
{sync}"#
    )
}

/// The autostart in COMMON_SEED reads both: the sentinel disables it
/// outright, and `~/.railway-code-agent` is what it would otherwise launch.
///
/// A shell launch is "give me the machine", not "retarget this VM": the
/// recorded autostart agent stays whatever a previous launch made it, so
/// plain reconnects keep dropping into that agent. App-mode still wins if
/// it was asked for — desktop takes the login shell entirely.
fn mode_seed(agent: Agent, app_mode: bool) -> String {
    if app_mode {
        "touch ~/.railway-app-mode\nrm -f ~/.railway-code-agent".to_string()
    } else {
        let record = match agent {
            Agent::Shell => "true".to_string(),
            _ => format!("echo {} > ~/.railway-code-agent", agent.name()),
        };
        format!("rm -f ~/.railway-app-mode\n{record}")
    }
}

/// Where a prepared session's output lands, which decides what quitting the
/// harness should leave behind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionStyle {
    /// ssh owns the real terminal (`railway ca start`, piped and `--`
    /// callers): quitting the agent lands in a shell on the VM, matching the
    /// `~/.profile` autostart, and `exit` ends the connection.
    FullTerminal,
    /// The TUI's session pane: when the harness exits the remote command ends,
    /// the durable session with it, and the pane closes. A shell fallback here
    /// would strand the user on a bare VM prompt inside what still looks like
    /// the TUI — and leave the durable session alive as a shell nobody wants
    /// to reattach to.
    Pane,
}

/// The command the launch session runs on the VM. Three shapes, and the
/// difference between them is whether you are left in a session afterwards:
///
/// - a seeded prompt starts the agent on that task and keeps the session;
/// - a bare launch starts the agent interactively and keeps the session;
/// - `-- args` execs the agent and exits with it, so a pipeline doesn't hang
///   waiting on a shell nobody is typing into.
///
/// "Keeps the session" is [`SessionStyle`]'s call: a full-terminal caller gets
/// a VM shell after the agent quits, a pane ends with it. Neither interactive
/// form uses `exec`, so the reset always runs. `RAILWAY_CODE_AUTOSTARTED`
/// stops the `~/.profile` autostart relaunching the agent on top of the user,
/// and the reset scrubs terminal state a TUI can leave behind on an unclean
/// exit.
fn remote_command(
    agent: Agent,
    env_prefix: &str,
    initial_prompt: Option<&str>,
    agent_args: &[String],
    style: SessionStyle,
) -> String {
    // No harness: the login shell IS the session, so there is nothing to hand
    // a prompt or args to and both are ignored (the TUI never sends either —
    // its prompt box goes inert on the shell option). The autostart guard
    // still matters: `bash -l` sources ~/.profile, which would otherwise
    // relaunch whatever agent the VM last recorded on top of the user.
    if agent == Agent::Shell {
        return format!("{env_prefix}export RAILWAY_CODE_AUTOSTARTED=1; exec bash -l");
    }
    // Railway's TUI fronts a shared daemon whose default is to join the
    // directory's live session — every window becomes another view of the
    // same conversation, and even a seeded prompt is steered into the run
    // already in flight. An explicit `--session <id>` spawns (or rejoins)
    // that exact id instead, so keying it to the durable session's own name —
    // which vm-init stamps into every durable session's environment — makes
    // each window its own conversation while keeping the daemon's
    // persistence. The `-- args` exec form below is left alone: its arguments
    // are the caller's, flags included.
    let name = match agent {
        Agent::Railway => "railway-agent-tui --session \"$RAILWAY_DURABLE_SESSION_NAME\"",
        _ => agent.name(),
    };
    let after = match style {
        SessionStyle::FullTerminal => "; exec bash -l",
        SessionStyle::Pane => "",
    };
    match initial_prompt.map(str::trim).filter(|p| !p.is_empty()) {
        Some(prompt) => format!(
            "{env_prefix}export RAILWAY_CODE_AUTOSTARTED=1; {name} {}; {}{after}",
            shell_join(std::slice::from_ref(&prompt.to_string())),
            terminal_reset_printf()
        ),
        None if agent_args.is_empty() => format!(
            "{env_prefix}export RAILWAY_CODE_AUTOSTARTED=1; {name}; {}{after}",
            terminal_reset_printf()
        ),
        None => format!(
            "{env_prefix}exec {} {}",
            agent.name(),
            shell_join(agent_args)
        ),
    }
}

/// SSH options shared by every connection this command runs, plus the info
/// needed to self-heal our relay known-hosts file.
///
/// Relay connections verify against the CLI's own known-hosts file
/// (`~/.railway/known_hosts_relay`) with accept-new, leaving the user's
/// `~/.ssh/known_hosts` untouched, and `ssh_plumbing` may heal THIS file (and
/// only this file) on a mismatch. The relay presents one stable ed25519 key
/// today (verified: 8/8 keyscans identical, and four sequential connections
/// recorded a single key), so the heal path is a safety net rather than a
/// routine occurrence — it covers the fleet going back to per-instance keys
/// without pinning us to a key that would then mismatch constantly.
///
/// These BASE options are deliberately not multiplexed, and readiness probes
/// must never be: a probe against a not-yet-routable agent still opens a real
/// connection (the relay falls through to the dev.new control surface instead
/// of refusing at the transport), so a ControlMaster owned by a failed probe
/// pins every later channel to that dead path — measured at 8/20 launch
/// timeouts when tried on 2026-08-18. Cross-RUN persistence is the other
/// historical trap: sleeping an agent kills the master's TCP while the socket
/// file lives on, so the next run rides a dead master and dies with a bare
/// exit 255.
///
/// What IS safe — and what [`launch_mux`] provides — is a master scoped to
/// one launch and opened only by the provision connection, which runs after
/// the route is verified and whose output markers prove it reached the real
/// agent. The interactive session then rides that master (one handshake saved,
/// ~0.35s), and a stale or dead socket falls back to a plain connection
/// because the session never sets ControlMaster itself. The socket path is
/// unique per launch, so nothing outlives the run that made it beyond the
/// 30s ControlPersist grace.
#[derive(Clone)]
struct RelaySsh {
    opts: Vec<String>,
    known_hosts: std::path::PathBuf,
    /// known-hosts pattern for ssh-keygen -R: `host` or `[host]:port`.
    host_pattern: String,
}

/// A fresh, never-reused control socket path: OS temp dir, pid + random, so
/// neither a recycled pid nor two sockets within one launch can collide.
/// Uniqueness is the safety property the whole mux design leans on — a socket
/// is only ever ridden by connections that know it was created by a
/// marker-verified connection to the real agent (see [`RelaySsh`]).
fn mux_socket() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "railway-cm-{}-{:08x}.sock",
        std::process::id(),
        rand::random::<u32>()
    ))
}

/// Whether connection sharing can work here at all: not on Windows, whose
/// OpenSSH has no ControlMaster support (passing the options anywhere from
/// warns to fails), and not when the socket path would overflow the unix
/// socket path limit (~104 bytes; a deep $TMPDIR gets there). Both cases fall
/// back to plain connections — the launch works identically, it just pays the
/// handshakes multiplexing would have saved.
fn mux_usable(socket: &std::path::Path) -> bool {
    !cfg!(windows) && socket.as_os_str().len() <= 100
}

/// Options for a connection allowed to CREATE the master on `socket`:
/// a readiness probe (each round gets its own fresh socket; only the round
/// whose marker round-trips is ever promoted) or the provision connection.
/// `persist` bounds how long an idle master outlives its last client — probes
/// use a short one so the losing rounds' masters evaporate.
/// Empty when multiplexing is unusable here, so every caller degrades to a
/// plain connection without carrying the platform check itself.
fn mux_master_opts(socket: &std::path::Path, persist: &str) -> Vec<String> {
    if !mux_usable(socket) {
        return Vec::new();
    }
    vec![
        "-o".into(),
        "ControlMaster=auto".into(),
        "-o".into(),
        format!("ControlPath={}", socket.display()),
        "-o".into(),
        format!("ControlPersist={persist}"),
    ]
}

/// Options for a connection that may RIDE the master on `socket` but never
/// create one: a dead or missing socket falls back to a plain connection.
/// Empty when multiplexing is unusable here, same as [`mux_master_opts`].
fn mux_client_opts(socket: &std::path::Path) -> Vec<String> {
    if !mux_usable(socket) {
        return Vec::new();
    }
    vec!["-o".into(), format!("ControlPath={}", socket.display())]
}

/// The known-hosts file the CLI keeps for the relay.
///
/// Exposed for `ca desktop`, which writes it into an OpenSSH block so a
/// third-party client trusts the relay's key the same way this CLI does — and,
/// more to the point, never lands the relay in the user's `~/.ssh/known_hosts`
/// or meets a host-key prompt it cannot answer.
pub fn relay_known_hosts() -> Result<std::path::PathBuf> {
    Ok(relay_ssh()?.known_hosts)
}

fn relay_ssh() -> Result<RelaySsh> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to get home directory"))?;
    let railway_dir = home.join(".railway");
    std::fs::create_dir_all(&railway_dir)?;
    let known_hosts = railway_dir.join("known_hosts_relay");

    let (host, port) = Configs::get_ssh_relay();
    let host_pattern = match port {
        Some(p) if p != 22 => format!("[{host}]:{p}"),
        _ => host.to_string(),
    };

    Ok(RelaySsh {
        opts: vec![
            "-o".into(),
            format!("UserKnownHostsFile={}", known_hosts.display()),
            "-o".into(),
            "StrictHostKeyChecking=accept-new".into(),
            // A session pane sits idle for as long as the user reads, and an
            // idle TCP path through a NAT or load balancer gets dropped without
            // either end being told. Without keepalives that shows up as a pane
            // frozen until the kernel gives up on the connection — tens of
            // minutes — with no error from anything. Probing every 30s keeps
            // the idle-timeout clocks at zero and turns a dead path into a
            // detected disconnect within ~90s, which the TUI can then offer to
            // reconnect. Same numbers as the port-forward path, for the same
            // reason.
            "-o".into(),
            "ServerAliveInterval=30".into(),
            "-o".into(),
            "ServerAliveCountMax=3".into(),
        ],
        known_hosts,
        host_pattern,
    })
}

impl RelaySsh {
    /// Drop the relay's entry from OUR known-hosts file so the next attempt
    /// re-accepts whichever fleet key answers. Never touches ~/.ssh.
    fn heal_known_hosts(&self) {
        let _ = std::process::Command::new("ssh-keygen")
            .arg("-R")
            .arg(&self.host_pattern)
            .arg("-f")
            .arg(&self.known_hosts)
            .output();
    }
}

/// Terminal-state reset emitted after the agent TUI exits. Codex enables the
/// kitty keyboard protocol (plus bracketed paste / mouse / focus reporting);
/// when it dies without restoring — ctrl-c mid-TUI, a crash, a dropped
/// connection — the terminal is left in enhanced-key mode and subsequent
/// keys render as junk like `9;5:3u`. Two pops unwind a nested push,
/// `CSI =0;1u` hard-zeroes the flags for terminals that ignore an unbalanced
/// pop, and the rest turn off bracketed paste, mouse and focus reporting and
/// re-show the cursor. Every sequence is a no-op on an already-clean
/// terminal.
const TERMINAL_RESET: &str = "\x1b[<u\x1b[<u\x1b[=0;1u\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?25h";

/// `TERMINAL_RESET` as a `printf`-ready octal-escaped literal for remote
/// shell snippets (raw ESC bytes don't survive quoting/readability in a
/// command string; `printf` re-expands `\033`).
fn terminal_reset_printf() -> String {
    format!("printf '{}'", TERMINAL_RESET.replace('\x1b', "\\033"))
}

/// Read a harness's local sign-in file — codex's `~/.codex/auth.json`, grok's
/// `~/.grok/auth.json` — so the agent starts already signed in.
///
/// A missing or empty file is not a failure. It means this machine never had
/// that harness signed in, so there is nothing to carry and the harness on the
/// agent asks for a sign-in itself; the launch is worth more than the
/// convenience. A file that exists but cannot be read *is* an error: the user
/// has a sign-in, and silently launching without it would look like the copy
/// worked.
fn local_signin(agent: Agent, home: &Path) -> Result<PendingAuth> {
    let Some(parts) = agent.local_signin_file() else {
        return Err(anyhow!(
            "{} has no local sign-in file to copy",
            agent.display()
        ));
    };
    let auth_path = home.join(parts[0]).join(parts[1]);
    let missing = || PendingAuth::SignInOnAgent {
        note: format!(
            "No {} sign-in on this machine ({}) — starting {} unauthenticated; {}.",
            agent.display(),
            auth_path.display(),
            agent.display(),
            agent.sign_in_on_agent_hint()
        ),
    };
    if !auth_path.exists() {
        return Ok(missing());
    }
    let bytes = std::fs::read(&auth_path)
        .with_context(|| format!("Couldn't read {}", auth_path.display()))?;
    if bytes.is_empty() {
        return Ok(missing());
    }
    Ok(PendingAuth::Ready {
        line: bytes,
        source: auth_path.display().to_string(),
    })
}

/// Where a minted `claude setup-token` grant is cached between runs.
///
/// A setup-token is valid for a year, so re-minting per run means an OAuth
/// round-trip for a credential the user already has. Caching it is the shape
/// Anthropic itself recommends — their `claude-code-action` has Pro/Max users
/// mint locally, store the result as a repository secret, and use it on remote
/// runners. Cleared by `railway logout`, or by `--refresh-auth`.
fn claude_token_cache_path() -> Option<std::path::PathBuf> {
    Some(claude_token_cache_path_in(&dirs::home_dir()?))
}

/// The cache path under an explicit home, so callers that already scope
/// themselves to one (`railway ca setup --reset`, and the tests that drive it
/// against a tempdir) clear the right file. Resolving `dirs::home_dir()` here
/// regardless of the caller's home meant the suite deleted the developer's own
/// cached setup-token every run, sending their next `--claude` launch through a
/// browser mint.
fn claude_token_cache_path_in(home: &Path) -> std::path::PathBuf {
    home.join(".railway").join("claude-code-token")
}

/// Read the cached token, if one is there and still plausible.
fn cached_claude_token() -> Option<String> {
    let tok = std::fs::read_to_string(claude_token_cache_path()?).ok()?;
    let tok = tok.trim().to_string();
    (!tok.is_empty() && validate_claude_token(&tok).is_ok()).then_some(tok)
}

/// Cache a freshly minted token 0600. Best-effort: a cache we cannot write is
/// a slower next run, not a failed this one.
fn cache_claude_token(token: &str) {
    if let Some(path) = claude_token_cache_path() {
        write_token_0600(&path, token);
    }
}

/// Write a token to `path`, creating it 0600.
///
/// Created 0600 rather than written-then-chmodded: the write-first order leaves
/// a year-long credential briefly readable at whatever the umask allows. Split
/// out from `cache_claude_token` so that property is testable without a $HOME.
fn write_token_0600(path: &std::path::Path, token: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(path) {
        use std::io::Write;
        let _ = f.write_all(format!("{token}\n").as_bytes());
    }
}

/// How long ago the cached setup-token was minted, from the cache file's
/// mtime — the file is written once per mint, so its age is the token's.
fn claude_token_age_days() -> Option<u64> {
    let meta = std::fs::metadata(claude_token_cache_path()?).ok()?;
    Some(meta.modified().ok()?.elapsed().ok()?.as_secs() / 86_400)
}

/// Name the cached credential with its age. A setup-token is bound for its
/// whole year to the account picked at mint time, and a months-old token
/// quietly explaining a missing model list is exactly the thing worth a
/// visible age and an exit. The re-mint hint waits for 30 days: younger
/// tokens are rarely the problem, and the hint would just be noise.
fn cached_token_source(age_days: Option<u64>) -> String {
    match age_days {
        None | Some(0) => "cached setup-token".to_string(),
        Some(days @ 1..30) => format!("cached setup-token from {days}d ago"),
        Some(days) => {
            format!("cached setup-token from {days}d ago — --refresh-auth re-mints")
        }
    }
}

/// Forget the cached token. Called by `railway logout`.
pub fn clear_claude_token_cache() {
    if let Some(home) = dirs::home_dir() {
        clear_claude_token_cache_in(&home);
    }
}

/// [`clear_claude_token_cache`] scoped to an explicit home.
pub fn clear_claude_token_cache_in(home: &Path) {
    let _ = std::fs::remove_file(claude_token_cache_path_in(home));
}

/// The credential to push, or the knowledge that obtaining one costs a browser
/// flow.
///
/// The distinction exists so the expensive path can be deferred: a reused agent
/// already holds a working credential on its own disk, and minting a second
/// year-long grant to overwrite it is pure waste. Everything cheap resolves up
/// front so a bad credential still fails before a VM is spent.
enum PendingAuth {
    /// A local file (codex, grok), the environment, or our cache — free.
    Ready { line: Vec<u8>, source: String },
    /// Only obtainable by running Claude's OAuth flow. Deferred until we know
    /// the agent doesn't already have one.
    MintClaude,
    /// Nothing to carry, and nothing local to get it from. The harness signs
    /// in on the agent instead, the way it does on any machine it has not seen
    /// before. `note` is the line the user gets so the extra sign-in isn't a
    /// surprise.
    SignInOnAgent { note: String },
    /// Railway's own harness: nothing to push, ever. Its credential is a
    /// server-minted VM env var, not a client-side file.
    None,
}

/// Set once a Claude mint has been offered this run and come away empty — no
/// local `claude`, no terminal, nothing pasted.
///
/// The launch pipeline resolves the credential twice on the TUI path (once
/// out-of-frame in [`ensure_claude_credential_cached`], once inside
/// [`prepare_inner`]), and without this the second pass would re-run a flow
/// that just failed — underneath a ratatui frame, where its browser wait and
/// paste prompt cannot render. Asked once, answered once.
static CLAUDE_MINT_DECLINED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn claude_sign_in_note() -> String {
    format!(
        "No {} credential to carry from this machine — starting it unauthenticated; {}.",
        Agent::Claude.display(),
        Agent::Claude.sign_in_on_agent_hint()
    )
}

fn claude_sign_in_on_agent() -> PendingAuth {
    PendingAuth::SignInOnAgent {
        note: claude_sign_in_note(),
    }
}

/// Resolve a Claude credential from the sources that cost nothing: this
/// command's own cache, then the environment. Anything else needs a mint —
/// unless there is nothing here to mint with, in which case the agent's own
/// sign-in is the flow, and the user finds that out before a VM is spent
/// rather than through a browser prompt that never arrives.
///
/// `refresh_auth` is the local half of `--refresh-auth`: it drops our cached
/// setup-token so a stale or revoked one can't be handed to the agent again,
/// and forces the mint below to run instead of silently reusing it. An
/// explicit `CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_API_KEY` still wins even
/// then — that is the user naming a credential for this run, not a cache.
fn claude_credentials_cheap(refresh_auth: bool) -> Result<PendingAuth> {
    for var in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"] {
        if let Ok(tok) = std::env::var(var) {
            let tok = tok.trim().to_string();
            if !tok.is_empty() {
                validate_claude_token(&tok)?;
                return Ok(PendingAuth::Ready {
                    line: format!("{var}={tok}\n").into_bytes(),
                    source: format!("${var}"),
                });
            }
        }
    }
    if refresh_auth {
        clear_claude_token_cache();
    } else if let Some(tok) = cached_claude_token() {
        return Ok(PendingAuth::Ready {
            line: format!("CLAUDE_CODE_OAUTH_TOKEN={tok}\n").into_bytes(),
            source: cached_token_source(claude_token_age_days()),
        });
    }
    if CLAUDE_MINT_DECLINED.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(claude_sign_in_on_agent());
    }
    // The mint runs the user's own `claude setup-token`. Without that binary
    // there is no flow to offer, and the manual paste prompt it falls back to
    // asks for the output of a command this machine cannot run.
    if which::which("claude").is_err() {
        return Ok(claude_sign_in_on_agent());
    }
    Ok(PendingAuth::MintClaude)
}

/// Run the OAuth flow to mint a fresh setup-token, and cache it.
///
/// Mirrors mono's agent-vm Connect tab flow: a deliberate long-lived grant, NOT
/// the local sign-in's `.credentials.json`, whose rotating refresh token two
/// machines cannot safely share.
/// `Ok(None)` when there is no credential to be had here — the caller launches
/// without one and the user signs in on the agent. Reserved for "this machine
/// can't produce one": a bad token that someone actually supplied is still an
/// error, because launching past it would look like it was accepted.
fn mint_claude_credentials() -> Result<Option<(Vec<u8>, String)>> {
    use colored::Colorize;

    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        // Nothing to prompt: the OAuth flow needs a terminal. Say what would
        // have skipped the sign-in, then get out of the way.
        eprintln!(
            "{}",
            "No Claude credential found — set CLAUDE_CODE_OAUTH_TOKEN (from `claude setup-token`) or ANTHROPIC_API_KEY to carry one from this machine."
                .yellow()
        );
        CLAUDE_MINT_DECLINED.store(true, std::sync::atomic::Ordering::Relaxed);
        return Ok(None);
    }

    // Automatic path: mint a fresh token with the user's own claude install,
    // fully hidden — the flow's TUI (and the token it prints) never touch the
    // screen. The browser side stays visible and interactive: with an
    // existing session + prior consent it completes hands-free; an approve
    // click also works. Only the degenerate paste-the-code-into-the-terminal
    // path can't complete hidden — that times out and falls back to the
    // manual paste prompt below. Skipped when the TUI already ran (and lost)
    // this exact flow under its frame: re-running it here would spend another
    // two-minute timeout to arrive at the same paste prompt.
    if !CLAUDE_AUTO_MINT_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
        let spinner = create_shimmer_spinner(
            "Minting a Claude token — approve the browser prompt if one appears",
        );
        match mint_claude_credential_headless() {
            Ok(tok) => {
                spinner.finish_and_clear();
                return Ok(Some((
                    format!("CLAUDE_CODE_OAUTH_TOKEN={tok}\n").into_bytes(),
                    "claude setup-token".to_string(),
                )));
            }
            Err(e) => {
                spinner.finish_and_clear();
                eprintln!(
                    "{}",
                    format!(
                        "Couldn't mint a token automatically ({e}) — run `claude setup-token` in another terminal instead."
                    )
                    .yellow()
                )
            }
        }
    }

    let tok = crate::util::prompt::prompt_secret(
        "Run `claude setup-token` on this machine, then paste the token (enter to skip and sign in on the agent)",
    )?;
    let tok = tok.trim().to_string();
    // Skipped: an empty answer to an optional convenience is an answer, not a
    // failure. The agent still launches; claude asks for the sign-in there.
    if tok.is_empty() {
        CLAUDE_MINT_DECLINED.store(true, std::sync::atomic::Ordering::Relaxed);
        return Ok(None);
    }
    validate_claude_token(&tok)?;
    cache_claude_token(&tok);
    Ok(Some((
        format!("CLAUDE_CODE_OAUTH_TOKEN={tok}\n").into_bytes(),
        "claude setup-token".to_string(),
    )))
}

/// How long the hidden setup-token flow may wait for the browser round-trip
/// before we kill it and fall back to the manual paste prompt. Generous: the
/// user may need to click Approve (or even sign in) in the browser first.
const SETUP_TOKEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Set once the hidden mint has run and lost this process, so no later path
/// spends another two-minute timeout re-running automation on its way to the
/// same manual paste prompt.
static CLAUDE_AUTO_MINT_FAILED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The hidden half of the mint, for a TUI that stays on screen: the browser
/// does all the interacting, the terminal is never touched, and success lands
/// in the cache the launch pipeline already reads. `Err` means this flow needs
/// the real terminal (the manual paste fallback) — the caller steps out the
/// way it always has, and the step-out skips straight to the paste prompt.
///
/// Blocking (a child process wait) — a TUI calls it via `spawn_blocking`.
pub fn mint_claude_credential_headless() -> Result<String> {
    let minted = run_claude_setup_token().and_then(|tok| {
        validate_claude_token(&tok)?;
        Ok(tok)
    });
    match minted {
        Ok(tok) => {
            cache_claude_token(&tok);
            Ok(tok)
        }
        Err(e) => {
            CLAUDE_AUTO_MINT_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
            Err(e)
        }
    }
}

/// Run `claude setup-token` invisibly under `script(1)`'s PTY: claude's TUI
/// needs a tty, `script` provides one and records every byte to a file, and
/// all of the process's stdio is detached so nothing (least of all the
/// minted token) renders on the user's screen. The token is harvested from
/// the recording afterward. The browser half of the OAuth flow is untouched
/// — claude still opens it, and an existing session with prior consent
/// completes the round-trip with no terminal input at all (verified). The
/// paste-a-code-into-the-terminal fallback can't work hidden, so a flow
/// stuck waiting on it hits the timeout and the caller drops to the manual
/// paste prompt.
///
/// The inner `stty cols 500` matters: claude's TUI soft-wraps output at the
/// pty width, and a wrapped token would be recorded in fragments. 500 cols
/// keeps the ~110-char token on one line (the extractor still reassembles
/// wraps as a backstop).
fn run_claude_setup_token() -> Result<String> {
    let capture = std::env::temp_dir().join(format!(
        "railway-setup-token-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    // Pre-create 0600: the recording contains the token, and script(1)
    // truncates rather than replaces, so the perms hold.
    {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&capture)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }

    #[cfg(not(unix))]
    {
        let _ = std::fs::remove_file(&capture);
        bail!("automatic token capture needs a unix pty");
    }

    #[cfg(unix)]
    {
        use std::process::Stdio;

        let inner = "stty cols 500 rows 50 2>/dev/null; exec claude setup-token";
        #[cfg(target_os = "macos")]
        let mut cmd = {
            let mut c = std::process::Command::new("script");
            c.arg("-q").arg(&capture).args(["/bin/sh", "-c", inner]);
            c
        };
        #[cfg(not(target_os = "macos"))]
        let mut cmd = {
            let mut c = std::process::Command::new("script");
            c.args(["-q", "-e", "-c", inner]).arg(&capture);
            c
        };
        // Fully detached: the recording is the only place the session (and
        // the token) lands. Null stdin is fine — the browser round-trip
        // needs no terminal input, and the hands-free completion was
        // verified against exactly this setup.
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("couldn't spawn script(1): {e}"))?;
        let deadline = std::time::Instant::now() + SETUP_TOKEN_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(200)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&capture);
                    return Err(anyhow!("couldn't wait on script(1): {e}"));
                }
            }
        };

        let recorded = std::fs::read_to_string(&capture);
        let _ = std::fs::remove_file(&capture);
        match extract_claude_token(&recorded.unwrap_or_default()) {
            Some(tok) => Ok(tok),
            None => match status {
                None => Err(anyhow!(
                    "the browser sign-in didn't complete within {}s",
                    SETUP_TOKEN_TIMEOUT.as_secs()
                )),
                Some(s) if !s.success() => Err(anyhow!(
                    "`claude setup-token` exited without minting a token — is claude installed locally?"
                )),
                Some(_) => Err(anyhow!("`claude setup-token` finished without a token")),
            },
        }
    }
}

/// Pull the `sk-ant-oat01-…` token out of a raw terminal recording: strip
/// the ANSI escapes, then reassemble the token across claude's soft line
/// wraps. A continuation line is all token-charset after at most one leading
/// space — prose like "Store this token securely" never qualifies, so the
/// join can't run past the token.
fn extract_claude_token(raw: &str) -> Option<String> {
    let text = strip_ansi(raw);
    let is_tok = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let lines: Vec<&str> = text.split(['\r', '\n']).collect();
    let mut best: Option<String> = None;
    for (i, line) in lines.iter().enumerate() {
        let Some(pos) = line.find("sk-ant-oat01-") else {
            continue;
        };
        let mut tok: String = line[pos..].chars().take_while(|&c| is_tok(c)).collect();
        // Only chase continuations when the token ran to the end of its line
        // (i.e. it may have been soft-wrapped there).
        let mut at_eol = pos + tok.len() >= line.trim_end().len();
        let mut j = i + 1;
        while at_eol {
            while j < lines.len() && lines[j].is_empty() {
                j += 1;
            }
            let Some(cont) = lines.get(j) else { break };
            let cont = cont.strip_prefix(' ').unwrap_or(cont);
            let cont = cont.trim_end();
            if !cont.is_empty() && cont.chars().all(is_tok) {
                tok.push_str(cont);
                j += 1;
                at_eol = true;
            } else {
                break;
            }
        }
        if best.as_ref().is_none_or(|b| tok.len() > b.len()) {
            best = Some(tok);
        }
    }
    // Real tokens are ~110 chars; anything shorter is a fragment or noise.
    best.filter(|t| t.len() >= 60)
}

/// Drop ANSI/VT escape sequences (CSI, OSC hyperlinks, charset selects) and
/// shift bytes from a terminal recording, keeping plain text and newlines.
fn strip_ansi(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: parameter/intermediate bytes, then one final byte @..~
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC / DCS / SOS / PM / APC: run to BEL or ST (ESC \)
                Some(']') | Some('P') | Some('X') | Some('^') | Some('_') => {
                    while let Some(n) = chars.next() {
                        if n == '\x07' {
                            break;
                        }
                        if n == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Charset selects take one designator byte.
                Some('(') | Some(')') => {
                    chars.next();
                }
                // Two-byte escapes (ESC 7, ESC 8, ESC =, …): already consumed.
                _ => {}
            },
            // SO/SI charset shifts.
            '\x0e' | '\x0f' => {}
            _ => out.push(c),
        }
    }
    out
}

/// The env line is sourced by a POSIX shell in the sandbox, so refuse
/// anything that could escape a bare KEY=VALUE assignment. Real tokens are
/// `sk-ant-…` charset; this is a tripwire for pasting the wrong thing, not a
/// format check.
fn validate_claude_token(tok: &str) -> Result<()> {
    if tok
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || "'\"\\$`;#".contains(c))
    {
        bail!(
            "That doesn't look like a Claude token (it contains whitespace or shell-special characters)."
        );
    }
    Ok(())
}

/// Plumbing ssh with retries. A fresh sandbox can take a beat before it
/// accepts connections, and the relay can blip — those retry with a short
/// backoff. A host-key mismatch heals the CLI's OWN relay known-hosts file
/// (the fleet presents per-instance keys, so a mismatch there is expected,
/// not a signal — see `relay_ssh`) and retries; the user's ~/.ssh files are
/// never modified. The remote scripts are idempotent, so re-running a
/// partially-applied attempt is safe.
fn ssh_plumbing(
    target: &str,
    command: &str,
    identity: Option<&std::path::Path>,
    stdin_payload: Option<&[u8]>,
    relay: &RelaySsh,
    mux_socket: Option<&std::path::Path>,
) -> Result<Vec<u8>> {
    // A woken agent re-boots its entrypoint, and the relay refuses the session
    // until the machine's new incarnation is attachable, so the first attempts
    // after a wake legitimately fail. ~20s total: enough to cross that gap,
    // short enough that a genuine failure reports promptly. How long a wake
    // actually needs is unmeasured — revisit with real numbers rather than
    // stretching this on a hunch.
    const BACKOFF_SECS: [u64; 5] = [2, 3, 5, 5, 5];
    let attempts = BACKOFF_SECS.len() + 1;

    let mut last: (i32, String) = (1, String::new());
    for attempt in 1..=attempts {
        let (code, out, err) =
            run_native_ssh_captured(target, command, identity, stdin_payload, &relay.opts)?;
        if code == 0 {
            return Ok(out);
        }

        // The relay writes its human-readable refusal ("Agent X is not running
        // …") onto the session channel, which lands on stdout — NOT stderr. Read
        // both, or a denied session surfaces as a bare exit 255 with no reason.
        let stderr_text = String::from_utf8_lossy(&err).trim().to_string();
        let stdout_text = String::from_utf8_lossy(&out).trim().to_string();
        let reason = if stderr_text.is_empty() {
            stdout_text
        } else if stdout_text.is_empty() {
            stderr_text.clone()
        } else {
            format!("{stderr_text}\n{stdout_text}")
        };

        let hostkey_mismatch = stderr_text.contains("Host key verification failed")
            || stderr_text.contains("REMOTE HOST IDENTIFICATION HAS CHANGED");
        if hostkey_mismatch {
            relay.heal_known_hosts();
        }
        // A failed attempt must not leave a master for the next attempt to
        // ride: removing the socket makes ControlMaster=auto open a genuinely
        // fresh connection instead of pinning every retry to whatever dead or
        // misrouted path the failure created — the historical 8/20-timeouts
        // trap, scoped here to within a single call.
        if let Some(socket) = mux_socket {
            let _ = std::fs::remove_file(socket);
        }
        last = (code, reason);

        if attempt < attempts {
            // A host-key roll is fixed by healing, not by waiting.
            let wait = if hostkey_mismatch {
                0
            } else {
                BACKOFF_SECS[attempt - 1]
            };
            if wait > 0 {
                std::thread::sleep(std::time::Duration::from_secs(wait));
            }
        }
    }

    let (code, reason) = last;
    if reason.is_empty() {
        bail!(
            "SSH to the agent failed after {attempts} attempts (exit {code}), with no message from the relay."
        )
    }
    bail!("SSH to the agent failed after {attempts} attempts (exit {code}):\n{reason}")
}

/// One cloud agent, reduced to what this command steers on. `pub(crate)`
/// because [`wait_until_connectable`] returns it to the `railway ca` verbs.
#[derive(Clone)]
pub(crate) struct CodeAgent {
    id: String,
    name: String,
    status: queries::cloud_agent::CloudAgentStatus,
}

/// How long to wait for a created or woken agent to reach RUNNING before
/// giving up. A cold create boots a microVM and publishes routes; a wake
/// restores a checkpoint and is much quicker.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Read one agent by id, scoped to the environment. `None` means it is gone
/// (deleted, or it belongs to another environment) — the caller's cue to forget
/// its stored pointer rather than to fail.
async fn fetch_agent(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
    id: &str,
) -> Result<Option<CodeAgent>> {
    let res = post_graphql::<queries::CloudAgent, _>(
        client,
        backboard,
        queries::cloud_agent::Variables {
            id: id.to_owned(),
            environment_id: environment_id.to_owned(),
        },
    )
    .await?;
    Ok(res.cloud_agent.map(|a| CodeAgent {
        id: a.id,
        name: a.name,
        status: a.status,
    }))
}

/// Wait until the agent is *connectable*, by probing the SSH route itself
/// rather than polling status up to RUNNING. The platform routes a shell as
/// soon as the agent's container exists — several seconds before the status
/// flips, which additionally waits out route publication, the status
/// projection, and a poll interval. Status is still read each round, but only
/// to catch terminal states (CRASHED/FAILED/DELETING) early instead of
/// burning the whole timeout on a box that will never come up.
/// The relay-side plumbing a probe or session needs, independent of which
/// agent it points at: the local key to offer and the relay's SSH options.
/// Built once per launch (the identity half costs an API round-trip) and
/// threaded through, where each waiter previously rebuilt it via
/// `connect_info` — a redundant registered-keys query at the top of every
/// wake and create wait.
pub(crate) struct RelayAccess {
    pub identity: Option<std::path::PathBuf>,
    pub relay_opts: Vec<String>,
}

/// Build [`RelayAccess`] the way `connect_info` does, for callers outside the
/// launch flow (the `ca` verbs) that don't already hold one.
pub(crate) async fn relay_access() -> Result<RelayAccess> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let identity = ensure_ssh_key_quiet(&client, &configs).await?;
    let relay = relay_ssh()?;
    Ok(RelayAccess {
        identity,
        relay_opts: relay.opts,
    })
}

/// On success, also returns the control socket of the winning probe's master
/// when there was one: that probe verified the marker round-trip, so its
/// connection provably reached the real agent, and the provision + session can
/// multiplex over it instead of paying a fresh relay handshake. `None` when
/// the status flip won the race (no verified connection exists yet).
pub(crate) async fn wait_until_connectable(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
    id: &str,
    access: &RelayAccess,
    initial_delay: std::time::Duration,
) -> Result<(CodeAgent, Option<std::path::PathBuf>)> {
    use queries::cloud_agent::CloudAgentStatus as S;
    // Measured as one stage because this is the leg the platform owns — VM
    // boot/restore up to a routable SSH target. Per-round detail goes to stderr
    // under RAILWAY_STAGE_TIMING; the recorded stage is what telemetry sees.
    let wait_started = std::time::Instant::now();
    let diagnostics = ssh_tel::timing_diagnostics();
    let ssh_target = format!("agent:{environment_id}:{id}");
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    // Callers that just issued the create or wake pass the physical floor of
    // that operation (measured: a fresh VM is never routable before ~550ms, a
    // restore before ~1.5s): probing earlier is a guaranteed miss that still
    // opens a real relay connection into the dev.new fall-through. Callers
    // that caught a boot already in flight pass zero — the box may be
    // routable right now, and any delay would be a pure regression.
    if !initial_delay.is_zero() {
        tokio::time::sleep(initial_delay).await;
    }
    let mut round = 0u32;
    // The status fetch keeps its OLD ~750ms grid even while probes run at the
    // tight cadence: it exists only to catch rare terminal states, and letting
    // it ride the probe cadence would triple backboard polling per launching
    // client for nothing. Round 1 always fetches.
    let mut last_fetch: Option<std::time::Instant> = None;
    let mut last_agent: Option<CodeAgent> = None;
    loop {
        round += 1;
        let round_started = std::time::Instant::now();

        // Every round probes as a would-be master on its own FRESH socket. A
        // round that lands on the relay's dev.new fall-through (agent not yet
        // routable) leaves a master nothing will ever reference — the socket
        // name is never reused, and only the round whose marker comes back is
        // promoted. Losers are told to exit below, with a short persist as the
        // backstop, so they don't pile up relay connections during a slow boot.
        let socket = mux_socket();
        let target = ssh_target.clone();
        let identity = access.identity.clone();
        let mut opts = access.relay_opts.clone();
        opts.extend(mux_master_opts(&socket, "3s"));
        let probe = tokio::task::spawn_blocking(move || {
            probe_native_ssh(&target, identity.as_deref(), &opts)
        });

        // Await the fetch BEFORE the probe (both are already in flight): a
        // terminal state must bail immediately, not after a probe that can sit
        // on its full ConnectTimeout against a box that will never answer.
        let fetch_due = last_fetch.is_none_or(|at| at.elapsed().as_millis() >= 700);
        if fetch_due {
            let agent = fetch_agent(client, backboard, environment_id, id)
                .await?
                .ok_or_else(|| anyhow!("Agent {id} disappeared while starting."))?;
            last_fetch = Some(std::time::Instant::now());
            match agent.status {
                S::RUNNING | S::STARTING | S::SLEEPING => {}
                S::CRASHED => bail!(
                    "Agent {} crashed while starting. `railway code --new` for a fresh one.",
                    agent.name
                ),
                S::FAILED => bail!(
                    "Agent {} failed to start. `railway code --new` for a fresh one.",
                    agent.name
                ),
                S::DELETING => bail!("Agent {} is being deleted.", agent.name),
                S::Other(ref s) => bail!("Agent {} is in an unknown state ({s}).", agent.name),
            }
            // RUNNING routes by definition, so don't spend another round on a
            // probe that lost the race to the status flip — but there is no
            // verified connection to promote (the in-flight probe is abandoned;
            // its master, if any, exits on the persist backstop).
            if agent.status == S::RUNNING {
                ssh_tel::record_stage("wait_connectable", wait_started.elapsed(), true);
                return Ok((agent, None));
            }
            last_agent = Some(agent);
        }

        let routed = probe.await?.unwrap_or(false);
        if diagnostics {
            eprintln!(
                "[wait_connectable] round {round}: status={:?} round={}ms routed={routed} fetched={fetch_due}",
                last_agent.as_ref().map(|a| &a.status),
                round_started.elapsed().as_millis()
            );
        }
        if routed {
            ssh_tel::record_stage("wait_connectable", wait_started.elapsed(), true);
            let agent = last_agent
                .ok_or_else(|| anyhow!("Agent {id} was never observed while starting."))?;
            return Ok((agent, Some(socket)));
        }
        // This round's would-be master lost; release it now rather than
        // letting it hold a relay connection for the persist window.
        release_probe_master(&socket, &ssh_target);

        if std::time::Instant::now() >= deadline {
            bail!(
                "Agent {id} did not become connectable within {}s (last state: {:?}).",
                READY_TIMEOUT.as_secs(),
                last_agent.map(|a| a.status)
            );
        }
        // Pace rounds to a cadence rather than sleeping on top of the probe: a
        // probe that already took that long IS the pacing. The cadence is
        // tight while the box is likely to come up (a fresh VM boots in
        // ~750ms, and a 750ms grid quantizes its discovery a full round late)
        // and relaxes once the wait has clearly become a boot-tail wait.
        let cadence = if wait_started.elapsed() < std::time::Duration::from_secs(4) {
            std::time::Duration::from_millis(250)
        } else {
            std::time::Duration::from_millis(750)
        };
        if let Some(rest) = cadence.checked_sub(round_started.elapsed()) {
            tokio::time::sleep(rest).await;
        }
    }
}

/// Tell a losing probe round's background master to exit now instead of
/// holding an authenticated relay connection until its persist backstop.
/// Fire-and-forget: the master may not exist (connection never completed) or
/// may already be gone — both are fine, and nothing waits on the result.
fn release_probe_master(socket: &std::path::Path, target: &str) {
    if !mux_usable(socket) {
        return;
    }
    let _ = std::process::Command::new("ssh")
        .arg("-O")
        .arg("exit")
        .arg("-o")
        .arg(format!("ControlPath={}", socket.display()))
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Bring an agent that already exists up to RUNNING: reuse it when it is
/// already up, wake it when it is asleep or still booting. `None` means the
/// agent is dead and the caller should create a fresh one.
async fn ready_existing_agent(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
    agent: CodeAgent,
    progress: &dyn Progress,
    access: &RelayAccess,
) -> Result<Option<(CodeAgent, Option<std::path::PathBuf>)>> {
    use queries::cloud_agent::CloudAgentStatus as S;

    match agent.status {
        S::RUNNING => {
            progress.note(&format!(
                "Using agent {} (--new for a fresh one)",
                agent.name
            ));
            Ok(Some((agent, None)))
        }
        // STARTING means a previous run is still booting it, so a re-run seconds
        // after a ctrl-c waits rather than minting a duplicate. SLEEPING is the
        // resting state this command leaves behind.
        S::SLEEPING | S::STARTING => {
            progress.step(&format!("Waking agent {}", agent.name));
            // The delay below is the wake's physical floor; a STARTING agent
            // caught mid-boot gets none — it may be routable right now.
            let mut probe_delay = std::time::Duration::ZERO;
            if agent.status == S::SLEEPING {
                let wake_started = std::time::Instant::now();
                let wake = post_graphql::<mutations::CloudAgentWake, _>(
                    client,
                    backboard,
                    mutations::cloud_agent_wake::Variables {
                        id: agent.id.clone(),
                    },
                )
                .await;
                ssh_tel::record_stage("wake_mutation", wake_started.elapsed(), wake.is_ok());
                if let Err(e) = wake {
                    return Err(e.into());
                }
                probe_delay = std::time::Duration::from_millis(350);
            }
            let (running, probe_master) = wait_until_connectable(
                client,
                backboard,
                environment_id,
                &agent.id,
                access,
                probe_delay,
            )
            .await?;
            progress.note(&format!(
                "Woke agent {} — your work is on its disk",
                running.name
            ));
            Ok(Some((running, probe_master)))
        }
        S::CRASHED | S::FAILED | S::DELETING | S::Other(_) => {
            progress.note(&format!(
                "Agent {} is {:?}; creating a fresh one.",
                agent.name, agent.status
            ));
            Ok(None)
        }
    }
}

/// The caller's own live agent in this environment, when there is exactly one.
///
/// `mine` is load-bearing rather than tidiness: agents authorize per
/// environment, so an unfiltered list includes teammates' — and adopting one
/// would put this user's credentials on a box someone else is working in.
async fn sole_owned_agent_id(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
) -> Result<Option<String>> {
    use queries::cloud_agents::CloudAgentStatus as S;

    let live: Vec<_> = post_graphql::<queries::CloudAgents, _>(
        client,
        backboard,
        queries::cloud_agents::Variables {
            environment_id: environment_id.to_owned(),
            mine: Some(true),
        },
    )
    .await?
    .cloud_agents
    .into_iter()
    .filter(|a| matches!(a.status, S::RUNNING | S::SLEEPING | S::STARTING))
    .collect();

    match live.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.id.clone())),
        many => bail!(
            "You have {} cloud agents in this environment and no local record of which one `railway code` should use:\n{}\nPick one with `railway ca`, or `railway code --new` to add another.",
            many.len(),
            many.iter()
                .map(|a| format!("  {} ({})", a.name, a.id))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

/// Resolve the agent this run should use: the environment's remembered one, the
/// caller's sole existing one, or a fresh one. Returns the running agent and
/// whether it was just created, which is only used for messaging.
///
/// Adoption exists because agents never self-reap: a second machine or a wiped
/// CLI config that created a duplicate would quietly bill for two boxes
/// forever, which is worth one extra lookup to avoid.
/// Where a launch lands, in the order `railway ca` uses.
///
/// A linked directory beats the configured default: `railway link` (or a
/// linked service checkout) is an explicit, per-directory statement of "this
/// is the project I'm working in", and that outranks a person-wide preference
/// that was chosen once, possibly a long time ago, from wherever the terminal
/// happened to be. The configured default is still the answer when there is no
/// link — most `railway code` invocations are not inside a linked directory —
/// or when the link points at a project or environment that no longer exists
/// (see [`stale_link_reason`]).
/// Flags still win over both: they are the caller saying it outright.
///
/// With nothing to go on, this runs `railway ca setup` rather than the
/// workspace → project → environment picker. The picker answers one launch; the
/// setup flow answers every launch after it, and asks the same question.
async fn resolve_target(
    configs: &mut Configs,
    client: &reqwest::Client,
    args: &LaunchArgs,
    prefs: &mut AgentPrefs,
    home: &Path,
) -> Result<(String, String)> {
    // The link is only consulted when nothing better is available, and reading
    // it can fail for reasons that are not this run's problem (no link at all,
    // the RAILWAY_ENVIRONMENT_ID-without-PROJECT_ID guard).
    let linked = if args.project.is_none() && args.environment.is_none() {
        configs
            .get_linked_project()
            .await
            .ok()
            .and_then(|l| l.environment.clone().map(|env| (l.project, env)))
    } else {
        None
    };

    // A link is stored intent, and stored intent goes stale: the project it
    // names can be deleted long after `railway link` wrote it, and links are
    // never cleaned up. Until now the first thing to notice was a bare
    // "Project not found. Run `railway link`" from deep inside the launch —
    // baffling in a directory nobody remembers linking. Probe the link before
    // it wins, and demote a dead one so the launch lands where a linkless run
    // would have: the configured default. Only a definite server-side "gone"
    // demotes — a transient failure keeps the link and lets the launch's own
    // calls decide, so a network blip cannot reroute a launch to another
    // project.
    let linked = match linked {
        Some((project_id, environment_id)) => {
            let lookup = get_project(client, configs, project_id.clone()).await;
            match stale_link_reason(&lookup, &environment_id) {
                Some(reason) => {
                    eprintln!(
                        "{}",
                        format!(
                            "This directory is linked to a {reason} — ignoring the link (`railway link` to fix it)."
                        )
                        .yellow()
                    );
                    if let Some(default) = prefs.default_project.as_ref() {
                        eprintln!(
                            "{}",
                            format!(
                                "Using your default cloud agents project instead: {} ({}).",
                                default.project_name, default.environment_name
                            )
                            .dimmed()
                        );
                    }
                    None
                }
                None => Some((project_id, environment_id)),
            }
        }
        None => None,
    };

    match choose_target(args, prefs.default_project.as_ref(), linked) {
        // Either flag means the caller is targeting deliberately; hand both to
        // the shared resolver so `-p` alone still finds an environment.
        TargetSource::Flags => {
            // Two UUIDs need no resolving — the TUI retargets launches with
            // ids it already resolved, and the shared resolver was spending a
            // round-trip re-answering a known question on every TUI launch.
            // Trusting them is safe: the create/wake mutations validate the
            // environment (and the caller's access to it) authoritatively.
            if let (Some(project), Some(environment)) = (&args.project, &args.environment)
                && is_uuid(project)
                && is_uuid(environment)
            {
                return Ok((project.clone(), environment.clone()));
            }
            resolve_project_and_env(
                configs,
                client,
                args.project.clone(),
                args.environment.clone(),
            )
            .await
        }
        TargetSource::Configured(project_id, environment_id)
        | TargetSource::Linked(project_id, environment_id) => Ok((project_id, environment_id)),
        TargetSource::Setup => {
            println!(
                "{}",
                "No default project for cloud agents yet — let's set one up.".dimmed()
            );
            crate::commands::cloud_agent::setup::command(Default::default()).await?;
            *prefs = AgentPrefs::load_in(home).unwrap_or_default();

            match prefs.default_project.clone() {
                Some(default) => Ok((default.project_id, default.environment_id)),
                // They skipped the question. Fall back to the one-off picker so
                // the launch they asked for still happens.
                None => resolve_project_and_env(configs, client, None, None).await,
            }
        }
        TargetSource::Ask => resolve_project_and_env(configs, client, None, None).await,
    }
}

/// The single stdin stream `provision_script_with_skills` reads: the
/// credential (exactly `len` bytes, which the script consumes via `head -c`)
/// followed by the skills tarball. The framing length and the written bytes
/// MUST come from the same buffer — this function is the only place both are
/// produced, which is the contract the byte-framing test pins.
fn combined_provision_payload(
    credential: Option<&[u8]>,
    tarball: &[u8],
) -> (Vec<u8>, Option<usize>) {
    let len = credential.map(<[u8]>::len);
    let mut payload = Vec::with_capacity(len.unwrap_or(0) + tarball.len());
    if let Some(credential) = credential {
        payload.extend_from_slice(credential);
    }
    payload.extend_from_slice(tarball);
    (payload, len)
}

/// A canonical 8-4-4-4-12 lowercase-or-uppercase hex UUID, the only shape
/// Railway ids take. Names can't collide with it in practice, and a
/// pathological UUID-shaped name merely skips a convenience resolution — the
/// mutation that follows still validates the id.
fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => *b == b'-',
        _ => b.is_ascii_hexdigit(),
    })
}

/// Which answer wins, given what is available. Separated from the I/O so the
/// order is checked by tests rather than by reading it.
#[derive(Debug, PartialEq, Eq)]
enum TargetSource {
    /// `-p`/`-e` were given; the shared resolver takes it from here.
    Flags,
    Configured(String, String),
    Linked(String, String),
    /// Nothing to go on, and someone to ask: run `railway ca setup`.
    Setup,
    /// Nothing to go on and nobody to ask — the one-off picker, which errors
    /// with instructions when it cannot prompt either.
    Ask,
}

fn choose_target(
    args: &LaunchArgs,
    configured: Option<&DefaultProject>,
    linked: Option<(String, String)>,
) -> TargetSource {
    if args.project.is_some() || args.environment.is_some() {
        return TargetSource::Flags;
    }
    // A linked directory is an explicit, per-directory statement of intent, so
    // it outranks the person-wide configured default.
    if let Some((project_id, environment_id)) = linked {
        return TargetSource::Linked(project_id, environment_id);
    }
    if let Some(default) = configured {
        return TargetSource::Configured(
            default.project_id.clone(),
            default.environment_id.clone(),
        );
    }
    // Only offer setup when there is someone to answer it. The TUI always
    // passes an explicit target, so this cannot fire underneath a frame, and a
    // script gets the picker's error rather than a prompt it can never answer.
    match is_stdout_terminal() {
        true => TargetSource::Setup,
        false => TargetSource::Ask,
    }
}

/// Why a linked target can no longer be launched into, phrased for the "This
/// directory is linked to a …" warning — or `None` when the link is usable.
///
/// Inconclusive is usable: an error other than "not found" (auth, network)
/// says nothing about the link itself, and treating it as stale would let a
/// blip reroute the launch to the configured default — a different project —
/// instead of failing where the caller can see why. Separated from the I/O so
/// each verdict is checked by tests rather than by reading it.
fn stale_link_reason(
    lookup: &std::result::Result<queries::RailwayProject, RailwayError>,
    environment_id: &str,
) -> Option<&'static str> {
    match lookup {
        Err(RailwayError::ProjectNotFound) => Some("project that no longer exists"),
        Err(_) => None,
        Ok(project) if project.deleted_at.is_some() => Some("project that was deleted"),
        Ok(project) => {
            let live = project
                .environments
                .edges
                .iter()
                .any(|edge| edge.node.id == environment_id && edge.node.deleted_at.is_none());
            (!live).then_some("environment that no longer exists")
        }
    }
}

async fn resolve_agent(
    configs: &mut Configs,
    client: &reqwest::Client,
    args: &LaunchArgs,
    environment_id: &str,
    progress: &dyn Progress,
    access: &RelayAccess,
) -> Result<(CodeAgent, bool, Option<std::path::PathBuf>)> {
    let backboard = configs.get_backboard();

    // An explicit agent wins over everything: the caller is looking at the one
    // it means. Inferring from the stored pointer instead is how "new session
    // on this agent" turned into a second VM.
    let candidate = match (&args.agent_id, args.new) {
        (Some(id), _) => Some(id.clone()),
        (None, true) => None,
        (None, false) => match configs.get_code_agent(environment_id) {
            Some(id) => Some(id),
            None => sole_owned_agent_id(client, &backboard, environment_id).await?,
        },
    };
    // Re-read by id either way, so both paths carry the same shape and the
    // stale-pointer case (agent deleted elsewhere) collapses into `None`.
    let existing = match candidate {
        Some(id) => fetch_agent(client, &backboard, environment_id, &id).await?,
        None => None,
    };
    if let Some(agent) = existing {
        if let Some((ready, probe_master)) =
            ready_existing_agent(client, &backboard, environment_id, agent, progress, access)
                .await?
        {
            warn_ignored_variables(args, progress);
            configs.set_code_agent(environment_id, &ready.id);
            configs.write()?;
            return Ok((ready, false, probe_master));
        }
        configs.remove_code_agent(environment_id);
    }

    let variables = crate::controllers::cloud_agent::with_default_variables(
        variables_to_input(&args.env_files, &args.variables)?
            .map(serde_json::to_value)
            .transpose()?,
    );
    progress.step("Creating a cloud agent");
    let create_started = std::time::Instant::now();
    let create = post_graphql::<mutations::CloudAgentCreate, _>(
        client,
        &backboard,
        mutations::cloud_agent_create::Variables {
            input: mutations::cloud_agent_create::CloudAgentCreateInput {
                environment_id: environment_id.to_owned(),
                name: args.name.clone(),
                variables,
            },
        },
    )
    .await;
    ssh_tel::record_stage("create_mutation", create_started.elapsed(), create.is_ok());
    let created = match create {
        Ok(res) => res.cloud_agent_create,
        Err(e) => return Err(e.into()),
    };

    // Remembered before the box is up: a create that succeeds and then times out
    // waiting has still spent a VM, and the pointer is the only handle the next
    // run has to it.
    configs.set_code_agent(environment_id, &created.id);
    configs.write()?;

    match wait_until_connectable(
        client,
        &backboard,
        environment_id,
        &created.id,
        access,
        // A created VM's measured routability floor; see the wait's doc.
        std::time::Duration::from_millis(350),
    )
    .await
    {
        Ok((running, probe_master)) => {
            progress.note(&format!("Created agent {}", running.name));
            Ok((running, true, probe_master))
        }
        Err(e) => {
            progress.finish();
            Err(e)
        }
    }
}

/// `--variable`/`--env-file` only reach the VM spec at create time, so say so
/// rather than silently dropping them on a reuse.
///
/// Through the progress sink rather than straight to stderr: these flags can
/// now arrive on a launch that opens in the TUI's pane, and a stray write there
/// lands on top of the frame.
fn warn_ignored_variables(args: &LaunchArgs, progress: &dyn Progress) {
    use colored::Colorize;
    if !args.variables.is_empty() || !args.env_files.is_empty() {
        progress.note(
            &"Note: --variable/--env-file only apply when an agent is created — reusing this environment's. Add --new to create with these variables."
                .yellow()
                .to_string(),
        );
    }
}

/// `railway code --rm`: destroy this environment's agent, disk and all.
///
/// Kept working for anyone with it in a script, but `railway ca delete` is the
/// command now: it can name any agent rather than only this environment's, and
/// it confirms before taking a disk away.
async fn destroy_agent(
    configs: &mut Configs,
    client: &reqwest::Client,
    environment_id: &str,
) -> Result<()> {
    use colored::Colorize;
    eprintln!(
        "{}",
        "Note: `railway ca delete` supersedes --rm — it names any agent and confirms first."
            .dimmed()
    );
    let backboard = configs.get_backboard();
    let Some(id) = configs.get_code_agent(environment_id) else {
        println!("No agent recorded for this environment.");
        return Ok(());
    };
    let name = fetch_agent(client, &backboard, environment_id, &id)
        .await?
        .map(|a| a.name);
    // Forget the pointer either way: a delete that reports failure on an
    // already-gone agent must not leave the CLI reaching for it forever.
    configs.remove_code_agent(environment_id);
    configs.write()?;
    match name {
        Some(name) => {
            post_graphql::<mutations::CloudAgentDelete, _>(
                client,
                &backboard,
                mutations::cloud_agent_delete::Variables { id },
            )
            .await?;
            println!("✓ Deleted agent {name}");
        }
        None => println!("Agent {id} is already gone."),
    }
    Ok(())
}

/// The harness a launch would use right now: the saved default, or
/// `RAILWAY_CA_AGENT`, or — on a terminal — one prompt whose answer is saved.
///
/// Public so `railway ca ssh` can start a session on an agent without
/// duplicating that precedence. It takes no flags because the lifecycle verbs
/// have none: choosing a harness per-invocation is what `railway ca start` and
/// `railway code` are for.
pub fn default_harness() -> Result<&'static str> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to get home directory"))?;
    let mut prefs = AgentPrefs::load_in(&home).unwrap_or_default();
    // The slug, not the remote binary name: this feeds `LaunchArgs::for_target`,
    // which matches a harness flag by slug, and it's echoed back to the user
    // in `start_session`'s "starting {harness}" line.
    Ok(resolve_agent_choice(&LaunchArgs::default(), &mut prefs, &home)?.slug())
}

/// Environment override for the saved default — one run, no file write. For
/// CI and for scripts that must not depend on whatever a machine happens to
/// have configured.
const AGENT_ENV_VAR: &str = "RAILWAY_CA_AGENT";

/// Which agent to launch: an explicit flag, then `RAILWAY_CA_AGENT`, then the
/// saved default, then — on a terminal — one prompt, whose answer is saved so
/// the question is asked once. Without a terminal there is nothing to fall back
/// on, so it fails with the two ways to fix it.
///
/// `prefs` is updated in place when the prompt runs, so the caller's copy
/// reflects what was saved.
fn resolve_agent_choice(args: &LaunchArgs, prefs: &mut AgentPrefs, home: &Path) -> Result<Agent> {
    let flagged: Vec<Agent> = [
        (args.codex, Agent::Codex),
        (args.claude, Agent::Claude),
        (args.grok, Agent::Grok),
        (args.railway, Agent::Railway),
        (args.shell, Agent::Shell),
    ]
    .into_iter()
    .filter_map(|(set, agent)| set.then_some(agent))
    .collect();
    match flagged.as_slice() {
        [agent] => return Ok(*agent),
        [] => {}
        _ => bail!("Pick one agent: --codex, --claude, --grok, or --railway."),
    }

    if let Ok(slug) = std::env::var(AGENT_ENV_VAR) {
        let slug = slug.trim().to_lowercase();
        if !slug.is_empty() {
            let agent = Agent::from_slug(&slug).ok_or_else(|| {
                anyhow!(
                    "{AGENT_ENV_VAR}={slug} is not a known agent (claude, codex, grok, railway, or shell)."
                )
            })?;
            return Ok(agent);
        }
    }

    if let Some(agent) = prefs.agent.as_deref().and_then(Agent::from_slug) {
        eprintln!(
            "{}",
            format!(
                "Launching {} — your default configuration in {}. You can change this by running `railway ca setup`.",
                agent.display(),
                AgentPrefs::path_in(home).display()
            )
            .dimmed()
        );
        return Ok(agent);
    }

    if !is_stdout_terminal() {
        bail!(
            "No default agent configured. Run `railway ca setup`, pass a flag \
             (`railway ca --claude`), or set {AGENT_ENV_VAR}=claude."
        );
    }

    let slug = crate::commands::cloud_agent::setup::prompt_agent(home, None)?;
    let agent = Agent::from_slug(&slug).ok_or_else(|| anyhow!("Unknown agent selected: {slug}"))?;
    prefs.agent = Some(slug);
    // Saving is the whole point of asking — but a preferences file that could
    // not be written must not sink a launch the user is already committed to.
    match prefs.save_in(home) {
        Ok(()) => eprintln!(
            "{}",
            "Saved as your default (`railway ca setup` to change).".dimmed()
        ),
        Err(err) => eprintln!("{}", format!("Couldn't save your choice: {err}").yellow()),
    }
    Ok(agent)
}

// ---------------------------------------------------------------------------
// Progress reporting.
//
// Preparing an agent is the same work whether a shell or a TUI asked for it,
// but the two cannot share an output channel: the CLI writes spinners and lines
// to the terminal, and the TUI owns that terminal, where a stray write lands on
// top of the frame. So the pipeline reports through this sink and the caller
// decides what a step looks like.
// ---------------------------------------------------------------------------

/// Where the launch pipeline reports what it is doing.
pub trait Progress: Send + Sync {
    /// A new phase began; the previous one finished.
    fn step(&self, text: &str);
    /// Something worth saying that isn't a phase.
    fn note(&self, text: &str);
    /// The pipeline is done, successfully or not.
    fn finish(&self);
}

/// Terminal progress: one shimmer spinner at a time, notes as plain lines.
#[derive(Default)]
pub struct CliProgress {
    spinner: std::sync::Mutex<Option<indicatif::ProgressBar>>,
}

impl Progress for CliProgress {
    fn step(&self, text: &str) {
        let mut slot = self.spinner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = slot.take() {
            previous.finish_and_clear();
        }
        *slot = Some(create_shimmer_spinner(text));
    }

    fn note(&self, text: &str) {
        // Suspend the spinner so the line doesn't interleave with its frames.
        let slot = self.spinner.lock().unwrap_or_else(|e| e.into_inner());
        match slot.as_ref() {
            Some(spinner) => spinner.suspend(|| eprintln!("{text}")),
            None => eprintln!("{text}"),
        }
    }

    fn finish(&self) {
        let mut slot = self.spinner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(spinner) = slot.take() {
            spinner.finish_and_clear();
        }
    }
}

/// Everything the caller needs to open a session on a prepared agent, and to
/// put it back to sleep afterwards.
pub struct Prepared {
    pub remote_cmd: String,
    pub ssh_target: String,
    pub identity: Option<std::path::PathBuf>,
    pub relay_opts: Vec<String>,
    pub agent_id: String,
    pub agent_name: String,
    pub environment_id: String,
    pub harness: &'static str,
    /// True when this run created the agent, which only affects messaging.
    pub created: bool,
}

/// Where a launch lands and what it runs there, settled before anything is
/// spent on it.
pub struct ResolvedLaunch {
    pub project_id: String,
    pub environment_id: String,
    /// The harness slug, matching [`Agent::slug`].
    pub harness: &'static str,
}

/// Answer a launch's two unavoidable questions — where, and which harness —
/// using the same order the direct path uses.
///
/// Split out so the pane path can settle both *before* the TUI takes the
/// screen. Either answer can print, prompt, or run `railway ca setup` inline,
/// and none of that survives underneath a ratatui frame.
///
/// The target goes first because `railway ca setup` is one of its answers, and
/// setup also writes the default harness — asking for the harness first would
/// ask a question setup is about to ask again.
pub async fn resolve_launch(
    args: &LaunchArgs,
    configs: &mut Configs,
    client: &reqwest::Client,
) -> Result<ResolvedLaunch> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to get home directory"))?;
    let mut prefs = AgentPrefs::load_in(&home).unwrap_or_default();
    let (project_id, environment_id) =
        resolve_target(configs, client, args, &mut prefs, &home).await?;
    let harness = resolve_agent_choice(args, &mut prefs, &home)?.slug();
    Ok(ResolvedLaunch {
        project_id,
        environment_id,
        harness,
    })
}

pub async fn launch(args: LaunchArgs) -> Result<()> {
    use colored::Colorize;

    // `--rm` is a lifecycle action, not a launch: it needs no agent choice and
    // no credential, so it resolves the environment and returns.
    if args.rm {
        let mut configs = Configs::new()?;
        let client = GQLClient::new_authorized(&configs)?;
        let (_project_id, environment_id) =
            resolve_project_and_env(&mut configs, &client, args.project, args.environment).await?;
        return destroy_agent(&mut configs, &client, &environment_id).await;
    }

    eprintln!(
        "{}",
        "Warning: Railway cloud agents are experimental and APIs may change or break during testing."
            .yellow()
    );

    let progress = CliProgress::default();
    // The flag check races prepare instead of preceding it: an un-flagged user
    // is still stopped the moment the check answers — before any mint or
    // create completes — while everyone else no longer pays a serialized
    // round-trip for a question whose answer is almost always yes. If prepare
    // somehow finishes first, the gate is still enforced before anything runs.
    let ensure_fut = async {
        let configs = Configs::new()?;
        let client = GQLClient::new_authorized(&configs)?;
        crate::commands::cloud_agent::access::ensure_enabled(&client, &configs).await
    };
    let prepare_fut = prepare(&args, &progress, SessionStyle::FullTerminal);
    tokio::pin!(ensure_fut);
    tokio::pin!(prepare_fut);
    let prepared = tokio::select! {
        enabled = &mut ensure_fut => {
            enabled?;
            prepare_fut.await?
        }
        prepared = &mut prepare_fut => {
            ensure_fut.await?;
            prepared?
        }
    };
    progress.finish();

    println!("Launching {}…", prepared.harness);
    let exit_code = run_session(&prepared)?;

    // The user's work is done; give detached telemetry a bounded window so a
    // short exec launch (`railway code -- <cmd>`) doesn't exit before its
    // outcome event leaves the machine. Interactive sessions resolve this
    // instantly — their sends finished minutes ago.
    ssh_tel::drain_detached(std::time::Duration::from_secs(2)).await;

    // Belt-and-suspenders for the remote reset: when the connection drops
    // mid-TUI the remote printf never reaches us, so scrub locally too before
    // printing anything. No-op on a clean terminal.
    if std::io::stdout().is_terminal() {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(TERMINAL_RESET.as_bytes());
        let _ = out.flush();
    }

    // Disconnecting no longer sleeps the agent: sleep kills every process on
    // the VM — including the durable session just detached from — while the
    // platform keeps listing those sessions as running, so the next reattach
    // landed on a dead name and a blank screen. Sleeping is deliberate now.
    println!(
        "\nDisconnected — agent {} is still running. `railway ca sleep {}` stops the compute bill.",
        prepared.agent_name.cyan(),
        prepared.agent_name
    );

    if prepared.created {
        println!("Agents persist between runs — this one is yours until you --rm it.");
    }
    println!("Get back in:");
    // There is no --shell flag to point at; the ssh spelling is the way back
    // into a bare shell.
    if prepared.harness == "shell" {
        println!(
            "  railway ca ssh {} -- bash   # wakes it and opens a plain shell",
            prepared.agent_name
        );
    } else {
        println!(
            "  railway code --{}   # wakes it and drops back into {}",
            prepared.harness, prepared.harness
        );
    }
    println!(
        "  railway ca ssh {}   # same, by name — and reattaches your session",
        prepared.agent_name
    );
    // The relay speaks plain ssh too, and `railway ca ssh -- bash` is that
    // without having to hold the target format. Only useful while the agent is
    // awake — hence after the commands that wake it.
    println!(
        "  railway ca ssh {} -- bash   # plain shell",
        prepared.agent_name
    );
    println!("Destroy it:");
    println!("  railway ca delete {}", prepared.agent_name);

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

/// Run the prepared session in *this* terminal, handing stdin/stdout to ssh.
pub fn run_session(prepared: &Prepared) -> Result<i32> {
    let cmd = vec![prepared.remote_cmd.clone()];
    let code = run_native_ssh_with_opts(
        &prepared.ssh_target,
        Some(&cmd),
        prepared.identity.as_deref(),
        None,
        &prepared.relay_opts,
    );
    crate::commands::ssh::native::clear_mouse_tracking();
    code
}

/// Everything between "the user asked" and "there is a session to open":
/// credential, skills, the agent itself, and provisioning it.
///
/// Split out of [`launch`] so a TUI can drive the same pipeline and render the
/// steps itself. The one thing that cannot happen here is an interactive Claude
/// mint — see [`ensure_claude_credential_cached`], which a TUI caller runs
/// before it takes the screen.
pub async fn prepare(
    args: &LaunchArgs,
    progress: &dyn Progress,
    style: SessionStyle,
) -> Result<Prepared> {
    // Timed and reported separately from `prepare_inner` so every caller
    // (`railway code`, `railway ca start`, and the TUI's `start_launch`) gets
    // the same outcome event without duplicating it at each call site — none
    // of the three otherwise see the others' launch attempts at all. Started
    // before the harness is even resolved so a bad `--codex --claude`
    // combination or a non-interactive run with no default agent — a real
    // failure someone hits — still reports, rather than silently dropping
    // out of the funnel before there's a harness to tag it with.
    let start = std::time::Instant::now();
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to get home directory"))?;
    let mut prefs = AgentPrefs::load_in(&home).unwrap_or_default();
    let agent = match resolve_agent_choice(args, &mut prefs, &home) {
        Ok(agent) => agent,
        Err(err) => {
            crate::commands::cloud_agent::telemetry::track_launch_outcome(
                "unresolved",
                None,
                start.elapsed(),
                Some(&format!("{err:#}")),
            )
            .await;
            return Err(err);
        }
    };

    let result = prepare_inner(args, progress, agent, prefs, &home, style).await;
    // After the outcome, and detached: the stages are already measured, and
    // reporting them must not extend the launch they describe.
    ssh_tel::flush_stages("cloud_agent_launch");
    match &result {
        // Detached on success for the same reason as the stage flush: the
        // outcome event is an HTTP round-trip, and awaiting it sat between
        // "provisioned" and "session opens" on every launch. The session that
        // follows gives the send minutes of runway.
        Ok(prepared) => crate::commands::cloud_agent::telemetry::track_launch_outcome_detached(
            agent.slug(),
            Some(prepared.created),
            start.elapsed(),
        ),
        // Still awaited: the process is about to exit with this error, and a
        // spawned task would be dropped before the failure ever reported.
        Err(e) => {
            crate::commands::cloud_agent::telemetry::track_launch_outcome(
                agent.slug(),
                None,
                start.elapsed(),
                Some(&format!("{e:#}")),
            )
            .await
        }
    }
    result
}

async fn prepare_inner(
    args: &LaunchArgs,
    progress: &dyn Progress,
    agent: Agent,
    mut prefs: AgentPrefs,
    home: &Path,
    style: SessionStyle,
) -> Result<Prepared> {
    // --- Resolve the local credential (client-side only, announced).
    //
    // Only the cheap sources run here. Codex and Grok read a local file, and a
    // missing one means the session starts unauthenticated rather than not at
    // all. Claude's mint costs a browser round-trip, so it is deferred until we
    // know whether the agent already holds a credential from a previous run —
    // see `PendingAuth`.
    let pending = match agent {
        Agent::Codex | Agent::Grok => {
            ssh_tel::timed_for("cloud_agent_launch", "credential", async {
                local_signin(agent, home)
            })
            .await?
        }
        Agent::Claude => {
            ssh_tel::timed_for("cloud_agent_launch", "credential", async {
                claude_credentials_cheap(args.refresh_auth)
            })
            .await?
        }
        // Nothing to read or mint — the VM already carries its own, and a
        // plain shell has nothing to sign in to.
        Agent::Railway | Agent::Shell => PendingAuth::None,
    };
    match pending {
        PendingAuth::Ready { ref source, .. } => progress.note(&format!(
            "Using your {} credential ({source}) on the agent",
            agent.display()
        )),
        // Said up front, before the VM: the sign-in is the first thing waiting
        // on the other end, and finding that out on arrival reads as a bug.
        PendingAuth::SignInOnAgent { ref note } => progress.note(note),
        PendingAuth::None if agent == Agent::Shell => {
            progress.note("No coding agent — opening a plain shell on the VM")
        }
        PendingAuth::None => progress.note("Using the agent's own integrated Railway credentials"),
        PendingAuth::MintClaude => {}
    }
    // Pack the user's skills before spending a VM: a skills directory that has
    // grown into something unshippable should fail here, not after a create.
    // The upload itself is decided later, against the hash the agent reports.
    let packed_skills = ssh_tel::timed_for("cloud_agent_launch", "skills_pack", async {
        skills_sync::pack(&prefs, home)
    })
    .await?;
    if let Some(packed) = &packed_skills {
        progress.note(&format!(
            "Including {} of your skills ({})",
            packed.names.len(),
            packed.source_dir.display()
        ));
    }
    // The launch directory's project MCP servers travel too — the `.mcp.json`
    // the repo committed is what "the servers this project's people get"
    // means. A broken file is said out loud but never blocks the launch: the
    // file is usually a teammate's commit, and one bad merge upstream must
    // not take everyone's `railway ca` down with it.
    let packed_mcp = match std::env::current_dir().map(|cwd| mcp_sync::pack(&prefs, &cwd)) {
        Ok(Ok(packed)) => packed,
        Ok(Err(err)) => {
            progress.note(&format!("Skipping MCP import: {err:#}"));
            None
        }
        Err(_) => None,
    };
    if let Some(packed) = &packed_mcp {
        progress.note(&format!(
            "Including {} MCP servers from the project ({})",
            packed.names.len(),
            packed.source_path.display()
        ));
    }

    // --- Resolve where the agent lives.
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    // The SSH key check is independent of where the agent lives, and the waits
    // inside resolve_agent need its result to probe with — so it runs alongside
    // target resolution instead of after the agent is already up, where its
    // round-trip (registered-keys query, plus a register mutation on first run)
    // was pure added latency. Its own `Configs` because resolve_target holds
    // the mutable borrow.
    // The project is no longer carried on `Prepared` — its only reader was the
    // launcher's exit hint, which now names the agent instead.
    let (target_res, identity) = tokio::join!(
        ssh_tel::timed_for(
            "cloud_agent_launch",
            "resolve_target",
            resolve_target(&mut configs, &client, args, &mut prefs, home),
        ),
        ssh_tel::timed_for("cloud_agent_launch", "ssh_key", async {
            let key_configs = Configs::new()?;
            // Non-interactive inside the join: resolve_target can be running
            // its own picker/setup prompts concurrently, and two prompt flows
            // interleaved on one terminal are gibberish. The rare
            // needs-registration case retries interactively below, after the
            // join, when the terminal is free again.
            crate::commands::ssh::native::ensure_ssh_key_noninteractive(&client, &key_configs).await
        }),
    );
    let (_project_id, environment_id) = target_res?;
    let identity = match identity {
        Ok(identity) => identity,
        Err(_) => {
            let key_configs = Configs::new()?;
            ssh_tel::timed_for(
                "cloud_agent_launch",
                "ssh_key_interactive",
                ensure_ssh_key_quiet(&client, &key_configs),
            )
            .await?
        }
    };

    let relay = ssh_tel::timed_for("cloud_agent_launch", "relay", async { relay_ssh() }).await?;
    let access = RelayAccess {
        identity: identity.clone(),
        relay_opts: relay.opts.clone(),
    };

    let (cloud_agent, created, probe_master) = ssh_tel::timed_for(
        "cloud_agent_launch",
        "resolve_agent",
        resolve_agent(
            &mut configs,
            &client,
            args,
            &environment_id,
            progress,
            &access,
        ),
    )
    .await?;
    configs.set_code_agent(&environment_id, &cloud_agent.id);
    configs.write()?;

    // The relay's cloud-agent grammar; by id rather than name because names are
    // not unique within an environment.
    let target = format!("agent:{environment_id}:{}", cloud_agent.id);

    // --- Deferred Claude mint. A setup-token lasts a year and the agent's disk
    // survives sleep, so a reused agent is normally still authenticated; minting
    // again would spend an OAuth round-trip to overwrite a working credential.
    // Probe first, and only pay for the flow when there is nothing there.
    let auth = match pending {
        PendingAuth::Ready { line, source } => Some((line, source)),
        PendingAuth::SignInOnAgent { .. } | PendingAuth::None => None,
        PendingAuth::MintClaude => {
            // A fresh agent has nothing to inherit, and --refresh-auth is an
            // explicit request to replace whatever is there; neither needs a probe.
            let needs_probe = !created && !args.refresh_auth;
            let inherit = if needs_probe {
                let probe = ssh_tel::timed_for("cloud_agent_launch", "claude_probe", async {
                    ssh_plumbing(
                        &target,
                        CLAUDE_CREDENTIAL_PROBE,
                        identity.as_deref(),
                        None,
                        &relay,
                        None,
                    )
                })
                .await?;
                String::from_utf8_lossy(&probe).contains("CRED-PRESENT")
            } else {
                false
            };
            if inherit {
                progress.note(
                    "Reusing the Claude credential already on this agent (--refresh-auth to replace it)",
                );
                None
            } else {
                match ssh_tel::timed_for("cloud_agent_launch", "claude_mint", async {
                    mint_claude_credentials()
                })
                .await?
                {
                    Some((line, source)) => {
                        progress.note(&format!(
                            "Using your Claude Code credential ({source}) on the agent"
                        ));
                        Some((line, source))
                    }
                    // Nothing to mint with, or nothing pasted: launch anyway
                    // and let claude run its own sign-in on the agent.
                    None => {
                        progress.note(&claude_sign_in_note());
                        None
                    }
                }
            }
        }
    };

    // --- Provision: credential (stdin) + reconnect seeds, one script.
    progress.step("Finalizing Configuration...");
    // One relay handshake per launch: when a readiness probe won the wait, its
    // marker-verified connection is already a master and both the provision
    // and the session ride it. When no probe ran (agent already RUNNING, or
    // the status flip won the race), the provision opens the master instead.
    // That one is verified only after the fact — the master exists from
    // connect time, and a marker failure does not tear it down — which is why
    // ssh_plumbing removes the socket on every failed attempt: no retry (and
    // no session) can ride a connection whose attempt didn't produce the
    // marker. See the RelaySsh doc for the history this guards against.
    let (master_socket, mux_provision) = match probe_master {
        Some(socket) => {
            let client_opts = mux_client_opts(&socket);
            (socket, client_opts)
        }
        None => {
            let socket = mux_socket();
            let master_opts = mux_master_opts(&socket, "30s");
            (socket, master_opts)
        }
    };
    let mux_client = mux_client_opts(&master_socket);
    let provision_relay = {
        let mut r = relay.clone();
        r.opts.extend(mux_provision);
        r
    };
    let provision = async {
        let target = target.clone();
        let identity = identity.clone();
        let relay = provision_relay.clone();
        let master_socket = master_socket.clone();
        // Copied out of `args`, which is a borrow the 'static closure can't take.
        let app_mode = args.app_mode;
        let skills_note = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let notes = skills_note.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<()> {
            let push = |line: String| {
                if let Ok(mut n) = notes.lock() {
                    n.push(line);
                }
            };
            let skills_note = |out: &str| {
                let reason = if out.contains("SKILLS-NO-TAR") {
                    "the agent has no `tar`"
                } else if out.contains("SKILLS-EXTRACT-FAILED") {
                    "the transfer did not unpack"
                } else {
                    "the sync did not report success"
                };
                format!(
                    "Couldn't sync your skills onto the agent ({reason}); continuing without them."
                )
            };
            let mcp_note = |out: &str| {
                let reason = if out.contains("MCP-NO-JQ") {
                    "the agent has no `jq`"
                } else if out.contains("MCP-BAD-JSON") {
                    "the payload did not survive the trip"
                } else if out.contains("MCP-MERGE-FAILED") {
                    "a harness config would not merge"
                } else {
                    "the sync did not report success"
                };
                format!(
                    "Couldn't sync the project's MCP servers ({reason}); continuing without them."
                )
            };
            // The project's MCP servers, merged add-only into the harness
            // configs on the agent. Never fatal, same trade as skills: the
            // agent is fully usable without them.
            let sync_mcp = |packed: &mcp_sync::PackedMcp| -> Result<()> {
                let out = ssh_plumbing(
                    &target,
                    &mcp_sync::provision_script(&packed.hash),
                    identity.as_deref(),
                    Some(&packed.payload),
                    &relay,
                    Some(&master_socket),
                )?;
                let out = String::from_utf8_lossy(&out);
                if !out.contains("MCP-OK") {
                    push(mcp_note(&out));
                }
                Ok(())
            };
            let check_ready = |out: &str| -> Result<()> {
                if out.contains("AGENT-READY") {
                    Ok(())
                } else if out.contains("AGENT-MISSING") {
                    bail!(
                        "`{}` was not found on the agent (PATH: ~/.local/bin, ~/.opencode/bin, ~/.grok/bin, mise shims). Cloud agents bake every harness, so report this with the agent id rather than retrying.",
                        agent.name()
                    )
                } else {
                    bail!(
                        "Provisioning produced no status marker — the connection likely dropped mid-script."
                    )
                }
            };

            // A fresh agent cannot already hold the skills, so the two-step
            // report-then-upload costs a relay round-trip that answers a known
            // question. Send everything in one connection: the credential
            // (length-framed) and the tarball share the provision stdin.
            if created && let Some(packed) = &packed_skills {
                let (payload, credential_len) = combined_provision_payload(
                    auth.as_ref().map(|(line, _)| line.as_slice()),
                    &packed.tarball,
                );
                let out = ssh_plumbing(
                    &target,
                    &provision_script_with_skills(agent, credential_len, app_mode, &packed.hash),
                    identity.as_deref(),
                    Some(&payload),
                    &relay,
                    Some(&master_socket),
                )?;
                let out = String::from_utf8_lossy(&out);
                check_ready(&out)?;
                // Never fatal: the agent is fully usable without skills, and
                // losing a session over a skills copy would be a worse trade
                // than launching without one.
                if !out.contains("SKILLS-OK") {
                    push(skills_note(&out));
                }
                // A fresh agent cannot already hold the MCP set either.
                if let Some(packed) = &packed_mcp {
                    sync_mcp(packed)?;
                }
                return Ok(());
            }

            let out = ssh_plumbing(
                &target,
                &provision_script(agent, auth.is_some(), app_mode),
                identity.as_deref(),
                auth.as_ref().map(|(line, _)| line.as_slice()),
                &relay,

                Some(&master_socket),
            )?;
            let out = String::from_utf8_lossy(&out);
            check_ready(&out)?;
            // Skills, when the set on the agent isn't already the set on this
            // machine. The hash rides the script above, so an unchanged set
            // costs nothing beyond the round-trip we already made.
            if let Some(packed) = packed_skills {
                // Already in sync: nothing to do, and nothing worth a line.
                if skills_sync::parse_remote_hash(&out).as_deref() != Some(packed.hash.as_str()) {
                    let out = ssh_plumbing(
                        &target,
                        &skills_sync::provision_script(&packed.hash),
                        identity.as_deref(),
                        Some(&packed.tarball),
                        &relay,

                        Some(&master_socket),
                    )?;
                    let out = String::from_utf8_lossy(&out);
                    if !out.contains("SKILLS-OK") {
                        push(skills_note(&out));
                    }
                }
            }
            // MCP by the same hash dance: the marker rode the script above,
            // so an unchanged `.mcp.json` costs nothing beyond the round-trip
            // already made.
            if let Some(packed) = &packed_mcp
                && mcp_sync::parse_remote_hash(&out).as_deref() != Some(packed.hash.as_str())
            {
                sync_mcp(packed)?;
            }
            Ok(())
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
        for line in skills_note.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            progress.note(line);
        }
        result
    };
    ssh_tel::timed_for("cloud_agent_launch", "provision", provision).await?;

    let env_prefix = format!(
        "{HARNESS_PATH}; [ -f ~/.gh-token ] && export GH_TOKEN=\"$(cat ~/.gh-token)\"; {CLAUDE_ENV_GUARD}; "
    );
    let remote_cmd = remote_command(
        agent,
        &env_prefix,
        args.initial_prompt.as_deref(),
        &args.agent_args,
        style,
    );

    Ok(Prepared {
        remote_cmd,
        ssh_target: target,
        identity,
        relay_opts: {
            // The session tries the provision connection's master first and
            // falls back to a plain connection if it's gone (it never sets
            // ControlMaster, so it can't create one).
            let mut opts = relay.opts;
            opts.extend(mux_client);
            opts
        },
        agent_id: cloud_agent.id,
        agent_name: cloud_agent.name,
        environment_id,
        harness: agent.slug(),
        created,
    })
}

/// What a session needs to reach an agent over the relay.
pub struct ConnectInfo {
    pub ssh_target: String,
    pub identity: Option<std::path::PathBuf>,
    pub relay_opts: Vec<String>,
}

/// Relay plumbing for an agent that is already running and provisioned.
///
/// Deliberately not [`prepare`]: reconnecting to a session that exists needs no
/// credential, no skills sync, and no provisioning script — the agent was set
/// up when it was created. Skipping all of it is the difference between a
/// reattach that is instant and one that walks the whole launch flow to arrive
/// at a box that was ready the entire time.
pub async fn connect_info(environment_id: &str, agent_id: &str) -> Result<ConnectInfo> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let identity = ensure_ssh_key_quiet(&client, &configs).await?;
    let relay = relay_ssh()?;
    Ok(ConnectInfo {
        ssh_target: format!("agent:{environment_id}:{agent_id}"),
        identity,
        relay_opts: relay.opts,
    })
}

/// How long the pre-sleep flush may take before we sleep the agent anyway.
const FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Flush the agent's filesystem so sleeping it cannot discard recent work.
///
/// `cloudAgentSleep` snapshots the disk without quiescing the guest, so pages
/// still dirty in its page cache are absent from the image the next wake
/// restores. Measured on a scratch agent: a file written a second before a
/// sleep was gone after waking, while the same write followed by `sync`
/// survived — including when the sleep was issued immediately on disconnect.
/// So the variable is the flush, not the timing.
///
/// A side connection is enough because `sync(2)` flushes the whole filesystem
/// regardless of which process dirtied it: this covers whatever the durable
/// session was writing without reaching into it.
///
/// Best-effort by design. Failing to reach the agent must not stop us sleeping
/// it — agents have no idle timeout, so the alternative to an imperfect sleep is
/// a machine that bills until someone remembers it.
pub async fn flush_disk(environment_id: &str, agent_id: &str) {
    // Deliberately not `ssh_plumbing`: its ~20s retry budget exists to cross the
    // gap while a woken agent becomes attachable. Spending it here would make
    // every disconnect slow whenever the relay is having a bad minute, to
    // protect writes on a box we have already failed to reach twice.
    let flush = async {
        let info = connect_info(environment_id, agent_id).await.ok()?;
        let mut opts = info.relay_opts;
        opts.push("-o".to_string());
        opts.push(format!("ConnectTimeout={}", FLUSH_TIMEOUT.as_secs()));
        tokio::task::spawn_blocking(move || {
            run_native_ssh_captured(
                &info.ssh_target,
                "sync",
                info.identity.as_deref(),
                None,
                &opts,
            )
        })
        .await
        .ok()
    };
    // On timeout the blocking ssh is left to finish on its own: it is a `sync`,
    // it harms nothing, and waiting on it is the thing we just declined to do.
    let _ = tokio::time::timeout(FLUSH_TIMEOUT, flush).await;
}

/// End a durable session on an agent.
///
/// There is no API for this — nothing in the stack exposes a kill — but
/// vm-init stamps `RAILWAY_DURABLE_SESSION_NAME` into the environment of every
/// process it starts under a session, so the session's processes can be
/// identified exactly and signalled. Matching on the environment rather than on
/// the command line matters: the command is a long shared launch line, and a
/// `pkill -f` on it would take out every session on the box.
///
/// TERM first so a harness can save what it is holding; the session ends when
/// its process does.
pub async fn kill_session(environment_id: &str, agent_id: &str, session_name: &str) -> Result<()> {
    let info = connect_info(environment_id, agent_id).await?;
    let script = kill_script(session_name);
    let relay = relay_ssh()?;
    let out = tokio::task::spawn_blocking(move || {
        ssh_plumbing(
            &info.ssh_target,
            &script,
            info.identity.as_deref(),
            None,
            &relay,
            None,
        )
    })
    .await??;
    let out = String::from_utf8_lossy(&out);
    match out.split("KILLED:").nth(1).and_then(|n| {
        n.trim()
            .lines()
            .next()
            .and_then(|n| n.trim().parse::<u32>().ok())
    }) {
        Some(0) => bail!("nothing was running under that session"),
        Some(_) => Ok(()),
        None => bail!("the agent did not confirm the session ended"),
    }
}

/// The VM-side script that ends one durable session.
///
/// `grep -z` reads the NUL-separated environ directly. The obvious alternative
/// pipes it through `tr` with a NUL literal in the script — a byte with no
/// business travelling through a source file, a format string and an argv on
/// its way to a shell, and which arrived at ssh as "nul byte found in provided
/// data" rather than as a script.
fn kill_script(session_name: &str) -> String {
    format!(
        r#"killed=0
for p in /proc/[0-9]*; do
  pid="${{p#/proc/}}"
  [ "$pid" = "$$" ] && continue
  if grep -qzxF 'RAILWAY_DURABLE_SESSION_NAME={session_name}' "$p/environ" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null && killed=$((killed+1))
  fi
done
echo "KILLED:$killed""#
    )
}

/// Does a Claude launch need the terminal back before it can start?
///
/// The TUI checks this: a cached token means the whole pipeline can run behind
/// a frame, and so does having nothing to mint with — that launch just goes
/// unauthenticated and claude asks for the sign-in on the agent. Only a mint
/// that can actually run needs the terminal, for its browser wait and paste
/// prompt. An error resolving the credential says yes too, so it surfaces out
/// of frame where it is readable.
pub fn claude_needs_local_mint() -> bool {
    !matches!(
        claude_credentials_cheap(false),
        Ok(PendingAuth::Ready { .. } | PendingAuth::SignInOnAgent { .. })
    )
}

/// Make sure a Claude credential exists locally, running the interactive mint
/// if it does not.
///
/// A TUI caller must do this *before* it takes the terminal: the mint opens a
/// browser and reads a pasted token from stdin, neither of which can happen
/// underneath a ratatui frame. Cheap and silent when the token is already
/// cached, which after the first run it is. A mint that comes away empty is
/// not an error — the launch continues without a credential, and
/// `CLAUDE_MINT_DECLINED` keeps the pipeline from asking a second time under
/// the frame.
pub fn ensure_claude_credential_cached(harness: &str) -> Result<()> {
    if harness != "claude" {
        return Ok(());
    }
    if let PendingAuth::MintClaude = claude_credentials_cheap(false)? {
        match mint_claude_credentials()? {
            Some((_line, source)) => {
                eprintln!("Using your Claude Code credential ({source}) on the agent")
            }
            None => eprintln!("{}", claude_sign_in_note()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// The shapes someone types at a prompt open in the pane. Nothing about a
    /// target, a harness or a variable changes that — they all describe a
    /// session, and a session is what the pane holds.
    #[test]
    fn an_ordinary_launch_opens_in_the_pane() {
        for argv in [
            vec!["code"],
            vec!["code", "--claude"],
            vec!["code", "--new", "--name", "api"],
            vec!["code", "-p", "proj_1", "-e", "env_prod"],
            vec!["code", "--variable", "K=V", "--refresh-auth"],
        ] {
            let args = LaunchArgs::parse_from(&argv);
            assert!(args.pane_shaped(), "{argv:?} should open in the pane");
        }
    }

    /// The two that a frame would break: `--rm` has no session to draw, and
    /// `-- args` is a caller asking for an exit code rather than a window.
    #[test]
    fn destroying_and_exec_take_the_terminal_instead() {
        assert!(!LaunchArgs::parse_from(["code", "--rm"]).pane_shaped());
        assert!(
            !LaunchArgs::parse_from(["code", "--codex", "--", "exec", "explain this"])
                .pane_shaped()
        );
    }

    /// What the TUI knows — where, which harness, which agent — wins, because
    /// the user may have moved since typing the command.
    #[test]
    fn retargeting_overrides_what_the_tui_decides() {
        let args = LaunchArgs::parse_from(["code", "--codex", "-p", "old_p", "-e", "old_e"])
            .retargeted(
                "new_p".into(),
                "new_e".into(),
                "claude",
                true,
                Some("fix the tests".into()),
                Some("ca_1".into()),
            );
        assert_eq!(args.project.as_deref(), Some("new_p"));
        assert_eq!(args.environment.as_deref(), Some("new_e"));
        assert_eq!(args.agent_id.as_deref(), Some("ca_1"));
        assert_eq!(args.initial_prompt.as_deref(), Some("fix the tests"));
        assert!(args.new);
        assert!(args.claude, "the harness the TUI chose");
        assert!(!args.codex, "and only that one");
    }

    /// Everything the TUI has no way to ask for survives the trip through it.
    /// Dropping these would silently ignore what was typed: `railway code
    /// --new --name api --variable K=V` would create an agent with a generated
    /// name and none of the variables.
    #[test]
    fn retargeting_carries_the_flags_the_tui_cannot_ask_for() {
        let args = LaunchArgs::parse_from([
            "code",
            "--new",
            "--name",
            "api",
            "--variable",
            "DB=postgres.DATABASE_URL",
            "--env-file",
            ".env",
            "--refresh-auth",
        ])
        .retargeted("p".into(), "e".into(), "claude", true, None, None);
        assert_eq!(args.name.as_deref(), Some("api"));
        assert_eq!(args.variables, ["DB=postgres.DATABASE_URL"]);
        assert_eq!(args.env_files, [std::path::PathBuf::from(".env")]);
        assert!(args.refresh_auth);
    }

    fn note_of(pending: PendingAuth) -> String {
        match pending {
            PendingAuth::SignInOnAgent { note } => note,
            PendingAuth::Ready { source, .. } => panic!("expected a fallback, got {source}"),
            PendingAuth::MintClaude => panic!("expected a fallback, got a mint"),
            PendingAuth::None => panic!("expected a fallback, got a harness needing no credential"),
        }
    }

    #[test]
    fn a_missing_local_signin_falls_back_to_signing_in_on_the_agent() {
        let home = tempfile::tempdir().unwrap();
        let note = note_of(local_signin(Agent::Codex, home.path()).unwrap());
        assert!(note.contains("codex login --device-auth"), "{note}");

        let note = note_of(local_signin(Agent::Grok, home.path()).unwrap());
        assert!(note.contains("Grok"), "{note}");
    }

    #[test]
    fn an_empty_local_signin_falls_back_too() {
        // Half-finished sign-ins leave the file behind; it carries nothing, so
        // it is the same case as no file at all.
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        std::fs::write(home.path().join(".codex").join("auth.json"), "").unwrap();
        note_of(local_signin(Agent::Codex, home.path()).unwrap());
    }

    #[test]
    fn a_local_signin_is_carried_verbatim() {
        let home = tempfile::tempdir().unwrap();
        let auth = home.path().join(".grok").join("auth.json");
        std::fs::create_dir_all(auth.parent().unwrap()).unwrap();
        std::fs::write(&auth, r#"{"k":1}"#).unwrap();
        match local_signin(Agent::Grok, home.path()).unwrap() {
            PendingAuth::Ready { line, source } => {
                assert_eq!(line, br#"{"k":1}"#);
                // Compared against the constructed path rather than a literal
                // suffix: the separator is the platform's, and `.grok/auth.json`
                // never matches on Windows.
                assert_eq!(source, auth.display().to_string());
            }
            _ => panic!("expected the local sign-in to be carried"),
        }
    }

    #[test]
    fn an_unauthenticated_launch_still_provisions_the_agent() {
        // The credential seed is the only thing the fallback drops: no
        // `cat >` truncating a file we have nothing to write to, and every
        // reconnect seed and readiness marker still there.
        for agent in [Agent::Codex, Agent::Claude, Agent::Grok] {
            let script = provision_script(agent, false, false);
            assert!(!script.contains("cat > ~/"), "{script}");
            assert!(script.contains("railway-code agent autostart"));
            assert!(script.contains("AGENT-READY"));
            assert!(script.contains(&format!("echo {} > ~/.railway-code-agent", agent.name())));
        }
    }

    #[test]
    fn the_claude_fallback_says_how_to_sign_in_on_the_agent() {
        let note = claude_sign_in_note();
        assert!(note.contains("Claude Code"), "{note}");
        assert!(note.contains("/login"), "{note}");
    }

    #[test]
    fn provision_script_delivers_credentials_only() {
        let codex = provision_script(Agent::Codex, true, false);
        assert!(codex.contains("cat > ~/.codex/auth.json"));
        assert!(codex.contains("echo codex > ~/.railway-code-agent"));

        let claude = provision_script(Agent::Claude, true, false);
        assert!(claude.contains("cat > ~/.claude-code-env"));
        assert!(claude.contains("echo claude > ~/.railway-code-agent"));

        let grok = provision_script(Agent::Grok, true, false);
        assert!(grok.contains("cat > ~/.grok/auth.json"));
        assert!(grok.contains("echo grok > ~/.railway-code-agent"));

        for script in [&codex, &claude, &grok] {
            // Shared plumbing: reconnect autostart, env sourcing, and the
            // markers the provisioning caller matches on.
            assert!(script.contains("railway-code agent autostart"));
            assert!(script.contains(". \"$HOME/.claude-code-env\""));
            assert!(script.contains("AGENT-READY"));
            assert!(script.contains("AGENT-MISSING"));

            // An `ssh <target> <cmd>` session is non-interactive and non-login,
            // so it gets neither /root/.profile nor the image ENV. Without this
            // export `command -v claude` reports missing on an image that has
            // it — the bug this assertion exists to prevent.
            assert!(script.contains("$HOME/.local/bin"));
            assert!(script.contains("$HOME/.grok/bin"));

            // The machine's spec sets the cwd for every in-VM session (/app,
            // the workspace dir express-agent reconciles trust into). Forcing
            // $HOME here would override that platform default, so the autostart
            // must launch the agent without a cd of its own.
            assert!(!script.contains("cd \"$HOME\""));
            assert!(!script.contains("cd ~"));

            // Cloud agent VMs bake every harness and reconcile its config at
            // boot, so this script must install nothing and configure nothing:
            // touching those files fights express-agent for ownership, and
            // installing races the image's own copy. These assertions are the
            // guard on that boundary, not incidental.
            assert!(!script.contains("npm install"));
            assert!(!script.contains("install.sh"));
            assert!(!script.contains("apt-get"));
            assert!(!script.contains("hasCompletedOnboarding"));
            assert!(!script.contains("trust_level"));
            assert!(!script.contains("yolo"));
            assert!(!script.contains("config.toml"));
        }
    }

    /// The carried setup-token is the session credential. An on-agent `/login`
    /// must not hide it — that path did not work correctly — so every source
    /// site writes the env file unconditionally, and v4 rewrites any v2/v3
    /// profile that still had the mtime skip.
    #[test]
    fn the_carried_claude_token_is_always_sourced() {
        for guard in [CLAUDE_ENV_GUARD, COMMON_SEED] {
            assert!(
                !guard.contains("credentials.json"),
                "login must not outrank the carried token: {guard}"
            );
            assert!(guard.contains(".claude-code-env"), "{guard}");
        }
        assert!(COMMON_SEED.contains("railway-code agent autostart v4"));
        assert!(COMMON_SEED.contains("sed -i '/# railway-code agent autostart/,/^fi$/d'"));
    }

    /// `railway ca desktop` hands the login shell to an external app, so the
    /// autostart must be off on those agents — and back on if the same agent is
    /// later launched with `railway code`. Both directions are asserted because
    /// only writing one's own marker would inherit the other's.
    #[test]
    fn app_mode_and_session_mode_disagree_about_the_autostart() {
        let app = provision_script(Agent::Claude, false, true);
        assert!(app.contains("touch ~/.railway-app-mode"));
        assert!(app.contains("rm -f ~/.railway-code-agent"));
        assert!(!app.contains("> ~/.railway-code-agent"));

        let session = provision_script(Agent::Claude, false, false);
        assert!(session.contains("rm -f ~/.railway-app-mode"));
        assert!(session.contains("echo claude > ~/.railway-code-agent"));
        assert!(!session.contains("touch ~/.railway-app-mode"));

        // The guard the sentinel exists for.
        assert!(COMMON_SEED.contains(r#"[ ! -f "$HOME/.railway-app-mode" ]"#));
    }

    /// The launch note names the cached token's age once it has one, and only
    /// nags about re-minting when the token is old enough to plausibly be the
    /// problem.
    #[test]
    fn the_cached_token_source_carries_its_age() {
        assert_eq!(cached_token_source(None), "cached setup-token");
        assert_eq!(cached_token_source(Some(0)), "cached setup-token");
        assert_eq!(
            cached_token_source(Some(12)),
            "cached setup-token from 12d ago"
        );
        assert_eq!(
            cached_token_source(Some(92)),
            "cached setup-token from 92d ago — --refresh-auth re-mints"
        );
    }

    // Reusing an agent's existing credential must omit the seed, not run it with
    // empty stdin — `cat > ~/.claude-code-env` would truncate the file we chose
    // to keep.
    #[test]
    fn provision_script_omits_the_seed_when_reusing_a_credential() {
        let claude = provision_script(Agent::Claude, false, false);
        assert!(!claude.contains("cat > ~/.claude-code-env"));
        // Everything else still runs.
        assert!(claude.contains("$HOME/.local/bin"));
        assert!(claude.contains("railway-code agent autostart"));
        assert!(claude.contains("echo claude > ~/.railway-code-agent"));
        assert!(claude.contains("AGENT-READY"));

        for (agent, seed) in [
            (Agent::Codex, "cat > ~/.codex/auth.json"),
            (Agent::Grok, "cat > ~/.grok/auth.json"),
        ] {
            assert!(provision_script(agent, true, false).contains(seed));
            assert!(!provision_script(agent, false, false).contains(seed));
        }
    }

    /// Railway's own harness never has a credential to push — `prepare_inner`
    /// always resolves it to `PendingAuth::None`, so `write_credential` is
    /// always false — but it still runs the reconnect/PATH seeds and reports
    /// its own binary name like every other harness.
    #[test]
    fn railway_needs_no_credential_seed() {
        let script = provision_script(Agent::Railway, false, false);
        for other_seed in [
            "cat > ~/.claude-code-env",
            "cat > ~/.codex/auth.json",
            "cat > ~/.grok/auth.json",
        ] {
            assert!(!script.contains(other_seed), "{script}");
        }
        assert!(script.contains("echo railway-agent-tui > ~/.railway-code-agent"));
        assert!(script.contains("if command -v railway-agent-tui"));
        assert!(script.contains("railway-code agent autostart"));
        assert!(script.contains("AGENT-READY"));
    }

    /// The combined fresh-agent script shares one stdin between the credential
    /// and the skills tarball, so the credential read must be length-framed —
    /// a `cat >` seed would swallow the tarball too.
    #[test]
    fn combined_provision_frames_the_credential() {
        let script = provision_script_with_skills(Agent::Claude, Some(42), false, "abc123");
        assert!(
            script.contains("head -c 42 > ~/.claude-code-env"),
            "{script}"
        );
        assert!(!script.contains("cat > ~/.claude-code-env"), "{script}");
        // The tarball is the rest of the stream, saved before anything can bail.
        assert!(script.contains(r#"cat > "$payload""#), "{script}");
        assert!(script.contains("'abc123'"), "{script}");
    }

    /// The sync block's degradation paths `exit 0`, so the launch's own status
    /// marker must already be printed by the time it runs — AGENT-READY before
    /// the tar check, or a skills failure would read as a dropped connection.
    #[test]
    fn combined_provision_reports_ready_before_skills() {
        let script = provision_script_with_skills(Agent::Claude, Some(10), false, "h");
        let ready = script.find("AGENT-READY").expect("ready marker");
        let sync = script.find("command -v tar").expect("sync block");
        assert!(ready < sync, "{script}");
        // The credential must be consumed before the payload drain, or the
        // tarball's first bytes land in the credential file.
        let cred = script.find("head -c 10").expect("framed credential");
        let drain = script.find(r#"cat > "$payload""#).expect("payload drain");
        assert!(cred < drain, "{script}");
    }

    /// No credential to write (sign-in deferred to the agent): stdin is the
    /// tarball alone, and nothing tries to read a credential off it.
    #[test]
    fn combined_provision_without_credential_reads_only_the_tarball() {
        let script = provision_script_with_skills(Agent::Claude, None, false, "h");
        assert!(!script.contains("head -c"), "{script}");
        assert!(script.contains(r#"cat > "$payload""#), "{script}");
        assert!(script.contains("AGENT-READY"), "{script}");
    }

    /// The byte-framing contract behind `head -c`: the length the script reads
    /// must be exactly the credential bytes at the front of the stream, with
    /// the tarball intact behind them. Anyone who re-encodes the credential or
    /// appends a newline breaks both at once — this test is what catches them.
    #[test]
    fn combined_payload_framing_splits_back_exactly() {
        let credential = b"CLAUDE_CODE_OAUTH_TOKEN=tok-123\n";
        let tarball = [0x1f, 0x8b, 0x08, 0x00, 0x42];
        let (payload, len) = combined_provision_payload(Some(credential), &tarball);
        let len = len.expect("credential present");
        assert_eq!(&payload[..len], credential);
        assert_eq!(&payload[len..], &tarball);
        // And the script consumes exactly that many bytes.
        let script = provision_script_with_skills(Agent::Claude, Some(len), false, "h");
        assert!(script.contains(&format!("head -c {len} ")), "{script}");

        let (payload, len) = combined_provision_payload(None, &tarball);
        assert!(len.is_none());
        assert_eq!(payload, tarball);
    }

    /// UUIDs are trusted as launch targets; anything else still resolves.
    #[test]
    fn uuid_shapes() {
        assert!(is_uuid("ddcba2f8-a773-4929-bfbd-52450cdf0356"));
        assert!(is_uuid("DDCBA2F8-A773-4929-BFBD-52450CDF0356"));
        assert!(!is_uuid("production"));
        assert!(!is_uuid("ddcba2f8-a773-4929-bfbd-52450cdf035")); // 35 chars
        assert!(!is_uuid("ddcba2f8-a773-4929-bfbd-52450cdf035g")); // non-hex
        assert!(!is_uuid("ddcba2f8_a773_4929_bfbd_52450cdf0356")); // separators
    }

    /// Multiplexing degrades to plain connections rather than erroring where
    /// it cannot work: Windows, and socket paths past the unix limit.
    #[test]
    fn mux_gating() {
        let short = std::path::Path::new("/tmp/railway-cm-1-abc.sock");
        if cfg!(windows) {
            assert!(mux_master_opts(short, "10s").is_empty());
            assert!(mux_client_opts(short).is_empty());
        } else {
            assert!(!mux_master_opts(short, "10s").is_empty());
            assert!(!mux_client_opts(short).is_empty());
            let long = std::path::PathBuf::from(format!("/{}/cm.sock", "x".repeat(120)));
            assert!(mux_master_opts(&long, "10s").is_empty());
            assert!(mux_client_opts(&long).is_empty());
        }
    }

    /// The shell option starts nothing and changes nothing: no credential, no
    /// autostart retarget — reconnects keep dropping into whatever agent a
    /// previous launch recorded — and the session is one login shell, with any
    /// prompt or args ignored rather than handed to a harness that isn't there.
    #[test]
    fn a_shell_launch_starts_no_harness_and_retargets_nothing() {
        for (prompt, args) in [
            (None, vec![]),
            (Some("fix the tests"), vec![]),
            (None, vec!["exec".to_string(), "explain this".to_string()]),
        ] {
            let cmd = remote_command(
                Agent::Shell,
                "P; ",
                prompt,
                &args,
                SessionStyle::FullTerminal,
            );
            assert_eq!(cmd, "P; export RAILWAY_CODE_AUTOSTARTED=1; exec bash -l");
        }

        let script = provision_script(Agent::Shell, false, false);
        assert!(
            !script.contains("~/.railway-code-agent"),
            "a shell launch must not retarget reconnects: {script}"
        );
        for seed in [
            "cat > ~/.claude-code-env",
            "cat > ~/.codex/auth.json",
            "cat > ~/.grok/auth.json",
        ] {
            assert!(!script.contains(seed), "{script}");
        }
        // The rest of the provision still runs, and readiness probes the one
        // binary the session needs.
        assert!(script.contains("railway-code agent autostart"));
        assert!(script.contains("if command -v bash"), "{script}");
        assert!(script.contains("AGENT-READY"));
    }

    /// Harness config on an agent VM belongs to express-agent, which reconciles
    /// it on every boot. The CLI used to copy the laptop's
    /// `~/.claude/settings.json` up; it no longer does, and must not drift back
    /// into it — a settings blob carries API keys, `apiKeyHelper`, and
    /// statusline commands that only resolve on the machine that wrote them.
    #[test]
    fn no_provision_step_writes_harness_config() {
        for agent in [
            Agent::Claude,
            Agent::Codex,
            Agent::Grok,
            Agent::Railway,
            Agent::Shell,
        ] {
            for write_credential in [true, false] {
                let script = provision_script(agent, write_credential, false);
                assert!(!script.contains(".claude/settings.json"), "{script}");
                assert!(!script.contains(".claude.json"), "{script}");
                assert!(!script.contains("apiKeyHelper"), "{script}");
            }
        }
        // Onboarding/trust is express-agent's to seed at boot; the CLI must not
        // have quietly reacquired it via a credential seed.
        assert!(!CLAUDE_SEED.contains("hasCompletedOnboarding"));
    }

    /// The launcher decides whether to upload skills from a marker the
    /// provision script prints, so the two must agree on its shape.
    #[test]
    fn provision_script_reports_the_agents_skills_hash() {
        let script = provision_script(Agent::Claude, true, false);
        assert!(script.contains(skills_sync::REMOTE_HASH_MARKER));
        assert!(script.contains(skills_sync::REMOTE_HASH_FILE));
        // The MCP set rides the same connection, by the same dance.
        assert!(script.contains(mcp_sync::REMOTE_HASH_MARKER));
        assert!(script.contains(mcp_sync::REMOTE_HASH_FILE));
        // An agent that has never synced prints an empty value rather than
        // failing the script — the marker parser treats that as "no hash".
        assert!(script.contains("2>/dev/null || true"));
    }

    /// Every generated provision script is valid POSIX shell. `sh -n` parses
    /// without executing, so this catches a broken heredoc, an unbalanced
    /// quote, or a mangled format! escape in ANY variant before a VM does —
    /// including the skills and MCP follow-up scripts.
    #[cfg(unix)]
    #[test]
    fn every_generated_provision_script_parses_as_shell() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut scripts: Vec<(String, String)> = Vec::new();
        for agent in [
            Agent::Codex,
            Agent::Claude,
            Agent::Grok,
            Agent::Railway,
            Agent::Shell,
        ] {
            for write_credential in [true, false] {
                for app_mode in [true, false] {
                    scripts.push((
                        format!("provision {agent:?} cred={write_credential} app={app_mode}"),
                        provision_script(agent, write_credential, app_mode),
                    ));
                }
            }
            scripts.push((
                format!("provision+skills {agent:?}"),
                provision_script_with_skills(agent, Some(42), false, "deadbeef"),
            ));
        }
        scripts.push((
            "skills follow-up".into(),
            skills_sync::provision_script("deadbeef"),
        ));
        scripts.push((
            "mcp follow-up".into(),
            mcp_sync::provision_script("deadbeef"),
        ));

        for (label, script) in scripts {
            let mut child = Command::new("sh")
                .arg("-n")
                .stdin(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(script.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "`{label}` is not valid shell:\n{}\n--- script ---\n{script}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// The TUI's prompt box and `-- exec …` must not collapse into the same
    /// remote command: one keeps you in the session, the other exits with the
    /// agent. Getting this backwards would drop you out of a session you asked
    /// to work in, or hang a script on a shell.
    #[test]
    fn remote_command_shapes() {
        use SessionStyle::FullTerminal;

        let seeded = remote_command(
            Agent::Claude,
            "P; ",
            Some("fix the tests"),
            &[],
            FullTerminal,
        );
        assert!(seeded.contains("claude 'fix the tests';"), "{seeded}");
        assert!(seeded.ends_with("exec bash -l"));
        assert!(!seeded.contains("exec claude"));

        let interactive = remote_command(Agent::Claude, "P; ", None, &[], FullTerminal);
        assert!(interactive.contains("claude;"), "{interactive}");
        assert!(interactive.ends_with("exec bash -l"));

        let scripted = remote_command(
            Agent::Codex,
            "P; ",
            None,
            &["exec".into(), "explain this".into()],
            FullTerminal,
        );
        assert!(
            scripted.contains("exec codex exec 'explain this'"),
            "{scripted}"
        );
        assert!(!scripted.contains("bash -l"));

        // A prompt of only whitespace is not a prompt.
        let blank = remote_command(Agent::Grok, "P; ", Some("   "), &[], FullTerminal);
        assert_eq!(
            blank,
            remote_command(Agent::Grok, "P; ", None, &[], FullTerminal)
        );

        // Railway's TUI runs in-process, or the shared daemon would join
        // every new session onto the directory's live conversation — and
        // steer a seeded prompt into it.
        let railway = remote_command(
            Agent::Railway,
            "P; ",
            Some("fix the tests"),
            &[],
            FullTerminal,
        );
        assert!(
            railway.contains(
                "railway-agent-tui --session \"$RAILWAY_DURABLE_SESSION_NAME\" 'fix the tests';"
            ),
            "{railway}"
        );
        let railway_bare = remote_command(Agent::Railway, "P; ", None, &[], FullTerminal);
        assert!(
            railway_bare.contains("railway-agent-tui --session \"$RAILWAY_DURABLE_SESSION_NAME\";"),
            "{railway_bare}"
        );
        // The exec form's args are the caller's, flags included.
        let railway_exec = remote_command(
            Agent::Railway,
            "P; ",
            None,
            &["--continue".into()],
            FullTerminal,
        );
        assert!(
            railway_exec.contains("exec railway-agent-tui --continue"),
            "{railway_exec}"
        );
        assert!(!railway_exec.contains("--session"), "{railway_exec}");
    }

    /// A pane session must end when the harness does. The shell fallback that
    /// serves a full-terminal caller strands a pane on a bare VM prompt inside
    /// what still looks like the TUI — ctrl-c out of the agent read as the CLI
    /// breaking, with a leftover shell session to reattach to.
    #[test]
    fn a_pane_session_ends_with_the_harness() {
        for prompt in [None, Some("fix the tests")] {
            let pane = remote_command(Agent::Claude, "P; ", prompt, &[], SessionStyle::Pane);
            assert!(!pane.contains("bash -l"), "{pane}");
            // The reset still runs — the pane's emulator swallows it, and a
            // full-screen takeover of the same session needs it.
            assert!(pane.ends_with("\\033[?25h'"), "{pane}");
        }
    }

    /// A prompt is user text arriving on a remote shell's command line; it has
    /// to be quoted, not interpolated.
    #[test]
    fn a_prompt_cannot_break_out_of_its_quoting() {
        let nasty = remote_command(
            Agent::Claude,
            "P; ",
            Some("'; rm -rf / #"),
            &[],
            SessionStyle::FullTerminal,
        );
        assert!(!nasty.contains("; rm -rf / #;"), "{nasty}");
        assert!(
            nasty.contains(r"'\''"),
            "expected shell-escaped quoting: {nasty}"
        );
    }

    fn default_project() -> DefaultProject {
        DefaultProject {
            project_id: "proj_default".into(),
            project_name: "Cloud Agents".into(),
            environment_id: "env_default".into(),
            environment_name: "production".into(),
        }
    }

    /// The order `railway ca` uses, so a launch lands in the same place from
    /// either command.
    #[test]
    fn the_linked_directory_beats_the_configured_default() {
        let linked = || Some(("proj_linked".to_string(), "env_linked".to_string()));

        // Flags win outright — the caller said it.
        let args = LaunchArgs {
            project: Some("proj_flag".into()),
            ..Default::default()
        };
        assert_eq!(
            choose_target(&args, Some(&default_project()), linked()),
            TargetSource::Flags
        );
        // Including `-e` on its own, so the shared resolver can find the project.
        let args = LaunchArgs {
            environment: Some("env_flag".into()),
            ..Default::default()
        };
        assert_eq!(
            choose_target(&args, Some(&default_project()), linked()),
            TargetSource::Flags
        );

        // A linked directory beats a configured default: the link is an
        // explicit statement of "this is the project I'm working in", a
        // default is a person-wide preference chosen once.
        assert_eq!(
            choose_target(&LaunchArgs::default(), Some(&default_project()), linked()),
            TargetSource::Linked("proj_linked".into(), "env_linked".into())
        );

        // With no link, the configured default is still better than a question.
        assert_eq!(
            choose_target(&LaunchArgs::default(), Some(&default_project()), None),
            TargetSource::Configured("proj_default".into(), "env_default".into())
        );
    }

    /// A minimal [`queries::RailwayProject`], built the way the API delivers
    /// one — through serde — because the generated struct tree is not worth
    /// spelling out by hand. `envs` is `(id, deleted)` per environment.
    fn project_lookup(deleted: bool, envs: &[(&str, bool)]) -> queries::RailwayProject {
        let stamp = "2026-01-01T00:00:00Z";
        serde_json::from_value(serde_json::json!({
            "id": "proj_linked",
            "name": "linked",
            "workspaceId": "ws",
            "deletedAt": deleted.then_some(stamp),
            "workspace": { "name": "ws" },
            "buckets": { "edges": [] },
            "environments": { "edges": envs.iter().map(|(id, dead)| serde_json::json!({
                "node": {
                    "id": id,
                    "name": id,
                    "canAccess": true,
                    "deletedAt": dead.then_some(stamp),
                    "unmergedChangesCount": 0,
                }
            })).collect::<Vec<_>>() },
            "services": { "edges": [] },
        }))
        .expect("a RailwayProject deserializes from the fields the query selects")
    }

    /// A stale link is demoted; anything short of a definite server-side
    /// "gone" keeps it, so a transient failure cannot reroute a launch to the
    /// configured default — a different project.
    #[test]
    fn only_a_definitely_dead_link_is_demoted() {
        // The project the link names was deleted outright.
        assert_eq!(
            stale_link_reason(&Err(RailwayError::ProjectNotFound), "env_linked"),
            Some("project that no longer exists")
        );
        // Deleted but still readable — the API sometimes keeps the record.
        assert_eq!(
            stale_link_reason(
                &Ok(project_lookup(true, &[("env_linked", false)])),
                "env_linked"
            ),
            Some("project that was deleted")
        );
        // The project survives but the linked environment does not.
        assert_eq!(
            stale_link_reason(
                &Ok(project_lookup(false, &[("env_other", false)])),
                "env_linked"
            ),
            Some("environment that no longer exists")
        );
        assert_eq!(
            stale_link_reason(
                &Ok(project_lookup(false, &[("env_linked", true)])),
                "env_linked"
            ),
            Some("environment that no longer exists")
        );

        // A live link is kept.
        assert_eq!(
            stale_link_reason(
                &Ok(project_lookup(false, &[("env_linked", false)])),
                "env_linked"
            ),
            None
        );
        // An inconclusive lookup is not evidence of staleness.
        assert_eq!(
            stale_link_reason(
                &Err(RailwayError::GraphQLError("connection reset".into())),
                "env_linked"
            ),
            None
        );
    }

    /// Nothing configured and nothing linked runs the setup flow, so the answer
    /// is remembered instead of being asked again next launch. Only where there
    /// is someone to ask.
    #[test]
    fn nothing_to_go_on_runs_setup_when_there_is_a_terminal() {
        let want = match is_stdout_terminal() {
            true => TargetSource::Setup,
            false => TargetSource::Ask,
        };
        assert_eq!(choose_target(&LaunchArgs::default(), None, None), want);
    }

    /// The TUI resolves its own target and passes it explicitly, which is what
    /// keeps the setup flow — and every other prompt — from ever being drawn
    /// underneath a frame.
    #[test]
    fn a_tui_launch_always_targets_by_flag() {
        let args = LaunchArgs::for_target(
            "proj_1".into(),
            "env_prod".into(),
            "claude",
            false,
            None,
            None,
        );
        assert!(args.project.is_some() && args.environment.is_some());
        assert_eq!(
            choose_target(&args, None, None),
            TargetSource::Flags,
            "a TUI launch must never reach a prompt"
        );
    }

    #[test]
    fn launch_args_bareness_and_targeting() {
        assert!(LaunchArgs::default().is_bare());

        let mut flagged = LaunchArgs::default();
        flagged.set_harness("codex");
        assert!(
            !flagged.is_bare(),
            "a harness flag is not a bare invocation"
        );

        let mut railway = LaunchArgs::default();
        railway.set_harness("railway");
        assert!(!railway.is_bare());

        let mut shell = LaunchArgs::default();
        shell.set_harness("shell");
        assert!(shell.shell, "the shell choice must survive the mapping");
        assert!(!shell.is_bare());
        shell.set_harness("claude");
        assert!(!shell.shell, "picking a harness clears it");

        let targeted = LaunchArgs::for_target(
            "proj_1".into(),
            "env_1".into(),
            "grok",
            true,
            Some("do the thing".into()),
            None,
        );
        assert!(!targeted.is_bare());
        assert_eq!(targeted.project.as_deref(), Some("proj_1"));
        assert_eq!(targeted.environment.as_deref(), Some("env_1"));
        assert!(targeted.new);
        assert_eq!(targeted.initial_prompt.as_deref(), Some("do the thing"));

        // An explicit agent is carried through: without it the pipeline infers
        // one from the stored pointer, which is how a "new session on this
        // agent" request became a second VM.
        let pinned = LaunchArgs::for_target(
            "proj_1".into(),
            "env_1".into(),
            "claude",
            false,
            None,
            Some("ca_7".into()),
        );
        assert_eq!(pinned.agent_id.as_deref(), Some("ca_7"));
        assert!(!pinned.new, "pinning an agent must not also create one");
        // Exactly one harness — two would hit the "pick one" bail.
        assert_eq!(
            [targeted.claude, targeted.codex, targeted.grok]
                .iter()
                .filter(|x| **x)
                .count(),
            1
        );
    }

    /// A script travels through a format string and an argv to reach a shell,
    /// so it may contain nothing a C string cannot: a stray NUL made ssh refuse
    /// the command outright, and the session survived.
    #[test]
    fn the_kill_script_is_plain_text() {
        let script = kill_script("claude-3s9r89");
        let bad: Vec<char> = script
            .chars()
            .filter(|c| c.is_control() && *c != '\n')
            .collect();
        assert!(bad.is_empty(), "control characters in the script: {bad:?}");
        assert!(!script.contains('\0'));
        assert!(script.contains("claude-3s9r89"));
    }

    /// Identify the session by the environment vm-init stamps on its
    /// processes, never by the command line — every session on an agent shares
    /// the same launch line, so matching on it would kill all of them.
    #[test]
    fn the_kill_script_matches_on_the_environment() {
        let script = kill_script("claude-3s9r89");
        assert!(script.contains("RAILWAY_DURABLE_SESSION_NAME=claude-3s9r89"));
        assert!(script.contains("/environ"));
        assert!(
            !script.contains("pkill"),
            "pkill -f would take the whole box"
        );
        assert!(
            script.contains("kill -TERM"),
            "TERM lets a harness save first"
        );
        assert!(
            script.contains("KILLED:"),
            "the caller counts what it ended"
        );
    }

    #[test]
    fn agent_slugs_round_trip() {
        for agent in [Agent::Claude, Agent::Codex, Agent::Grok] {
            assert_eq!(agent.slug(), agent.name());
            assert_eq!(Agent::from_slug(agent.name()), Some(agent));
        }
        // Railway is the one exception: the slug is "railway", not the
        // interactive binary's own name — which is what session prefixes,
        // launch messages, and telemetry should read.
        assert_eq!(Agent::from_slug("railway"), Some(Agent::Railway));
        assert_eq!(Agent::Railway.slug(), "railway");
        assert_eq!(Agent::Railway.name(), "railway-agent-tui");
        // Shell is the other: the slug is the option's name, and what the
        // session runs (and the readiness probe checks) is bash.
        assert_eq!(Agent::from_slug("shell"), Some(Agent::Shell));
        assert_eq!(Agent::Shell.slug(), "shell");
        assert_eq!(Agent::Shell.name(), "bash");
        assert!(Agent::from_slug("droid").is_none());
        assert!(Agent::from_slug("").is_none());
    }

    // The cached token is a year-long credential to the user's Anthropic
    // account. It must never exist at the umask default, even momentarily —
    // hence create-with-mode rather than write-then-chmod.
    #[cfg(unix)]
    #[test]
    fn cached_token_is_created_0600_regardless_of_umask() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("railway-tok-{}", std::process::id()));
        let path = dir.join("nested").join("claude-code-token");
        let _ = std::fs::remove_dir_all(&dir);

        // Deliberately does NOT touch the process umask: mutating a global in a
        // parallel test suite is its own bug. The assertion stands on its own —
        // under the usual 0o022 umask a plain `fs::write` yields 0644, so an
        // exact 0600 here can only come from create-with-mode. It also proves
        // the parent directories get created.
        write_token_0600(&path, "sk-ant-oat01-abc");

        let mode = std::fs::metadata(&path)
            .expect("written")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "cached token was {mode:o}, not 0600");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            "sk-ant-oat01-abc"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_token_validation_rejects_shell_specials() {
        assert!(validate_claude_token("sk-ant-oat01-abc_DEF-123").is_ok());
        for bad in ["has space", "quote'", "semi;colon", "dollar$var", "tick`"] {
            assert!(validate_claude_token(bad).is_err(), "accepted: {bad}");
        }
    }

    const FAKE_A: &str =
        "sk-ant-oat01-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const FAKE_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// Mirrors a real `script(1)` recording of `claude setup-token` v2.1.207:
    /// the token is amber-colored and soft-wrapped at pty width, with the
    /// continuation on the next line behind a cursor-down + one leading
    /// space, followed by prose that must not be swallowed into the token.
    fn fake_recording() -> String {
        format!(
            "\x1b[?25l\x1b[<u\x1b[>1u\x1b[38;2;78;186;101m Long-lived authentication token created successfully!\r\x1b[1B\x1b[39m\x1b[K\r\x1b[1B Your OAuth token (valid for 1 year):\x1b[K\r\x1b[1B\x1b[K\r\x1b[1B \x1b[38;2;255;193;7m{FAKE_A}\r\x1b[1B\x1b[39m \x1b[38;2;255;193;7m{FAKE_B}\r\x1b[1C\x1b[2B\x1b[38;2;153;153;153mStore this token securely. You won't be able to see it again.\r\x1b[1C\x1b[1B\x1b[39m\x1b[K\r\r\n\x1b]8;id=1;https://example.com\x07link\x1b]8;;\x07\r\n"
        )
    }

    #[test]
    fn extracts_wrapped_token_from_recording() {
        let tok = extract_claude_token(&fake_recording()).expect("token");
        assert_eq!(tok, format!("{FAKE_A}{FAKE_B}"));
    }

    #[test]
    fn extracts_unwrapped_token() {
        let raw = format!("junk\r\n \x1b[33m{FAKE_A}\x1b[39m\r\nStore this token securely.\r\n");
        assert_eq!(extract_claude_token(&raw).as_deref(), Some(FAKE_A));
    }

    #[test]
    fn rejects_fragments_and_noise() {
        // Too short to be a real token.
        assert_eq!(extract_claude_token("sk-ant-oat01-tooshort\r\n"), None);
        // No token at all (OSC hyperlink URLs must not confuse the stripper).
        assert_eq!(
            extract_claude_token("\x1b]8;;https://claude.com/oauth?x=1\x07sign in\x1b]8;;\x07\r\n"),
            None
        );
    }

    #[test]
    fn terminal_reset_printf_has_no_raw_escapes() {
        let printf = terminal_reset_printf();
        assert!(!printf.contains('\x1b'));
        assert!(printf.starts_with("printf '") && printf.ends_with('\''));
    }
}
