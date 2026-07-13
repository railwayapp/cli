use anyhow::{Result, anyhow, bail};
use clap::Parser;
use is_terminal::IsTerminal;

use crate::client::{GQLClient, post_graphql};
use crate::commands::sandbox::{
    CreateReport, create_and_store, resolve_project_and_env, spawn_heartbeat, variables_to_input,
};
use crate::commands::ssh::{
    ensure_ssh_key_quiet, run_native_ssh_captured, run_native_ssh_with_opts,
};
use crate::config::Configs;
use crate::gql::{mutations, queries};
use crate::util::progress::{create_shimmer_spinner, fail_spinner};
use crate::util::shell::shell_join;

// ---------------------------------------------------------------------------
// `railway code --codex` / `railway code --claude` — launch a coding agent in
// a Railway sandbox on the user's own plan.
//
// Auth shape: Codex copies the user's existing local sign-in
// (`~/.codex/auth.json`) — the flow OpenAI documents for remote machines.
// Claude uses a deliberate long-lived token (`claude setup-token` output, or
// an ANTHROPIC_API_KEY), matching mono's agent-vm Connect tab flow — never
// the local sign-in's `.credentials.json`, whose refresh token two machines
// can't safely share. Either credential is announced to the user, read
// client-side, and rides ssh stdin into a 0600 file in the sandbox: it never
// appears in an argv, a Railway variable, an image, or server-side config.
// Nothing is stored locally by this command.
// ---------------------------------------------------------------------------

/// Launch a coding agent in a Railway sandbox
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway code --codex              # sandbox + your local Codex sign-in\n  railway code --claude             # sandbox + your Claude setup-token\n  railway code --codex --new        # force a fresh sandbox\n  railway code --claude --gh        # also inject your GitHub auth (gh auth token)\n  railway code --codex --new --variable DB_URL=postgres.DATABASE_URL\n  railway code --codex --new --env-file .env\n  railway code --codex -- exec \"explain this codebase\"\n\nNote: requires the PROJECT_SANDBOXES feature to be enabled."
)]
pub struct Args {
    /// Launch OpenAI Codex using your local ChatGPT sign-in (~/.codex/auth.json)
    #[clap(long)]
    codex: bool,

    /// Launch Claude Code — runs `claude setup-token` for you to mint a
    /// sandbox token (CLAUDE_CODE_OAUTH_TOKEN / ANTHROPIC_API_KEY env
    /// variables skip that when set)
    #[clap(long)]
    claude: bool,

    /// Always create a fresh sandbox instead of reusing the active one
    #[clap(long)]
    new: bool,

    /// Minutes the sandbox may sit idle (disconnected) before it is
    /// auto-destroyed
    #[clap(long, value_name = "MINUTES", default_value = "30")]
    idle_timeout: i64,

    /// Set a variable on the sandbox (repeatable, comma-separable). Values
    /// may reference other variables — `DB_URL=postgres.DATABASE_URL` or the
    /// full `${{postgres.DATABASE_URL}}` form — resolved server-side at
    /// create time. Applies to newly created sandboxes (combine with --new)
    #[clap(long = "variable", value_name = "KEY=VALUE[,KEY=VALUE...]")]
    variables: Vec<String>,

    /// Load variables from a .env file (repeatable). `--variable` flags
    /// override file entries with the same key
    #[clap(long = "env-file", value_name = "PATH")]
    env_files: Vec<std::path::PathBuf>,

    /// Also inject your GitHub auth (read via `gh auth token`) so git and gh
    /// can reach your repos over HTTPS inside the sandbox
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

/// The coding agent to launch, and everything that differs between them:
/// where the local sign-in lives, how the sandbox-side seed looks, and what
/// to install when the image doesn't ship the binary.
#[derive(Clone, Copy, PartialEq)]
enum Agent {
    Codex,
    Claude,
}

impl Agent {
    /// The remote binary name (also what's autostarted on reconnect).
    fn name(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::Claude => "claude",
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Agent::Codex => "--codex",
            Agent::Claude => "--claude",
        }
    }

    /// Human-facing product name for announce/error copy.
    fn display(self) -> &'static str {
        match self {
            Agent::Codex => "Codex",
            Agent::Claude => "Claude Code",
        }
    }

    fn npm_package(self) -> &'static str {
        match self {
            Agent::Codex => "@openai/codex",
            Agent::Claude => "@anthropic-ai/claude-code",
        }
    }

    /// Silent refresh run during provisioning (behind the spinner) when the
    /// binary already exists — synchronous on purpose: every fresh sandbox
    /// starts from the image's baked version, so a background update always
    /// loses the race against the launch and codex greets the user with an
    /// "update available" banner. The cheap path is taken when possible:
    /// codex compares installed vs registry (~2s when current) and only pays
    /// for the install on an actual version gap; claude's own updater
    /// no-ops quickly when current. `timeout` bounds a wedged registry so
    /// provisioning can't hang on it.
    fn update_snippet(self) -> &'static str {
        match self {
            Agent::Codex => {
                r#"current="$(codex --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -n1)"
latest="$(timeout 15 npm view @openai/codex version 2>/dev/null)"
if [ -n "$latest" ] && [ "$current" != "$latest" ]; then timeout 180 npm install -g @openai/codex@latest >/dev/null 2>&1; fi"#
            }
            Agent::Claude => "timeout 180 claude update >/dev/null 2>&1 || true",
        }
    }

    fn credential_seed(self) -> &'static str {
        match self {
            Agent::Codex => CODEX_SEED,
            Agent::Claude => CLAUDE_SEED,
        }
    }
}

/// Codex-specific sandbox seed. The credential arrives on stdin into a 0600
/// file (never an argv). config.toml pre-trusts the dirs codex lands in
/// ($HOME via `cd ~`, and / where the relay drops non-cd sessions) or the
/// TUI stops at a folder-trust prompt even when authenticated — only seeded
/// when no config exists, so a user-customized config is never clobbered.
/// bubblewrap: codex warns at startup when distro bwrap is absent (it falls
/// back to its bundled copy, so this is cosmetic); best-effort apt install
/// (~8s) until the sandbox image ships it — the `command -v` guard makes
/// this a free no-op once it does.
const CODEX_SEED: &str = r#"mkdir -p ~/.codex
cat > ~/.codex/auth.json
if [ ! -f ~/.codex/config.toml ]; then
cat > ~/.codex/config.toml <<'EOF'
[projects."/root"]
trust_level = "trusted"

[projects."/"]
trust_level = "trusted"
EOF
fi
command -v bwrap >/dev/null 2>&1 || { apt-get update >/dev/null 2>&1 && apt-get install -y bubblewrap >/dev/null 2>&1; } || true"#;

/// Claude-specific sandbox seed. The credential is one `KEY=VALUE` line —
/// `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token`, or a passed-through
/// `ANTHROPIC_API_KEY` — arriving on stdin into a 0600 env file that login
/// shells and the launch prefix source (claude reads the env var; the key
/// name rides the payload so both var names work without baking either into
/// the script). ~/.claude.json is the gotcha: until it says onboarding is
/// done, claude ignores the env token and shows the first-run login picker
/// — and the sandbox image SHIPS a ~/.claude.json (the image build runs
/// claude once, stamping installMethod/firstStartTime) WITHOUT the flag, so
/// a write-only-when-absent seed never fires. Existing files therefore get
/// the flags MERGED in (jq ships in the image; node is the fallback), never
/// replaced — the rest of the state file is preserved. The same flags
/// pre-accept the folder-trust dialog for the dirs claude lands in.
const CLAUDE_SEED: &str = r#"cat > ~/.claude-code-env
chmod 600 ~/.claude-code-env
if [ ! -f ~/.claude.json ]; then
cat > ~/.claude.json <<'EOF'
{"hasCompletedOnboarding":true,"projects":{"/root":{"hasTrustDialogAccepted":true},"/":{"hasTrustDialogAccepted":true}}}
EOF
elif command -v jq >/dev/null 2>&1; then
jq '.hasCompletedOnboarding = true | .projects."/root".hasTrustDialogAccepted = true | .projects."/".hasTrustDialogAccepted = true' ~/.claude.json > ~/.claude.json.new 2>/dev/null && mv ~/.claude.json.new ~/.claude.json || rm -f ~/.claude.json.new
elif command -v node >/dev/null 2>&1; then
node -e 'const fs=require("fs");const p=process.env.HOME+"/.claude.json";const j=JSON.parse(fs.readFileSync(p,"utf8"));j.hasCompletedOnboarding=true;j.projects=Object.assign({},j.projects);for(const d of["/root","/"])j.projects[d]=Object.assign({},j.projects[d],{hasTrustDialogAccepted:true});fs.writeFileSync(p,JSON.stringify(j,null,2))' 2>/dev/null || true
fi"#;

/// Second `--claude` provision, run only when the user has a local
/// `~/.claude/settings.json`: mirror it into the sandbox so their setup
/// (permissions mode, model, plugins, statusline) carries over. Overwrites on
/// every provision — the laptop copy is the source of truth. The onboarding
/// disable does NOT ride this file; it's the ~/.claude.json flag in
/// `CLAUDE_SEED`, which is seeded whether or not local settings exist.
const CLAUDE_SETTINGS_PROVISION: &str = r#"umask 077
mkdir -p ~/.claude
cat > ~/.claude/settings.json
echo SETTINGS-OK"#;

/// The user's local Claude settings, when they have any (`None` when the
/// file is missing or empty — the sandbox then just gets the onboarding
/// seed).
fn local_claude_settings() -> Option<Vec<u8>> {
    let path = dirs::home_dir()?.join(".claude").join("settings.json");
    std::fs::read(&path).ok().filter(|b| !b.is_empty())
}

/// Agent-independent seeds, all idempotent:
/// - COLORTERM: the relay forwards TERM but not COLORTERM; without it TUIs
///   render a greyed/degraded palette.
/// - ~/.profile autostart: plain connects (`railway sandbox ssh`) run bash as
///   a login shell (verified: ~/.profile IS sourced; command sessions are
///   not), so any interactive reconnect drops into the agent recorded in
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
  if command -v "$agent" >/dev/null 2>&1; then
    export RAILWAY_CODE_AUTOSTARTED=1
    [ -f "$HOME/.gh-token" ] && export GH_TOKEN="$(cat "$HOME/.gh-token")"
    [ -f "$HOME/.claude-code-env" ] && set -a && . "$HOME/.claude-code-env" && set +a
    cd "$HOME" && "$agent"
    printf '\033[<u\033[<u\033[=0;1u\033[?2004l\033[?1000l\033[?1002l\033[?1003l\033[?1006l\033[?1004l\033[?25h'
  fi
fi
PROFEOF
fi"#;

/// The whole sandbox-side provision as ONE script over ONE connection — the
/// credential arrives on stdin (never an argv) into a 0600 file, then the
/// seeds and the agent install run. One connection instead of three matters:
/// success/failure markers ride stdout so a relay-level failure is
/// distinguishable from "npm missing" / "install failed".
///
/// When the binary already exists, `update_snippet` runs to completion
/// before the ready marker, so the agent that launches moments later is the
/// current version — see `update_snippet` for why background updating can't
/// achieve that. Only a missing binary takes the full install path.
fn provision_script(agent: Agent) -> String {
    let seed = agent.credential_seed();
    let name = agent.name();
    let pkg = agent.npm_package();
    let update = agent.update_snippet();
    format!(
        r#"umask 077
{seed}
{COMMON_SEED}
echo {name} > ~/.railway-code-agent
if command -v {name} >/dev/null 2>&1; then
{update}
echo AGENT-READY
exit 0
fi
command -v npm >/dev/null 2>&1 || {{ echo AGENT-NO-NPM; exit 0; }}
npm install -g {pkg} >/dev/null 2>&1
if command -v {name} >/dev/null 2>&1; then echo AGENT-READY; else echo AGENT-INSTALL-FAILED; fi"#
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
        bail!("SSH to the sandbox failed after {ATTEMPTS} attempts (exit {code}).")
    }
    bail!("SSH to the sandbox failed after {ATTEMPTS} attempts (exit {code}):\n{err_text}")
}

pub async fn command(args: Args) -> Result<()> {
    use colored::Colorize;

    let agent = match (args.codex, args.claude) {
        (true, false) => Agent::Codex,
        (false, true) => Agent::Claude,
        (true, true) => bail!("Pick one agent: --codex or --claude."),
        (false, false) => bail!(
            "Specify which agent to launch, e.g.:\n  railway code --codex\n  railway code --claude"
        ),
    };

    eprintln!(
        "{}",
        "Warning: Railway sandboxes are experimental and APIs may change or break during testing."
            .yellow()
    );

    // --- Resolve the local credential (client-side only, announced).
    let (auth_bytes, auth_source) = match agent {
        Agent::Codex => codex_credentials()?,
        Agent::Claude => claude_credentials()?,
    };
    if args.gh {
        eprintln!(
            "Using your {} credential ({auth_source}) and GitHub token (`gh auth token`) in the sandbox",
            agent.display()
        );
    } else {
        eprintln!(
            "Using your {} credential ({auth_source}) in the sandbox",
            agent.display()
        );
    }
    // Read the GitHub token before spending a sandbox, so a missing gh login
    // fails fast and cheap.
    let gh_token = if args.gh {
        Some(host_gh_token()?)
    } else {
        None
    };
    // Mirror the user's local Claude settings into the sandbox so their setup
    // carries over; without one, the CLAUDE_SEED onboarding disable still
    // applies.
    let claude_settings = if agent == Agent::Claude {
        let settings = local_claude_settings();
        if settings.is_some() {
            eprintln!("Including your local Claude settings (~/.claude/settings.json)");
        }
        settings
    } else {
        None
    };

    // --- Resolve where the sandbox lives.
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let (project_id, environment_id) =
        resolve_project_and_env(&mut configs, &client, args.project, args.environment).await?;

    // Reuse the active sandbox when it's still alive here — repeated runs
    // shouldn't mint a fleet of boxes. CREATING counts as alive: a re-run
    // seconds after a launch (flaky connection, ctrl-c) must reuse the box
    // that's still booting, not spawn a duplicate. `--new` forces a fresh one.
    let reusable = if args.new {
        None
    } else {
        match configs.get_active_sandbox() {
            Some(stored) if stored.environment_id == environment_id => {
                let res = post_graphql::<queries::Sandboxes, _>(
                    &client,
                    configs.get_backboard(),
                    queries::sandboxes::Variables {
                        environment_id: environment_id.clone(),
                        first: Some(100),
                        after: None,
                    },
                )
                .await?;
                res.sandboxes
                    .edges
                    .into_iter()
                    .map(|e| e.node)
                    .find(|n| {
                        n.id == stored.id
                            && matches!(
                                n.status,
                                queries::sandboxes::SandboxStatus::RUNNING
                                    | queries::sandboxes::SandboxStatus::CREATING
                            )
                    })
                    .map(|n| n.id)
            }
            _ => None,
        }
    };

    let sandbox_id = if let Some(id) = reusable {
        println!("Reusing active sandbox {id} (use --new for a fresh one)");
        if !args.variables.is_empty() || !args.env_files.is_empty() {
            eprintln!(
                "{}",
                "Note: --variable/--env-file only apply when a sandbox is created — reusing the active one. Add --new to create with these variables."
                    .yellow()
            );
        }
        id
    } else {
        let input = mutations::sandbox_create::SandboxCreateInput {
            environment_id: environment_id.clone(),
            // Default is a shorter idle window for a credential-bearing box
            // than the CLI default; it's interactive, so this only reaps
            // forgotten ones. `--idle-timeout` overrides.
            idle_timeout_minutes: Some(args.idle_timeout),
            template: None,
            source_sandbox_id: None,
            network_isolation: None,
            variables: variables_to_input(&args.env_files, &args.variables)?,
        };
        create_and_store(
            &mut configs,
            &client,
            project_id.clone(),
            environment_id.clone(),
            input,
            CreateReport::Quiet,
            false,
        )
        .await?
    };

    let identity = ensure_ssh_key_quiet(&client, &configs).await?;
    let target = format!("sbx:{environment_id}:{sandbox_id}");
    let heartbeat = spawn_heartbeat(
        client.clone(),
        configs.get_backboard(),
        environment_id.clone(),
        sandbox_id.clone(),
    );

    // Multiplex every ssh in this run over one verified connection: the
    // provisioning call establishes the master, the interactive launch rides
    // it — one host-key decision per run, not one per connection.
    let relay = relay_ssh()?;

    // --- Provision: credential (stdin) + seeds + agent install, one script.
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
            } else if out.contains("AGENT-NO-NPM") {
                bail!(
                    "The sandbox image has no npm, so {} can't be installed automatically.",
                    agent.name()
                )
            } else if out.contains("AGENT-INSTALL-FAILED") {
                bail!("`npm install -g {}` failed in the sandbox.", agent.npm_package())
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
                if !String::from_utf8_lossy(&out).contains("SETTINGS-OK") {
                    bail!("Claude settings provisioning did not complete in the sandbox.")
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
                    bail!("GitHub auth provisioning did not complete in the sandbox.")
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
                heartbeat.abort();
                return Err(e);
            }
        }
    }

    // --- Launch: interactive agent over the relay (a real PTY is allocated),
    // multiplexed over the provisioning master. Command sessions don't source
    // ~/.profile, so the GH_TOKEN export is inlined here (no-op when --gh
    // wasn't used — the guard keeps an empty var from shadowing gh's config).
    let env_prefix = "[ -f ~/.gh-token ] && export GH_TOKEN=\"$(cat ~/.gh-token)\"; [ -f ~/.claude-code-env ] && set -a && . ~/.claude-code-env && set +a; cd ~ && ";
    let remote_cmd = if args.agent_args.is_empty() {
        // Interactive: no `exec` — quitting the agent lands in a sandbox
        // shell (matching the ~/.profile autostart behavior) instead of
        // tearing the whole session down. The exported guard keeps the login
        // shell's profile autostart from relaunching the agent on top of the
        // user. The reset scrubs terminal state a TUI leaves behind on an
        // unclean exit (kitty keyboard mode et al) before the shell takes
        // over.
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
    let exit_code = tokio::task::spawn_blocking(move || {
        run_native_ssh_with_opts(&target, Some(&cmd), identity.as_deref(), None, &relay.opts)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)?;

    heartbeat.abort();

    // Belt-and-suspenders for the remote reset: when the connection drops
    // mid-TUI the remote printf never reaches us, so scrub locally too before
    // printing anything. No-op on a clean terminal.
    if std::io::stdout().is_terminal() {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(TERMINAL_RESET.as_bytes());
        let _ = out.flush();
    }

    // Where-did-my-sandbox-go breadcrumbs: the box outlives the session but
    // only for the idle window, and `sandbox list` is environment-scoped —
    // spell out the commands that find it from anywhere.
    println!(
        "\nDisconnected — sandbox {sandbox_id} stays up for ~{}m of idle time.",
        args.idle_timeout
    );
    println!("Get back in:");
    println!(
        "  railway sandbox ssh      # drops straight back into {}",
        agent.name()
    );
    println!(
        "  railway code {}     # same, from any dir linked to this project",
        agent.flag()
    );
    println!("Find it later (sandbox list is per-environment):");
    println!("  railway sandbox list -p {project_id} -e {environment_id}");

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provision_script_embeds_agent_specifics() {
        let codex = provision_script(Agent::Codex);
        assert!(codex.contains("cat > ~/.codex/auth.json"));
        assert!(codex.contains("npm install -g @openai/codex"));
        assert!(codex.contains("echo codex > ~/.railway-code-agent"));

        let claude = provision_script(Agent::Claude);
        assert!(claude.contains("cat > ~/.claude-code-env"));
        assert!(claude.contains("hasCompletedOnboarding"));
        // The image ships a ~/.claude.json without the onboarding flag, so
        // an existing file must be merged, not skipped.
        assert!(claude.contains("jq '.hasCompletedOnboarding = true"));
        assert!(claude.contains("npm install -g @anthropic-ai/claude-code"));
        assert!(claude.contains("echo claude > ~/.railway-code-agent"));

        // Synchronous refresh when the binary exists, so a fresh sandbox
        // launches the current version instead of the image's baked one:
        // codex checks the registry and installs only on a version gap;
        // claude's own updater no-ops when current. Both are bounded.
        assert!(codex.contains("npm view @openai/codex version"));
        assert!(codex.contains("npm install -g @openai/codex@latest"));
        assert!(claude.contains("timeout 180 claude update"));

        // Shared plumbing: reconnect autostart, env sourcing, and the status
        // markers the provisioning caller matches on.
        for script in [&codex, &claude] {
            assert!(script.contains("railway-code agent autostart"));
            assert!(script.contains(". \"$HOME/.claude-code-env\""));
            assert!(script.contains("AGENT-READY"));
            assert!(script.contains("AGENT-NO-NPM"));
            assert!(script.contains("AGENT-INSTALL-FAILED"));
        }
    }

    #[test]
    fn claude_settings_provision_writes_settings_json() {
        assert!(CLAUDE_SETTINGS_PROVISION.contains("cat > ~/.claude/settings.json"));
        assert!(CLAUDE_SETTINGS_PROVISION.contains("SETTINGS-OK"));
        // The onboarding disable must not depend on the settings mirror.
        assert!(CLAUDE_SEED.contains("hasCompletedOnboarding"));
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
