use anyhow::{Result, anyhow, bail};
use clap::Parser;
use is_terminal::IsTerminal;

use crate::client::{GQLClient, post_graphql};
use crate::commands::sandbox::{resolve_project_and_env, variables_to_input};
use crate::commands::ssh::{
    ensure_ssh_key_quiet, run_native_ssh_captured, run_native_ssh_with_opts,
};
use crate::config::Configs;
use crate::gql::{mutations, queries};
use crate::util::progress::{create_shimmer_spinner, fail_spinner};
use crate::util::shell::shell_join;

// ---------------------------------------------------------------------------
// `railway code --codex` / `railway code --claude` / `railway code --grok` —
// launch a coding agent on a Railway cloud agent VM, on the user's own plan.
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
// machines can't safely share. Every credential is announced to the user, read
// client-side, and rides ssh stdin into a 0600 file on the VM: deliberately
// NOT a create-time variable, so it never appears in an argv, a Railway
// variable, the VM spec, an image, or server-side config. That also means a
// reused agent gets its credential refreshed the same way a fresh one does.
// Nothing is stored locally by this command.
//
// Lifecycle: agents are durable and have no idle timeout, so unlike a sandbox
// nothing eventually reaps one. Disconnecting therefore SLEEPS the agent —
// disk and work survive, compute stops billing — and the next run wakes it.
// ---------------------------------------------------------------------------

/// Launch a coding agent on a Railway cloud agent VM
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway code --codex              # agent VM + your local Codex sign-in\n  railway code --claude             # agent VM + your Claude setup-token\n  railway code --grok               # agent VM + your local Grok sign-in\n  railway code --codex --new        # force a fresh agent instead of reusing\n  railway code --claude --gh        # also inject your GitHub auth (gh auth token)\n  railway code --codex --new --variable DB_URL=postgres.DATABASE_URL\n  railway code --codex --new --env-file .env\n  railway code --codex -- exec \"explain this codebase\"\n\nAgents persist between runs: disconnecting sleeps yours, and the next\n`railway code` wakes it with your work still on disk. `--keep-awake` leaves it\nrunning; `railway code --rm` destroys it.\n\nNote: requires the CLOUD_AGENTS feature to be enabled."
)]
pub struct Args {
    /// Launch OpenAI Codex using your local ChatGPT sign-in (~/.codex/auth.json)
    #[clap(long)]
    codex: bool,

    /// Launch Claude Code — runs `claude setup-token` for you to mint a
    /// token for the VM (CLAUDE_CODE_OAUTH_TOKEN / ANTHROPIC_API_KEY env
    /// variables skip that when set)
    #[clap(long)]
    claude: bool,

    /// Launch Grok CLI using your local sign-in (~/.grok/auth.json)
    #[clap(long)]
    grok: bool,

    /// Always create a fresh agent instead of reusing this environment's
    #[clap(long)]
    new: bool,

    /// Leave the agent running on disconnect instead of putting it to sleep.
    /// A running agent keeps billing for compute
    #[clap(long)]
    keep_awake: bool,

    /// Destroy this environment's agent and exit. Its disk goes with it
    #[clap(long)]
    rm: bool,

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

    /// Also inject your GitHub auth (read via `gh auth token`) so git and gh
    /// can reach your repos over HTTPS inside the agent
    #[clap(long)]
    gh: bool,

    /// Environment name or ID (defaults to the linked environment)
    #[clap(long, short)]
    environment: Option<String>,

    /// Project ID (defaults to the linked project)
    #[clap(long, short)]
    project: Option<String>,

    /// Extra arguments passed through to the agent (after `--`)
    #[clap(trailing_var_arg = true)]
    agent_args: Vec<String>,
}

/// The coding agent to launch, and the two things that differ between them:
/// where the local sign-in lives, and how its credential is written on the VM.
/// Installing and configuring the harness is the image's job, not ours.
#[derive(Clone, Copy, PartialEq)]
enum Agent {
    Codex,
    Claude,
    Grok,
}

impl Agent {
    /// The remote binary name (also what's autostarted on reconnect).
    fn name(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::Claude => "claude",
            Agent::Grok => "grok",
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Agent::Codex => "--codex",
            Agent::Claude => "--claude",
            Agent::Grok => "--grok",
        }
    }

    /// Human-facing product name for announce/error copy.
    fn display(self) -> &'static str {
        match self {
            Agent::Codex => "Codex",
            Agent::Claude => "Claude Code",
            Agent::Grok => "Grok",
        }
    }

    fn credential_seed(self) -> &'static str {
        match self {
            Agent::Codex => CODEX_SEED,
            Agent::Claude => CLAUDE_SEED,
            Agent::Grok => GROK_SEED,
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

/// Second `--claude` provision, run only when the user has a local
/// `~/.claude/settings.json`: merge it into the VM's so their setup
/// (permissions mode, model, plugins, statusline) carries over.
///
/// A merge, emphatically not the overwrite this used to be: on an agent VM
/// `~/.claude/settings.json` is co-owned — express-agent writes the railway and
/// playwright MCP servers and its hook entries there, and railway-agent reads
/// the same file. Truncating it would strip the harness's Railway tools until
/// the next boot reconcile. So the laptop's keys win where they overlap, and
/// `hooks`/`mcpServers` are left to their owner. The user's settings arrive on
/// stdin as a file rather than an argv because they can be large and are the
/// user's own data.
const CLAUDE_SETTINGS_PROVISION: &str = r#"umask 077
mkdir -p ~/.claude
cat > /tmp/.railway-code-settings.json
if [ ! -s ~/.claude/settings.json ]; then
  mv /tmp/.railway-code-settings.json ~/.claude/settings.json
  echo SETTINGS-OK
elif command -v jq >/dev/null 2>&1; then
  jq -s '.[0] * (.[1] | del(.hooks, .mcpServers))' ~/.claude/settings.json /tmp/.railway-code-settings.json > ~/.claude/settings.json.new 2>/dev/null \
    && mv ~/.claude/settings.json.new ~/.claude/settings.json && echo SETTINGS-OK \
    || { rm -f ~/.claude/settings.json.new; echo SETTINGS-MERGE-FAILED; }
  rm -f /tmp/.railway-code-settings.json
else
  echo SETTINGS-NO-JQ
  rm -f /tmp/.railway-code-settings.json
fi"#;

/// The user's local Claude settings, when they have any (`None` when the
/// file is missing or empty — the sandbox then just gets the onboarding
/// seed).
fn local_claude_settings() -> Option<Vec<u8>> {
    let path = dirs::home_dir()?.join(".claude").join("settings.json");
    std::fs::read(&path).ok().filter(|b| !b.is_empty())
}

/// Grok-specific VM seed: the credential is the user's local
/// `~/.grok/auth.json`, arriving on stdin into a 0600 file like codex. grok's
/// always-approve posture (`permission_mode = "bypassPermissions"`) and its MCP
/// servers are reconciled into `~/.grok/config.toml` at boot by express-agent,
/// and the image puts `~/.grok/bin` on PATH via `/etc/environment`, so neither
/// the old `[ui] yolo` merge nor a `/usr/local/bin` symlink is needed.
const GROK_SEED: &str = r#"mkdir -p ~/.grok
cat > ~/.grok/auth.json"#;

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
fn provision_script(agent: Agent) -> String {
    let seed = agent.credential_seed();
    let name = agent.name();
    format!(
        r#"umask 077
{seed}
{COMMON_SEED}
echo {name} > ~/.railway-code-agent
if command -v {name} >/dev/null 2>&1; then echo AGENT-READY; else echo AGENT-MISSING; fi"#
    )
}

/// `--gh` provision (rungate-proven recipe): the token arrives on stdin into
/// a 0600 file, an idempotent ~/.profile line exports GH_TOKEN for login
/// shells, and a git credential helper reads the file for HTTPS pulls/pushes.
/// Deliberately no `gh auth login` and no gh install requirement: GH_TOKEN is
/// gh's own documented env var, so gh works if present, and git works either
/// way. The helper re-reads the file per invocation, so refreshing the token
/// is just re-running with --gh.
const GH_PROVISION: &str = r##"umask 077
cat > ~/.gh-token
chmod 600 ~/.gh-token
grep -q "railway-code gh-token" ~/.profile 2>/dev/null || printf '\n%s\n%s\n' "# railway-code gh-token" 'export GH_TOKEN="$(cat ~/.gh-token 2>/dev/null)"' >> ~/.profile
git config --global credential."https://github.com".helper "!f(){ echo username=x-access-token; echo \"password=\$(cat ~/.gh-token)\"; };f" 2>/dev/null || true
git config --global credential."https://gist.github.com".helper "!f(){ echo username=x-access-token; echo \"password=\$(cat ~/.gh-token)\"; };f" 2>/dev/null || true
echo GH-OK"##;

/// Read the host's GitHub token via the gh CLI — the source of truth that
/// works regardless of where gh stores it (macOS keychain, hosts.yml, env).
fn host_gh_token() -> Result<String> {
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .map_err(|_| {
            anyhow!(
                "--gh needs the GitHub CLI on this machine (brew install gh), or drop the flag."
            )
        })?;
    let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || tok.is_empty() {
        bail!("`gh auth token` returned nothing — run `gh auth login` first, or drop --gh.");
    }
    Ok(tok)
}

/// SSH options shared by every connection this command runs, plus the info
/// needed to self-heal our relay known-hosts file. Two layers:
///
/// **Multiplexing** — the relay fleet answers with per-instance host keys, so
/// each fresh TCP connection is a new host-key roll; a multiplexed session
/// rides the master's already-verified connection. One `railway code` run
/// makes exactly one host-key decision instead of one per step.
/// ControlPersist keeps the master alive briefly so the interactive launch
/// reuses the provisioning master.
///
/// **Dedicated known-hosts** — the fleet currently presents many distinct
/// per-instance keys behind one hostname (7+ observed), so pinning a single
/// key is both futile (most connections mismatch) and security theater (a
/// fresh TOFU accept is indistinguishable from a MITM anyway). Relay
/// connections from this command therefore verify against the CLI's own
/// file (`~/.railway/known_hosts_relay`) with accept-new, leaving the user's
/// ~/.ssh/known_hosts untouched, and `ssh_plumbing` may heal THIS file (and
/// only this file) on a mismatch. Revisit when the relay ships a stable
/// shared host key or CA: flip to strict checking against the published key.
#[derive(Clone)]
struct RelaySsh {
    opts: Vec<String>,
    known_hosts: std::path::PathBuf,
    /// known-hosts pattern for ssh-keygen -R: `host` or `[host]:port`.
    host_pattern: String,
}

fn relay_ssh() -> Result<RelaySsh> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to get home directory"))?;
    let ssh_dir = home.join(".ssh");
    if !ssh_dir.exists() {
        std::fs::create_dir_all(&ssh_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ssh_dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let railway_dir = home.join(".railway");
    std::fs::create_dir_all(&railway_dir)?;
    let known_hosts = railway_dir.join("known_hosts_relay");

    let (host, port) = Configs::get_ssh_relay();
    let host_pattern = match port {
        Some(p) if p != 22 => format!("[{host}]:{p}"),
        _ => host.to_string(),
    };

    // %C hashes (local host, remote user, host, port) — short & per-target,
    // safely under the unix socket path length limit.
    let control_path = ssh_dir.join("railway-cm-%C");
    Ok(RelaySsh {
        opts: vec![
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            format!("ControlPath={}", control_path.display()),
            "-o".into(),
            "ControlPersist=90s".into(),
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

/// Read the local Codex sign-in (`~/.codex/auth.json`). Returns the
/// credential bytes plus a human label for the announce line.
fn codex_credentials() -> Result<(Vec<u8>, String)> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to get home directory"))?;
    let auth_path = home.join(".codex").join("auth.json");
    if !auth_path.exists() {
        bail!(
            "No Codex sign-in found at {}.\nRun `codex login` locally first (or `codex login --device-auth` on this machine), then re-run this command.",
            auth_path.display()
        );
    }
    let bytes = std::fs::read(&auth_path)?;
    if bytes.is_empty() {
        bail!(
            "{} is empty — run `codex login` locally first.",
            auth_path.display()
        );
    }
    Ok((bytes, auth_path.display().to_string()))
}

/// Read the local Grok sign-in (`~/.grok/auth.json`) — the same
/// copy-the-local-login shape as codex.
fn grok_credentials() -> Result<(Vec<u8>, String)> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to get home directory"))?;
    let auth_path = home.join(".grok").join("auth.json");
    if !auth_path.exists() {
        bail!(
            "No Grok sign-in found at {}.\nRun `grok` locally and sign in first, then re-run this command.",
            auth_path.display()
        );
    }
    let bytes = std::fs::read(&auth_path)?;
    if bytes.is_empty() {
        bail!(
            "{} is empty — run `grok` locally and sign in first.",
            auth_path.display()
        );
    }
    Ok((bytes, auth_path.display().to_string()))
}

/// Resolve the Claude Code credential as one `KEY=VALUE` env line, mirroring
/// mono's agent-vm Connect tab flow: a deliberate long-lived token from
/// `claude setup-token` (or an Anthropic API key) — NOT the local sign-in's
/// `.credentials.json`. That blob carries the refresh token; two machines
/// racing one rotating refresh token can sign the laptop out, and a
/// setup-token is its own revocable grant. Sources, in order: local
/// CLAUDE_CODE_OAUTH_TOKEN, local ANTHROPIC_API_KEY, then `claude
/// setup-token` run automatically on the user's terminal, then a manual
/// paste prompt as the last resort.
fn claude_credentials() -> Result<(Vec<u8>, String)> {
    use colored::Colorize;

    for var in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"] {
        if let Ok(tok) = std::env::var(var) {
            let tok = tok.trim().to_string();
            if !tok.is_empty() {
                validate_claude_token(&tok)?;
                return Ok((format!("{var}={tok}\n").into_bytes(), format!("${var}")));
            }
        }
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!(
            "No Claude credential found. Set CLAUDE_CODE_OAUTH_TOKEN (from `claude setup-token`) or ANTHROPIC_API_KEY, then re-run this command."
        );
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
            return Ok((
                format!("CLAUDE_CODE_OAUTH_TOKEN={tok}\n").into_bytes(),
                "claude setup-token".to_string(),
            ));
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
        "Run `claude setup-token` on this machine, then paste the token",
    )?;
    let tok = tok.trim().to_string();
    if tok.is_empty() {
        bail!("No token pasted — run `claude setup-token` and paste its output.");
    }
    validate_claude_token(&tok)?;
    Ok((
        format!("CLAUDE_CODE_OAUTH_TOKEN={tok}\n").into_bytes(),
        "claude setup-token".to_string(),
    ))
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
    const ATTEMPTS: u32 = 4;
    let mut last = (1, String::new());
    for attempt in 1..=ATTEMPTS {
        let (code, out, err) =
            run_native_ssh_captured(target, command, identity, stdin_payload, &relay.opts)?;
        if code == 0 {
            return Ok(out);
        }
        let err_text = String::from_utf8_lossy(&err).trim().to_string();
        let hostkey_mismatch = err_text.contains("Host key verification failed")
            || err_text.contains("REMOTE HOST IDENTIFICATION HAS CHANGED");
        if hostkey_mismatch {
            relay.heal_known_hosts();
        }
        last = (code, err_text);
        if attempt < ATTEMPTS && !hostkey_mismatch {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
    let (code, err_text) = last;
    if err_text.is_empty() {
        bail!("SSH to the agent failed after {ATTEMPTS} attempts (exit {code}).")
    }
    bail!("SSH to the agent failed after {ATTEMPTS} attempts (exit {code}):\n{err_text}")
}

/// The literal `ssh` command for a relay target, for the disconnect hint. The
/// relay is the same one this command connected through, so whatever worked here
/// works when pasted — including the dev relay's non-default port.
fn raw_ssh_hint(target: &str) -> String {
    let (host, port) = Configs::get_ssh_relay();
    match port {
        Some(p) if p != 22 => format!("ssh -p {p} {target}@{host}"),
        _ => format!("ssh {target}@{host}"),
    }
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
) -> Result<Option<CodeAgent>> {
    use colored::Colorize;
    use queries::cloud_agent::CloudAgentStatus as S;

    match agent.status {
        S::RUNNING => {
            println!("Using agent {} (--new for a fresh one)", agent.name.cyan());
            Ok(Some(agent))
        }
        // STARTING means a previous run is still booting it, so a re-run seconds
        // after a ctrl-c waits rather than minting a duplicate. SLEEPING is the
        // resting state this command leaves behind.
        S::SLEEPING | S::STARTING => {
            let mut spinner = create_shimmer_spinner(&format!("Waking agent {}", agent.name));
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
                fail_spinner(&mut spinner, "Wake failed".to_string());
                return Err(e.into());
            }
            match wait_until_running(client, backboard, environment_id, &agent.id).await {
                Ok(running) => {
                    spinner.finish_and_clear();
                    println!(
                        "Woke agent {} — your work is on its disk",
                        running.name.cyan()
                    );
                    Ok(Some(running))
                }
                Err(e) => {
                    fail_spinner(&mut spinner, "Wake failed".to_string());
                    Err(e)
                }
            }
        }
        S::CRASHED | S::FAILED | S::DELETING | S::Other(_) => {
            eprintln!(
                "{}",
                format!(
                    "Agent {} is {:?}; creating a fresh one.",
                    agent.name, agent.status
                )
                .yellow()
            );
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
            "You have {} cloud agents in this environment and no local record of which one `railway code` should use:\n{}\nPick one in the dashboard, or `railway code --new` to add another.",
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
async fn resolve_agent(
    configs: &mut Configs,
    client: &reqwest::Client,
    args: &Args,
    environment_id: &str,
) -> Result<(CodeAgent, bool)> {
    use colored::Colorize;

    let backboard = configs.get_backboard();

    let candidate = if args.new {
        None
    } else {
        match configs.get_code_agent(environment_id) {
            Some(id) => Some(id),
            None => sole_owned_agent_id(client, &backboard, environment_id).await?,
        }
    };
    // Re-read by id either way, so both paths carry the same shape and the
    // stale-pointer case (agent deleted elsewhere) collapses into `None`.
    let existing = match candidate {
        Some(id) => fetch_agent(client, &backboard, environment_id, &id).await?,
        None => None,
    };
    if let Some(agent) = existing {
        if let Some(ready) = ready_existing_agent(client, &backboard, environment_id, agent).await?
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
    let mut spinner = create_shimmer_spinner("Creating a cloud agent");
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
        Err(e) => {
            fail_spinner(&mut spinner, "Create failed".to_string());
            return Err(e.into());
        }
    };

    // Remembered before the box is up: a create that succeeds and then times out
    // waiting has still spent a VM, and the pointer is the only handle the next
    // run has to it.
    configs.set_code_agent(environment_id, &created.id);
    configs.write()?;

    match wait_until_running(client, &backboard, environment_id, &created.id).await {
        Ok(running) => {
            spinner.finish_and_clear();
            println!("✓ Created agent {}", running.name.cyan());
            Ok((running, true))
        }
        Err(e) => {
            fail_spinner(&mut spinner, "Agent did not start".to_string());
            Err(e)
        }
    }
}

/// `--variable`/`--env-file` only reach the VM spec at create time, so say so
/// rather than silently dropping them on a reuse.
fn warn_ignored_variables(args: &Args) {
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
async fn destroy_agent(
    configs: &mut Configs,
    client: &reqwest::Client,
    environment_id: &str,
) -> Result<()> {
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

pub async fn command(args: Args) -> Result<()> {
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

    let agent = match (args.codex, args.claude, args.grok) {
        (true, false, false) => Agent::Codex,
        (false, true, false) => Agent::Claude,
        (false, false, true) => Agent::Grok,
        (false, false, false) => bail!(
            "Specify which agent to launch, e.g.:\n  railway code --codex\n  railway code --claude\n  railway code --grok"
        ),
        _ => bail!("Pick one agent: --codex, --claude, or --grok."),
    };

    eprintln!(
        "{}",
        "Warning: Railway cloud agents are experimental and APIs may change or break during testing."
            .yellow()
    );

    // --- Resolve the local credential (client-side only, announced).
    let (auth_bytes, auth_source) = match agent {
        Agent::Codex => codex_credentials()?,
        Agent::Claude => claude_credentials()?,
        Agent::Grok => grok_credentials()?,
    };
    if args.gh {
        eprintln!(
            "Using your {} credential ({auth_source}) and GitHub token (`gh auth token`) on the agent",
            agent.display()
        );
    } else {
        eprintln!(
            "Using your {} credential ({auth_source}) on the agent",
            agent.display()
        );
    }
    // Read the GitHub token before spending a VM, so a missing gh login fails
    // fast and cheap.
    let gh_token = if args.gh {
        Some(host_gh_token()?)
    } else {
        None
    };
    // Mirror the user's local Claude settings onto the agent so their setup
    // carries over; express-agent's own entries in that file survive the merge.
    let claude_settings = if agent == Agent::Claude {
        let settings = local_claude_settings();
        if settings.is_some() {
            eprintln!("Including your local Claude settings (~/.claude/settings.json)");
        }
        settings
    } else {
        None
    };

    // --- Resolve where the agent lives.
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (project_id, environment_id) = resolve_project_and_env(
        &mut configs,
        &client,
        args.project.clone(),
        args.environment.clone(),
    )
    .await?;

    let (cloud_agent, created) =
        resolve_agent(&mut configs, &client, &args, &environment_id).await?;
    configs.set_code_agent(&environment_id, &cloud_agent.id);
    configs.write()?;

    let identity = ensure_ssh_key_quiet(&client, &configs).await?;
    // The relay's cloud-agent grammar; by id rather than name because names are
    // not unique within an environment.
    let target = format!("agent:{environment_id}:{}", cloud_agent.id);

    // Multiplex every ssh in this run over one verified connection: the
    // provisioning call establishes the master, the interactive launch rides
    // it — one host-key decision per run, not one per connection.
    let relay = relay_ssh()?;

    // --- Provision: credential (stdin) + reconnect seeds, one script.
    {
        let target = target.clone();
        let identity = identity.clone();
        let relay = relay.clone();
        let gh_token = gh_token.clone();
        let mut spinner = create_shimmer_spinner(&format!("Provisioning {}", agent.name()));
        let provision = tokio::task::spawn_blocking(move || -> Result<()> {
            let out = ssh_plumbing(
                &target,
                &provision_script(agent),
                identity.as_deref(),
                Some(&auth_bytes),
                &relay,
            )?;
            let out = String::from_utf8_lossy(&out);
            if out.contains("AGENT-READY") {
                // ok
            } else if out.contains("AGENT-MISSING") {
                bail!(
                    "`{}` isn't on this agent's image. Cloud agents bake every harness, so this is an image problem rather than something to retry — report it with the agent id.",
                    agent.name()
                )
            } else {
                bail!("Provisioning produced no status marker — the connection likely dropped mid-script.")
            }
            if let Some(settings) = claude_settings {
                // Rides the same multiplexed connection — no new host-key roll.
                let out = ssh_plumbing(
                    &target,
                    CLAUDE_SETTINGS_PROVISION,
                    identity.as_deref(),
                    Some(&settings),
                    &relay,
                )?;
                let out = String::from_utf8_lossy(&out);
                // A failed merge is not worth aborting a launch over: the agent
                // is fully usable on its own settings, only the laptop's
                // preferences are missing. Say so and carry on.
                if !out.contains("SETTINGS-OK") {
                    eprintln!(
                        "Couldn't merge your local Claude settings onto the agent; continuing with the agent's own."
                    );
                }
            }
            if let Some(tok) = gh_token {
                let out = ssh_plumbing(
                    &target,
                    GH_PROVISION,
                    identity.as_deref(),
                    Some(tok.as_bytes()),
                    &relay,
                )?;
                if !String::from_utf8_lossy(&out).contains("GH-OK") {
                    bail!("GitHub auth provisioning did not complete on the agent.")
                }
            }
            Ok(())
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
        match provision {
            Ok(()) => spinner.finish_and_clear(),
            Err(e) => {
                fail_spinner(&mut spinner, "Provisioning failed".to_string());
                return Err(e);
            }
        }
    }

    // --- Launch: interactive agent over the relay (a real PTY is allocated),
    // multiplexed over the provisioning master. Command sessions don't source
    // ~/.profile, so the GH_TOKEN export is inlined here (no-op when --gh
    // wasn't used — the guard keeps an empty var from shadowing gh's config).
    //
    // Deliberately no `cd`: the machine's spec sets workDir for the workload and
    // every in-VM session (`/app`, the workspace dir express-agent reconciles
    // instructions and per-project trust into), so forcing $HOME here would
    // override a platform default and drop the agent somewhere its harness
    // config does not cover.
    let env_prefix = "[ -f ~/.gh-token ] && export GH_TOKEN=\"$(cat ~/.gh-token)\"; [ -f ~/.claude-code-env ] && set -a && . ~/.claude-code-env && set +a; [ -d ~/.grok/bin ] && export PATH=\"$HOME/.grok/bin:$PATH\"; ";
    let remote_cmd = if args.agent_args.is_empty() {
        // Interactive: no `exec` — quitting the agent lands in a shell on the
        // agent (matching the ~/.profile autostart behavior) instead of tearing
        // the whole session down. The exported guard keeps the login shell's
        // profile autostart from relaunching the agent on top of the user. The
        // reset scrubs terminal state a TUI leaves behind on an unclean exit
        // (kitty keyboard mode et al) before the shell takes over.
        format!(
            "{env_prefix}export RAILWAY_CODE_AUTOSTARTED=1; {}; {}; exec bash -l",
            agent.name(),
            terminal_reset_printf()
        )
    } else {
        // Scripted (`-- exec …`, `-- --version`): exit when the agent does —
        // a trailing shell would hang pipelines waiting on it.
        format!(
            "{env_prefix}exec {} {}",
            agent.name(),
            shell_join(&args.agent_args)
        )
    };

    println!("Launching {}…", agent.name());
    let cmd = vec![remote_cmd];
    let ssh_target = target.clone();
    let ssh_relay = relay.clone();
    let ssh_identity = identity.clone();
    let exit_code = tokio::task::spawn_blocking(move || {
        run_native_ssh_with_opts(
            &ssh_target,
            Some(&cmd),
            ssh_identity.as_deref(),
            None,
            &ssh_relay.opts,
        )
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)?;

    // Belt-and-suspenders for the remote reset: when the connection drops
    // mid-TUI the remote printf never reaches us, so scrub locally too before
    // printing anything. No-op on a clean terminal.
    if std::io::stdout().is_terminal() {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(TERMINAL_RESET.as_bytes());
        let _ = out.flush();
    }

    // --- Sleep on disconnect. An agent has no idle timeout, so nothing else
    // will ever put this box down; leaving it running bills compute until the
    // user remembers it. Sleeping keeps the disk, so the work survives and the
    // next run wakes into it. Best-effort: a failure here is worth a warning
    // (the user is now paying for a live VM) but not a non-zero exit on an
    // otherwise successful session.
    if args.keep_awake {
        println!(
            "\nDisconnected — agent {} is still running (--keep-awake).",
            cloud_agent.name.cyan()
        );
    } else {
        let mut spinner = create_shimmer_spinner("Sleeping the agent");
        match post_graphql::<mutations::CloudAgentSleep, _>(
            &client,
            configs.get_backboard(),
            mutations::cloud_agent_sleep::Variables {
                id: cloud_agent.id.clone(),
            },
        )
        .await
        {
            Ok(_) => {
                spinner.finish_and_clear();
                println!(
                    "\nDisconnected — agent {} is asleep; your work is on its disk.",
                    cloud_agent.name.cyan()
                );
            }
            Err(e) => {
                fail_spinner(&mut spinner, "Sleep failed".to_string());
                eprintln!(
                    "{}",
                    format!(
                        "Agent {} is still running and billing compute. Sleep it from the dashboard, or `railway code {} --rm` to destroy it. ({e})",
                        cloud_agent.name,
                        agent.flag()
                    )
                    .yellow()
                );
            }
        }
    }

    if created {
        println!("Agents persist between runs — this one is yours until you --rm it.");
    }
    println!("Get back in:");
    println!(
        "  railway code {}   # wakes it and drops back into {}",
        agent.flag(),
        agent.name()
    );
    // `railway ssh` addresses services and deployments, not agents, so the
    // plain-shell route is ssh itself against the relay. Only useful while the
    // agent is awake — hence second, after the command that wakes it.
    println!("  {}   # plain shell (once awake)", raw_ssh_hint(&target));
    println!("Destroy it:");
    println!("  railway code --rm -p {project_id} -e {environment_id}");

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provision_script_delivers_credentials_only() {
        let codex = provision_script(Agent::Codex);
        assert!(codex.contains("cat > ~/.codex/auth.json"));
        assert!(codex.contains("echo codex > ~/.railway-code-agent"));

        let claude = provision_script(Agent::Claude);
        assert!(claude.contains("cat > ~/.claude-code-env"));
        assert!(claude.contains("echo claude > ~/.railway-code-agent"));

        let grok = provision_script(Agent::Grok);
        assert!(grok.contains("cat > ~/.grok/auth.json"));
        assert!(grok.contains("echo grok > ~/.railway-code-agent"));

        for script in [&codex, &claude, &grok] {
            // Shared plumbing: reconnect autostart, env sourcing, and the
            // markers the provisioning caller matches on.
            assert!(script.contains("railway-code agent autostart"));
            assert!(script.contains(". \"$HOME/.claude-code-env\""));
            assert!(script.contains("AGENT-READY"));
            assert!(script.contains("AGENT-MISSING"));

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

    #[test]
    fn claude_settings_provision_merges_rather_than_truncates() {
        // A truncating write here would strip express-agent's railway and
        // playwright MCP servers plus its hook entries from the file it
        // co-owns, leaving the harness without Railway tools until the next
        // boot reconcile.
        assert!(!CLAUDE_SETTINGS_PROVISION.contains("cat > ~/.claude/settings.json"));
        assert!(CLAUDE_SETTINGS_PROVISION.contains("del(.hooks, .mcpServers)"));
        assert!(CLAUDE_SETTINGS_PROVISION.contains("SETTINGS-OK"));
        // Onboarding/trust is express-agent's to seed at boot; the CLI must not
        // have quietly reacquired it via the settings path.
        assert!(!CLAUDE_SEED.contains("hasCompletedOnboarding"));
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
