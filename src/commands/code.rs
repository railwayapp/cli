use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use colored::Colorize;
use is_terminal::IsTerminal;

use crate::client::{GQLClient, post_graphql};
use crate::commands::cloud_agent::prefs::{AgentPrefs, DefaultProject};
use crate::commands::cloud_agent::skills_sync;
use crate::commands::sandbox::{resolve_project_and_env, variables_to_input};
use crate::commands::ssh::tel as ssh_tel;
use crate::commands::ssh::{
    ensure_ssh_key_quiet, run_native_ssh_captured, run_native_ssh_with_opts,
};
use crate::config::Configs;
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
// nothing eventually reaps one. Disconnecting therefore SLEEPS the agent —
// disk and work survive, compute stops billing — and the next run wakes it.
// ---------------------------------------------------------------------------

/// `railway code` is the launcher on its own: the same flags and the same
/// preferences as `railway ca`, minus the TUI. Kept as a distinct command
/// rather than an alias because the two now differ in exactly one way — one
/// browses first and one does not — and that is the reason to type either.
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
    launch(args).await
}

/// Launch a coding agent on a Railway cloud agent VM
//
// `Default` is derived so the TUI can build a launch without going through
// clap: every field is an Option/Vec/bool, so the derive produces exactly the
// "nothing was passed" state clap would. Kept out of the doc comment — clap
// renders those as `long_about` and it would show up in `--help`.
#[derive(Parser, Default)]
#[clap(
    after_help = "Examples:\n\n  railway ca                        # launch your configured default\n  railway ca setup                  # choose the default agent and skills\n  railway code --codex              # agent VM + your local Codex sign-in\n  railway code --claude             # agent VM + your Claude setup-token\n  railway code --grok               # agent VM + your local Grok sign-in\n  railway code --railway            # agent VM + Railway's own agent, no sign-in needed\n  railway code --codex --new        # force a fresh agent instead of reusing\n  railway code --codex --new --variable DB_URL=postgres.DATABASE_URL\n  railway code --codex --new --env-file .env\n  railway code --codex -- exec \"explain this codebase\"\n\nWith no agent flag, the default saved by `railway ca setup` is used\n(RAILWAY_CA_AGENT overrides it for one run).\n\nAgents persist between runs: disconnecting sleeps yours, and the next\n`railway code` wakes it with your work still on disk. `--keep-awake` leaves it\nrunning; `railway code --rm` destroys it.\n\nClaude auth is minted once (`claude setup-token`), cached locally, and reused —\nincluding the copy already on a reused agent. `--refresh-auth` re-mints it.\n\nCarrying a sign-in from this machine is a convenience, not a requirement: with\nnothing local to copy or mint from, the agent still starts and the harness asks\nyou to sign in there.\n\nNote: requires the CLOUD_AGENTS feature to be enabled."
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

    /// Leave the agent running on disconnect instead of putting it to sleep.
    /// A running agent keeps billing for compute
    #[clap(long)]
    keep_awake: bool,

    /// Destroy this environment's agent and exit. Its disk goes with it.
    /// Superseded by `railway ca delete`, which can name any agent and asks
    /// before it destroys one
    #[clap(long)]
    rm: bool,

    /// Re-mint the Claude credential even if the agent already has a working
    /// one. Use after revoking a token, or when auth fails on an existing agent
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
    }

    /// Force one harness, overriding preferences — how the TUI passes the
    /// choice made in its prompt footer.
    pub fn set_harness(&mut self, slug: &str) {
        self.claude = slug == "claude";
        self.codex = slug == "codex";
        self.grok = slug == "grok";
        self.railway = slug == "railway";
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
        let mut args = Self {
            project: Some(project_id),
            environment: Some(environment_id),
            new: force_new,
            initial_prompt: prompt,
            agent_id,
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
#[derive(Clone, Copy, PartialEq, Debug)]
enum Agent {
    Codex,
    Claude,
    Grok,
    Railway,
}

impl Agent {
    /// The remote binary name (also what's autostarted on reconnect). The one
    /// exception to "identical to the slug" below: the interactive frontend
    /// binary is `railway-agent-tui`, not `railway-agent` (that name is the
    /// headless `run`/`serve` CLI it drives).
    fn name(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::Claude => "claude",
            Agent::Grok => "grok",
            Agent::Railway => "railway-agent-tui",
        }
    }

    /// The slug persisted in `agent-prefs.json` and accepted by
    /// `RAILWAY_CA_AGENT`. Identical to the remote binary name for every agent
    /// except Railway's own — "railway" reads better than "railway-agent-tui"
    /// in a flag or a config file, and there is only the one harness it could
    /// mean.
    fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "grok" => Some(Agent::Grok),
            "railway" => Some(Agent::Railway),
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
            Agent::Railway => "",
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
            Agent::Claude | Agent::Railway => None,
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
const COMMON_SEED: &str = r#"grep -q "^COLORTERM=" /etc/environment 2>/dev/null || echo "COLORTERM=truecolor" >> /etc/environment 2>/dev/null || true
if ! grep -q "railway-code agent autostart" ~/.profile 2>/dev/null; then
cat >> ~/.profile <<'PROFEOF'

# railway-code agent autostart (connecting drops into the agent; exit it for a shell)
if [ -z "$RAILWAY_CODE_AUTOSTARTED" ] && [ -t 1 ] && [ -s "$HOME/.railway-code-agent" ]; then
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
fn provision_script(agent: Agent, write_credential: bool) -> String {
    let seed = if write_credential {
        agent.credential_seed()
    } else {
        "true"
    };
    let name = agent.name();
    let hash_marker = skills_sync::REMOTE_HASH_MARKER;
    let hash_file = skills_sync::REMOTE_HASH_FILE;
    format!(
        r#"umask 077
{HARNESS_PATH}
{seed}
{COMMON_SEED}
echo {name} > ~/.railway-code-agent
printf '{hash_marker}%s\n' "$(cat "{hash_file}" 2>/dev/null || true)"
if command -v {name} >/dev/null 2>&1; then echo AGENT-READY; else echo AGENT-MISSING; fi"#
    )
}

/// The command the launch session runs on the VM. Three shapes, and the
/// difference between them is whether you are left in a session afterwards:
///
/// - a seeded prompt starts the agent on that task and keeps the session;
/// - a bare launch starts the agent interactively and keeps the session;
/// - `-- args` execs the agent and exits with it, so a pipeline doesn't hang
///   waiting on a shell nobody is typing into.
///
/// Neither interactive form uses `exec`: quitting the agent lands in a shell on
/// the VM, matching the `~/.profile` autostart. `RAILWAY_CODE_AUTOSTARTED`
/// stops that autostart relaunching the agent on top of the user, and the reset
/// scrubs terminal state a TUI can leave behind on an unclean exit.
fn remote_command(
    agent: Agent,
    env_prefix: &str,
    initial_prompt: Option<&str>,
    agent_args: &[String],
) -> String {
    let name = agent.name();
    match initial_prompt.map(str::trim).filter(|p| !p.is_empty()) {
        Some(prompt) => format!(
            "{env_prefix}export RAILWAY_CODE_AUTOSTARTED=1; {name} {}; {}; exec bash -l",
            shell_join(std::slice::from_ref(&prompt.to_string())),
            terminal_reset_printf()
        ),
        None if agent_args.is_empty() => format!(
            "{env_prefix}export RAILWAY_CODE_AUTOSTARTED=1; {name}; {}; exec bash -l",
            terminal_reset_printf()
        ),
        None => format!("{env_prefix}exec {name} {}", shell_join(agent_args)),
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
/// Deliberately NOT multiplexed. ControlMaster was here to make one host-key
/// decision per run when the fleet answered with per-instance keys, which it no
/// longer does. It also had a failure mode worse than the problem it solved:
/// sleeping an agent on disconnect kills the master's TCP connection while the
/// socket file lives on for ControlPersist, so the next run rides a dead master
/// and dies with exit 255 and no message from either stream — invisible, and
/// immune to retries because waiting cannot revive it.
#[derive(Clone)]
struct RelaySsh {
    opts: Vec<String>,
    known_hosts: std::path::PathBuf,
    /// known-hosts pattern for ssh-keygen -R: `host` or `[host]:port`.
    host_pattern: String,
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
/// runners. Cleared by `railway logout`.
fn claude_token_cache_path() -> Option<std::path::PathBuf> {
    Some(dirs::home_dir()?.join(".railway").join("claude-code-token"))
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

/// Forget the cached token. Called by `railway logout`.
pub fn clear_claude_token_cache() {
    if let Some(path) = claude_token_cache_path() {
        let _ = std::fs::remove_file(path);
    }
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
fn claude_credentials_cheap() -> Result<PendingAuth> {
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
    if let Some(tok) = cached_claude_token() {
        return Ok(PendingAuth::Ready {
            line: format!("CLAUDE_CODE_OAUTH_TOKEN={tok}\n").into_bytes(),
            source: "cached setup-token".to_string(),
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
    // manual paste prompt below.
    let spinner = create_shimmer_spinner(
        "Minting a Claude token — approve the browser prompt if one appears",
    );
    match run_claude_setup_token() {
        Ok(tok) => {
            spinner.finish_and_clear();
            validate_claude_token(&tok)?;
            cache_claude_token(&tok);
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

/// One cloud agent, reduced to what this command steers on.
#[derive(Clone)]
struct CodeAgent {
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

/// Poll until the agent is RUNNING. A terminal state (CRASHED/FAILED/DELETING)
/// is reported immediately instead of burning the whole timeout on a box that
/// will never come up.
async fn wait_until_running(
    client: &reqwest::Client,
    backboard: &str,
    environment_id: &str,
    id: &str,
) -> Result<CodeAgent> {
    use queries::cloud_agent::CloudAgentStatus as S;
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    loop {
        let agent = fetch_agent(client, backboard, environment_id, id)
            .await?
            .ok_or_else(|| anyhow!("Agent {id} disappeared while starting."))?;
        match agent.status {
            S::RUNNING => return Ok(agent),
            S::STARTING | S::SLEEPING => {}
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
        if std::time::Instant::now() >= deadline {
            bail!(
                "Agent {} did not reach RUNNING within {}s (last state: {:?}).",
                agent.name,
                READY_TIMEOUT.as_secs(),
                agent.status
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
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
) -> Result<Option<CodeAgent>> {
    use queries::cloud_agent::CloudAgentStatus as S;

    match agent.status {
        S::RUNNING => {
            progress.note(&format!(
                "Using agent {} (--new for a fresh one)",
                agent.name
            ));
            Ok(Some(agent))
        }
        // STARTING means a previous run is still booting it, so a re-run seconds
        // after a ctrl-c waits rather than minting a duplicate. SLEEPING is the
        // resting state this command leaves behind.
        S::SLEEPING | S::STARTING => {
            progress.step(&format!("Waking agent {}", agent.name));
            if agent.status == S::SLEEPING
                && let Err(e) = post_graphql::<mutations::CloudAgentWake, _>(
                    client,
                    backboard,
                    mutations::cloud_agent_wake::Variables {
                        id: agent.id.clone(),
                    },
                )
                .await
            {
                return Err(e.into());
            }
            let running = wait_until_running(client, backboard, environment_id, &agent.id).await?;
            progress.note(&format!(
                "Woke agent {} — your work is on its disk",
                running.name
            ));
            Ok(Some(running))
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
/// The configured default project is the answer to "where do agents go", so it
/// beats the linked directory — a link is about deploys, and running
/// `railway code` inside some service's checkout should not put an agent there.
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

    match choose_target(args, prefs.default_project.as_ref(), linked) {
        // Either flag means the caller is targeting deliberately; hand both to
        // the shared resolver so `-p` alone still finds an environment.
        TargetSource::Flags => {
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
    if let Some(default) = configured {
        return TargetSource::Configured(
            default.project_id.clone(),
            default.environment_id.clone(),
        );
    }
    // A linked directory is a worse answer than a configured default but a
    // better one than a question, and plenty of people rely on it.
    if let Some((project_id, environment_id)) = linked {
        return TargetSource::Linked(project_id, environment_id);
    }
    // Only offer setup when there is someone to answer it. The TUI always
    // passes an explicit target, so this cannot fire underneath a frame, and a
    // script gets the picker's error rather than a prompt it can never answer.
    match is_stdout_terminal() {
        true => TargetSource::Setup,
        false => TargetSource::Ask,
    }
}

async fn resolve_agent(
    configs: &mut Configs,
    client: &reqwest::Client,
    args: &LaunchArgs,
    environment_id: &str,
    progress: &dyn Progress,
) -> Result<(CodeAgent, bool)> {
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
        if let Some(ready) =
            ready_existing_agent(client, &backboard, environment_id, agent, progress).await?
        {
            warn_ignored_variables(args);
            configs.set_code_agent(environment_id, &ready.id);
            configs.write()?;
            return Ok((ready, false));
        }
        configs.remove_code_agent(environment_id);
    }

    let variables = variables_to_input(&args.env_files, &args.variables)?
        .map(serde_json::to_value)
        .transpose()?;
    progress.step("Creating a cloud agent");
    let created = match post_graphql::<mutations::CloudAgentCreate, _>(
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
    .await
    {
        Ok(res) => res.cloud_agent_create,
        Err(e) => return Err(e.into()),
    };

    // Remembered before the box is up: a create that succeeds and then times out
    // waiting has still spent a VM, and the pointer is the only handle the next
    // run has to it.
    configs.set_code_agent(environment_id, &created.id);
    configs.write()?;

    match wait_until_running(client, &backboard, environment_id, &created.id).await {
        Ok(running) => {
            progress.note(&format!("Created agent {}", running.name));
            Ok((running, true))
        }
        Err(e) => {
            progress.finish();
            Err(e)
        }
    }
}

/// `--variable`/`--env-file` only reach the VM spec at create time, so say so
/// rather than silently dropping them on a reuse.
fn warn_ignored_variables(args: &LaunchArgs) {
    use colored::Colorize;
    if !args.variables.is_empty() || !args.env_files.is_empty() {
        eprintln!(
            "{}",
            "Note: --variable/--env-file only apply when an agent is created — reusing this environment's. Add --new to create with these variables."
                .yellow()
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
    Ok(resolve_agent_choice(&LaunchArgs::default(), &mut prefs, &home)?.name())
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
                    "{AGENT_ENV_VAR}={slug} is not a known agent (claude, codex, grok, or railway)."
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

    // Before anything is resolved, minted or provisioned: without the flag the
    // create at the end of all that is refused, and the work up to it — a
    // credential, possibly a browser round-trip — is spent for nothing.
    {
        let configs = Configs::new()?;
        let client = GQLClient::new_authorized(&configs)?;
        crate::commands::cloud_agent::access::ensure_enabled(&client, &configs).await?;
    }

    eprintln!(
        "{}",
        "Warning: Railway cloud agents are experimental and APIs may change or break during testing."
            .yellow()
    );

    let progress = CliProgress::default();
    let prepared = prepare(&args, &progress).await?;
    progress.finish();

    println!("Launching {}…", prepared.harness);
    let exit_code = run_session(&prepared)?;

    // Belt-and-suspenders for the remote reset: when the connection drops
    // mid-TUI the remote printf never reaches us, so scrub locally too before
    // printing anything. No-op on a clean terminal.
    if std::io::stdout().is_terminal() {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(TERMINAL_RESET.as_bytes());
        let _ = out.flush();
    }

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    if args.keep_awake {
        println!(
            "\nDisconnected — agent {} is still running (--keep-awake).",
            prepared.agent_name.cyan()
        );
    } else {
        let progress = CliProgress::default();
        if let Err(e) = sleep_agent(&client, &configs, &prepared, &progress).await {
            progress.finish();
            eprintln!(
                "{}",
                format!(
                    "Agent {} is still running and billing compute. Sleep it from the dashboard, or `railway code --rm` to destroy it. ({e})",
                    prepared.agent_name
                )
                .yellow()
            );
        }
        progress.finish();
    }

    if prepared.created {
        println!("Agents persist between runs — this one is yours until you --rm it.");
    }
    println!("Get back in:");
    println!(
        "  railway code --{}   # wakes it and drops back into {}",
        prepared.harness, prepared.harness
    );
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

/// Put the agent back to sleep. Agents have no idle timeout, so nothing else
/// ever will: leaving one running bills compute until the user remembers it,
/// and sleeping keeps the disk so the next run wakes into the same work.
pub async fn sleep_agent(
    client: &reqwest::Client,
    configs: &Configs,
    prepared: &Prepared,
    progress: &dyn Progress,
) -> Result<()> {
    progress.step("Sleeping the agent");
    crate::controllers::cloud_agent::sleep(
        client,
        &configs.get_backboard(),
        &prepared.environment_id,
        &prepared.agent_id,
    )
    .await?;
    progress.finish();
    Ok(())
}

/// Everything between "the user asked" and "there is a session to open":
/// credential, skills, the agent itself, and provisioning it.
///
/// Split out of [`launch`] so a TUI can drive the same pipeline and render the
/// steps itself. The one thing that cannot happen here is an interactive Claude
/// mint — see [`ensure_claude_credential_cached`], which a TUI caller runs
/// before it takes the screen.
pub async fn prepare(args: &LaunchArgs, progress: &dyn Progress) -> Result<Prepared> {
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

    let result = prepare_inner(args, progress, agent, prefs, &home).await;
    crate::commands::cloud_agent::telemetry::track_launch_outcome(
        agent.name(),
        result.as_ref().ok().map(|p| p.created),
        start.elapsed(),
        result.as_ref().err().map(|e| format!("{e:#}")).as_deref(),
    )
    .await;
    result
}

async fn prepare_inner(
    args: &LaunchArgs,
    progress: &dyn Progress,
    agent: Agent,
    mut prefs: AgentPrefs,
    home: &Path,
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
            ssh_tel::track_for(
                "cloud_agent_launch",
                "credential",
                local_signin(agent, home),
            )
            .await?
        }
        Agent::Claude => {
            ssh_tel::track_for(
                "cloud_agent_launch",
                "credential",
                claude_credentials_cheap(),
            )
            .await?
        }
        // Nothing to read or mint — the VM already carries its own.
        Agent::Railway => PendingAuth::None,
    };
    match pending {
        PendingAuth::Ready { ref source, .. } => progress.note(&format!(
            "Using your {} credential ({source}) on the agent",
            agent.display()
        )),
        // Said up front, before the VM: the sign-in is the first thing waiting
        // on the other end, and finding that out on arrival reads as a bug.
        PendingAuth::SignInOnAgent { ref note } => progress.note(note),
        PendingAuth::None => {
            progress.note("Using the agent's own integrated Railway credentials")
        }
        PendingAuth::MintClaude => {}
    }
    // Pack the user's skills before spending a VM: a skills directory that has
    // grown into something unshippable should fail here, not after a create.
    // The upload itself is decided later, against the hash the agent reports.
    let packed_skills = ssh_tel::track_for(
        "cloud_agent_launch",
        "skills_pack",
        skills_sync::pack(&prefs, home),
    )
    .await?;
    if let Some(packed) = &packed_skills {
        progress.note(&format!(
            "Including {} of your skills ({})",
            packed.names.len(),
            packed.source_dir.display()
        ));
    }

    // --- Resolve where the agent lives.
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    // The project is no longer carried on `Prepared` — its only reader was the
    // launcher's exit hint, which now names the agent instead.
    let (_project_id, environment_id) = ssh_tel::track_for(
        "cloud_agent_launch",
        "resolve_target",
        resolve_target(&mut configs, &client, args, &mut prefs, home).await,
    )
    .await?;

    let (cloud_agent, created) = ssh_tel::track_for(
        "cloud_agent_launch",
        "resolve_agent",
        resolve_agent(&mut configs, &client, args, &environment_id, progress).await,
    )
    .await?;
    configs.set_code_agent(&environment_id, &cloud_agent.id);
    configs.write()?;

    let identity = ssh_tel::track_for(
        "cloud_agent_launch",
        "ssh_key",
        ensure_ssh_key_quiet(&client, &configs).await,
    )
    .await?;
    // The relay's cloud-agent grammar; by id rather than name because names are
    // not unique within an environment.
    let target = format!("agent:{environment_id}:{}", cloud_agent.id);

    let relay = ssh_tel::track_for("cloud_agent_launch", "relay", relay_ssh()).await?;

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
                let probe = ssh_tel::track_for(
                    "cloud_agent_launch",
                    "claude_probe",
                    ssh_plumbing(
                        &target,
                        CLAUDE_CREDENTIAL_PROBE,
                        identity.as_deref(),
                        None,
                        &relay,
                    ),
                )
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
                match ssh_tel::track_for(
                    "cloud_agent_launch",
                    "claude_mint",
                    mint_claude_credentials(),
                )
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
    let provision = {
        let target = target.clone();
        let identity = identity.clone();
        let relay = relay.clone();
        let skills_note = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let notes = skills_note.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<()> {
            let push = |line: String| {
                if let Ok(mut n) = notes.lock() {
                    n.push(line);
                }
            };
            let out = ssh_plumbing(
                &target,
                &provision_script(agent, auth.is_some()),
                identity.as_deref(),
                auth.as_ref().map(|(line, _)| line.as_slice()),
                &relay,
            )?;
            let out = String::from_utf8_lossy(&out);
            if out.contains("AGENT-READY") {
                // ok
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
                    )?;
                    let out = String::from_utf8_lossy(&out);
                    // Never fatal: the agent is fully usable without them, and
                    // losing a session over a skills copy would be a worse
                    // trade than launching without one. Name the marker so a
                    // report says which step gave up.
                    if !out.contains("SKILLS-OK") {
                        let reason = if out.contains("SKILLS-NO-TAR") {
                            "the agent has no `tar`"
                        } else if out.contains("SKILLS-EXTRACT-FAILED") {
                            "the transfer did not unpack"
                        } else {
                            "the sync did not report success"
                        };
                        push(format!(
                            "Couldn't sync your skills onto the agent ({reason}); continuing without them."
                        ));
                    }
                }
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
    ssh_tel::track_for("cloud_agent_launch", "provision", provision).await?;

    let env_prefix = format!(
        "{HARNESS_PATH}; [ -f ~/.gh-token ] && export GH_TOKEN=\"$(cat ~/.gh-token)\"; [ -f ~/.claude-code-env ] && set -a && . ~/.claude-code-env && set +a; "
    );
    let remote_cmd = remote_command(
        agent,
        &env_prefix,
        args.initial_prompt.as_deref(),
        &args.agent_args,
    );

    Ok(Prepared {
        remote_cmd,
        ssh_target: target,
        identity,
        relay_opts: relay.opts,
        agent_id: cloud_agent.id,
        agent_name: cloud_agent.name,
        environment_id,
        harness: agent.name(),
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
        claude_credentials_cheap(),
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
    if let PendingAuth::MintClaude = claude_credentials_cheap()? {
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

    fn note_of(pending: PendingAuth) -> String {
        match pending {
            PendingAuth::SignInOnAgent { note } => note,
            PendingAuth::Ready { source, .. } => panic!("expected a fallback, got {source}"),
            PendingAuth::MintClaude => panic!("expected a fallback, got a mint"),
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
            let script = provision_script(agent, false);
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
        let codex = provision_script(Agent::Codex, true);
        assert!(codex.contains("cat > ~/.codex/auth.json"));
        assert!(codex.contains("echo codex > ~/.railway-code-agent"));

        let claude = provision_script(Agent::Claude, true);
        assert!(claude.contains("cat > ~/.claude-code-env"));
        assert!(claude.contains("echo claude > ~/.railway-code-agent"));

        let grok = provision_script(Agent::Grok, true);
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

    // Reusing an agent's existing credential must omit the seed, not run it with
    // empty stdin — `cat > ~/.claude-code-env` would truncate the file we chose
    // to keep.
    #[test]
    fn provision_script_omits_the_seed_when_reusing_a_credential() {
        let claude = provision_script(Agent::Claude, false);
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
            assert!(provision_script(agent, true).contains(seed));
            assert!(!provision_script(agent, false).contains(seed));
        }
    }

    /// Railway's own harness never has a credential to push — `prepare_inner`
    /// always resolves it to `PendingAuth::None`, so `write_credential` is
    /// always false — but it still runs the reconnect/PATH seeds and reports
    /// its own binary name like every other harness.
    #[test]
    fn railway_needs_no_credential_seed() {
        let script = provision_script(Agent::Railway, false);
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

    /// Harness config on an agent VM belongs to express-agent, which reconciles
    /// it on every boot. The CLI used to copy the laptop's
    /// `~/.claude/settings.json` up; it no longer does, and must not drift back
    /// into it — a settings blob carries API keys, `apiKeyHelper`, and
    /// statusline commands that only resolve on the machine that wrote them.
    #[test]
    fn no_provision_step_writes_harness_config() {
        for agent in [Agent::Claude, Agent::Codex, Agent::Grok, Agent::Railway] {
            for write_credential in [true, false] {
                let script = provision_script(agent, write_credential);
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
        let script = provision_script(Agent::Claude, true);
        assert!(script.contains(skills_sync::REMOTE_HASH_MARKER));
        assert!(script.contains(skills_sync::REMOTE_HASH_FILE));
        // An agent that has never synced prints an empty value rather than
        // failing the script — the marker parser treats that as "no hash".
        assert!(script.contains("2>/dev/null || true"));
    }

    /// The TUI's prompt box and `-- exec …` must not collapse into the same
    /// remote command: one keeps you in the session, the other exits with the
    /// agent. Getting this backwards would drop you out of a session you asked
    /// to work in, or hang a script on a shell.
    #[test]
    fn remote_command_shapes() {
        let seeded = remote_command(Agent::Claude, "P; ", Some("fix the tests"), &[]);
        assert!(seeded.contains("claude 'fix the tests';"), "{seeded}");
        assert!(seeded.ends_with("exec bash -l"));
        assert!(!seeded.contains("exec claude"));

        let interactive = remote_command(Agent::Claude, "P; ", None, &[]);
        assert!(interactive.contains("claude;"), "{interactive}");
        assert!(interactive.ends_with("exec bash -l"));

        let scripted = remote_command(
            Agent::Codex,
            "P; ",
            None,
            &["exec".into(), "explain this".into()],
        );
        assert!(
            scripted.contains("exec codex exec 'explain this'"),
            "{scripted}"
        );
        assert!(!scripted.contains("bash -l"));

        // A prompt of only whitespace is not a prompt.
        let blank = remote_command(Agent::Grok, "P; ", Some("   "), &[]);
        assert_eq!(blank, remote_command(Agent::Grok, "P; ", None, &[]));
    }

    /// A prompt is user text arriving on a remote shell's command line; it has
    /// to be quoted, not interpolated.
    #[test]
    fn a_prompt_cannot_break_out_of_its_quoting() {
        let nasty = remote_command(Agent::Claude, "P; ", Some("'; rm -rf / #"), &[]);
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
    fn the_configured_default_beats_the_linked_directory() {
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

        // A configured default beats a linked directory: the default answers
        // "where do agents go", a link answers "what do I deploy".
        assert_eq!(
            choose_target(&LaunchArgs::default(), Some(&default_project()), linked()),
            TargetSource::Configured("proj_default".into(), "env_default".into())
        );

        // With no default, the link is still better than a question.
        assert_eq!(
            choose_target(&LaunchArgs::default(), None, linked()),
            TargetSource::Linked("proj_linked".into(), "env_linked".into())
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
            assert_eq!(Agent::from_slug(agent.name()), Some(agent));
        }
        // Railway is the one exception: the slug is "railway", not the
        // interactive binary's own name.
        assert_eq!(Agent::from_slug("railway"), Some(Agent::Railway));
        assert_eq!(Agent::Railway.name(), "railway-agent-tui");
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
