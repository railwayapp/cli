use std::{collections::HashMap, fs, io::IsTerminal, path::PathBuf, sync::OnceLock};

use anyhow::Context;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::client::GQLClient;
use crate::config::Configs;
use crate::consts::{
    RAILWAY_AGENT_SESSION_ENV, RAILWAY_CALLER_ENV, RAILWAY_INSTALL_REQUEST_ID_ENV,
};

pub struct CliTrackEvent {
    pub command: String,
    pub sub_command: Option<String>,
    pub duration_ms: u64,
    pub success: bool,
    pub error_message: Option<String>,
    pub os: &'static str,
    pub arch: &'static str,
    pub cli_version: &'static str,
    pub is_ci: bool,
}

pub struct SetupAgentTrackEvent {
    pub phase: SetupAgentPhase,
    pub success: Option<bool>,
    pub error_message: Option<String>,
    pub configured_clients: Option<Vec<String>>,
}

pub enum SetupAgentPhase {
    Start,
    Finish,
}

/// Identity surfaced by an MCP client during the JSON-RPC `initialize`
/// handshake. When set, it is the authoritative agent fingerprint for
/// MCP-driven tool events and overrides any env/process-tree heuristic.
#[derive(Clone, Debug)]
pub struct McpClientInfo {
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CliEventTrackInput {
    command: String,
    sub_command: Option<String>,
    duration_ms: i64,
    success: bool,
    error_message: Option<String>,
    os: String,
    arch: String,
    cli_version: String,
    is_ci: bool,
    session_id: String,
    caller: String,
    agent_session_id: Option<String>,
    install_request_id: Option<String>,
    project_id: Option<String>,
    environment_id: Option<String>,
    service_id: Option<String>,
    error_class: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LegacyCliEventTrackInput {
    command: String,
    sub_command: Option<String>,
    duration_ms: i64,
    success: bool,
    error_message: Option<String>,
    os: String,
    arch: String,
    cli_version: String,
    is_ci: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupAgentEventTrackInput {
    phase: &'static str,
    success: Option<bool>,
    error_message: Option<String>,
    configured_clients: Option<Vec<String>>,
    session_id: String,
    caller: String,
    agent_session_id: Option<String>,
    install_request_id: Option<String>,
    cli_version: String,
    os: String,
    arch: String,
    is_ci: bool,
}

#[derive(Clone)]
struct TelemetryContext {
    session_id: String,
    caller: String,
    agent_session_id: Option<String>,
    install_request_id: Option<String>,
    project_id: Option<String>,
    environment_id: Option<String>,
    service_id: Option<String>,
}

fn env_var_is_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn safe_telemetry_value(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > 256 {
        return None;
    }

    if value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'@' | b'/' | b'-'))
    {
        Some(value.to_string())
    } else {
        None
    }
}

fn safe_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .and_then(|value| safe_telemetry_value(value.trim()))
}

fn session_id() -> String {
    static SESSION_ID: OnceLock<String> = OnceLock::new();
    SESSION_ID
        .get_or_init(|| {
            let mut bytes = [0u8; 16];
            rand::thread_rng().fill_bytes(&mut bytes);
            format!("cli_{}", URL_SAFE_NO_PAD.encode(bytes))
        })
        .clone()
}

// ---------------------------------------------------------------------------
// Layer 1 — strong agent env signals (always-set, documented fingerprints)
// ---------------------------------------------------------------------------

/// Env variables that uniquely identify a specific agent harness. These are
/// set by the agent in every spawned subprocess and are documented in the
/// agent's source or official docs. High confidence — if any of these is
/// present, we trust it.
const STRONG_AGENT_ENV: &[(&str, &str)] = &[
    ("CLAUDECODE", "claude_code"),
    ("CLAUDE_CODE", "claude_code"),
    ("CLAUDE_CODE_SESSION_ID", "claude_code"),
    ("CLAUDE_CODE_ENTRYPOINT", "claude_code"),
    ("CURSOR_AGENT", "cursor"),
    ("CURSOR_TRACE_ID", "cursor"),
    ("CODEX_SANDBOX", "codex"),
    ("OPENAI_CODEX", "codex"),
    ("OPENCODE", "opencode"),
    ("OPENCODE_SESSION_ID", "opencode"),
    ("AMP_CURRENT_THREAD_ID", "amp"),
    ("AIDER", "aider"),
    ("COPILOT_AGENT_SESSION_ID", "copilot_cli"),
    ("COPILOT_CLI", "copilot_cli"),
    ("FACTORY_DROID", "factory_droid"),
    ("GEMINI_CLI", "gemini_cli"),
    ("REPLIT_AGENT", "replit_agent"),
    ("PI_CODING_AGENT", "pi"),
    ("__COG_BASHRC_SOURCED", "devin"),
];

fn agent_from_strong_env() -> Option<&'static str> {
    // `AGENT=amp` is what Sourcegraph Amp sets as a generic marker.
    if std::env::var("AGENT")
        .map(|value| value.eq_ignore_ascii_case("amp"))
        .unwrap_or(false)
    {
        return Some("amp");
    }

    // Match a strong env var by name only; presence (not value) is the signal.
    STRONG_AGENT_ENV
        .iter()
        .find_map(|(name, caller)| std::env::var(name).ok().map(|_| *caller))
        .or_else(|| {
            // `AI_AGENT` is set by Claude Code (e.g. `claude-code_2-1-133_agent`).
            std::env::var("AI_AGENT").ok().and_then(|value| {
                if value.contains("claude") {
                    Some("claude_code")
                } else {
                    None
                }
            })
        })
}

// ---------------------------------------------------------------------------
// Layer 2 — cloud-IDE / sandbox env signals
// ---------------------------------------------------------------------------

/// Cloud-hosted developer environments. Returns the canonical slug for the
/// platform. These do *not* identify which agent is driving — only the host
/// — so they are used as an `cloud_ide:<slug>` bucket when no stronger
/// agent signal is found.
fn cloud_ide_from_env() -> Option<&'static str> {
    if std::env::var("REPL_ID").is_ok() || std::env::var("REPLIT_USER").is_ok() {
        return Some("replit");
    }
    if env_var_is_truthy("CODESPACES") {
        return Some("codespaces");
    }
    if env_var_is_truthy("CLOUD_SHELL") || std::env::var("EDITOR_IN_CLOUD_SHELL").is_ok() {
        return Some("cloud_shell");
    }
    if std::env::var("MONOSPACE_ENV").is_ok() {
        return Some("firebase_studio");
    }
    if std::env::var("ANTIGRAVITY_CLI_ALIAS").is_ok() {
        return Some("antigravity");
    }
    None
}

// ---------------------------------------------------------------------------
// Layer 3 — process-tree inspection
// ---------------------------------------------------------------------------

/// Single process node from a `ps` snapshot or `/proc` walk.
#[derive(Clone, Debug)]
struct ProcessNode {
    ppid: u32,
    /// Full command line (argv joined). Falls back to the executable basename
    /// if the full argv is unavailable. Lower-cased for case-insensitive
    /// matching.
    command: String,
}

/// Walk the parent chain starting from `pid`, calling `f` on each node.
/// Stops when the callback returns `Some(_)`, when no parent is found, or
/// after `max_hops` iterations.
fn walk_ancestors<T, F>(
    snapshot: &HashMap<u32, ProcessNode>,
    pid: u32,
    max_hops: u32,
    mut f: F,
) -> Option<T>
where
    F: FnMut(&ProcessNode) -> Option<T>,
{
    let mut current = pid;
    for _ in 0..max_hops {
        let Some(node) = snapshot.get(&current) else {
            break;
        };
        if let Some(result) = f(node) {
            return Some(result);
        }
        if node.ppid == 0 || node.ppid == current {
            break;
        }
        current = node.ppid;
    }
    None
}

/// One-shot snapshot of the system process table. Spawning `ps` once and
/// building an in-memory map is significantly cheaper than calling `ps` at
/// every hop. Empty map on platforms or invocations where the snapshot is
/// unavailable; callers degrade gracefully (process-tree layer becomes a
/// no-op and we fall through to env-only detection).
fn process_snapshot() -> &'static HashMap<u32, ProcessNode> {
    static SNAPSHOT: OnceLock<HashMap<u32, ProcessNode>> = OnceLock::new();
    SNAPSHOT.get_or_init(|| {
        #[cfg(unix)]
        {
            build_unix_snapshot().unwrap_or_default()
        }
        #[cfg(not(unix))]
        {
            HashMap::new()
        }
    })
}

#[cfg(unix)]
fn build_unix_snapshot() -> Option<HashMap<u32, ProcessNode>> {
    // `ps -A -o pid=,ppid=,command=` is portable across macOS and Linux and
    // returns the full argv (not the truncated 15-char `comm`). On Linux
    // alternatives like `/proc/<pid>/cmdline` are faster but require N
    // syscalls per ancestor walk; the single-shot `ps` keeps the code
    // simple and one-and-done.
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut map = HashMap::new();
    for line in stdout.lines() {
        // `ps -o pid=,ppid=,command=` right-pads the pid/ppid columns with
        // spaces (variable width), so `split_whitespace` is the safe parser:
        // it collapses runs of whitespace and skips leading/trailing.
        let mut tokens = line.split_whitespace();
        let Some(pid) = tokens.next().and_then(|t| t.parse::<u32>().ok()) else {
            continue;
        };
        let Some(ppid) = tokens.next().and_then(|t| t.parse::<u32>().ok()) else {
            continue;
        };
        // Re-join the remaining tokens with single spaces. We only use the
        // result for substring matching, so collapsed whitespace is fine.
        let command: String = tokens.collect::<Vec<_>>().join(" ").to_ascii_lowercase();
        map.insert(pid, ProcessNode { ppid, command });
    }
    Some(map)
}

/// Map a process command line to a canonical agent slug. Operates on the
/// lowercased full command line so that node-bundled agents (e.g. `node
/// /path/to/cursor-agent`) match even though their `comm` is just `node`.
fn caller_from_process_name(command: &str) -> Option<&'static str> {
    let lower = command.to_ascii_lowercase();
    let argv0 = lower.split_whitespace().next().unwrap_or("");
    let basename = argv0.rsplit('/').next().unwrap_or("");

    // Short / generic agent names need exact-basename matching to avoid
    // false positives (`pi` vs `pilot`, `amp` vs `chamber`, `droid` vs
    // `android-tools`, etc.).
    match basename {
        "pi" => return Some("pi"),
        "amp" => return Some("amp"),
        "droid" => return Some("factory_droid"),
        _ => {}
    }

    // Distinctive substrings — these names are unique enough across the
    // ecosystem that anywhere they appear in argv (binary path, embedded
    // script, wrapper) is a positive match. Order matters: longer / more
    // specific patterns first.
    if lower.contains("cursor-agent") {
        return Some("cursor");
    }
    if lower.contains("opencode") {
        return Some("opencode");
    }
    if lower.contains("aider") {
        return Some("aider");
    }
    if lower.contains("replit") {
        return Some("replit_agent");
    }
    if lower.contains("copilot") {
        return Some("copilot_cli");
    }
    if lower.contains("gemini") {
        return Some("gemini_cli");
    }
    if lower.contains("qwen") {
        return Some("qwen_code");
    }
    if lower.contains("factory-droid") || lower.contains("factory_droid") {
        return Some("factory_droid");
    }
    // Tighter than bare "claude": bare-substring matched Claude Desktop
    // helper paths, MCP server binaries with "claude" in argv, scripts in
    // ~/.claude/, etc., over-attributing those invocations to claude_code.
    // The audit on 2026-05-10 confirmed claude_code dominated event counts
    // partly due to this. Match the known agent identifiers explicitly,
    // plus the literal `claude` argv0 (the official CLI entry point).
    if lower.contains("claude-code")
        || lower.contains("claude_code")
        || lower.contains("anthropic.claude-code")
        || basename == "claude"
    {
        return Some("claude_code");
    }
    if lower.contains("windsurf") {
        return Some("windsurf");
    }
    if lower.contains("cursor") {
        // Plain "cursor" without the agent suffix is typically the IDE
        // process (e.g. `Cursor Helper: terminal pty-host`); the caller
        // distinguishes user-vs-agent later via TTY check.
        return Some("cursor");
    }
    if lower.contains("pi-coding-agent") {
        return Some("pi");
    }
    if lower.contains("codex") {
        return Some("codex");
    }
    if lower.contains("goose") {
        return Some("goose");
    }
    if lower.contains("junie") {
        return Some("junie");
    }
    if lower.contains("cody") {
        return Some("cody");
    }
    None
}

/// Walk the process tree from the current PID upward looking for a
/// recognized agent binary in the ancestry. Up to 15 hops because some
/// agents introduce multiple wrapper layers (npm/npx/node/shell/agent).
fn agent_from_process_tree() -> Option<&'static str> {
    let snapshot = process_snapshot();
    if snapshot.is_empty() {
        return None;
    }
    walk_ancestors(snapshot, std::process::id(), 15, |node| {
        caller_from_process_name(&node.command)
    })
}

/// Categorize a generic interpreter / shell parent so unknown subprocess
/// callers can still tell us *what shape* of caller we're looking at
/// (Python vs Node vs Bash). Returns the canonical slug or None.
fn parent_kind_from_command(command: &str) -> Option<&'static str> {
    let name = command.to_ascii_lowercase();
    // Exact-binary / path-suffix matches to avoid false positives on
    // strings that incidentally contain the substring.
    let basename = name
        .split_whitespace()
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");
    match basename {
        "python" | "python2" | "python3" | "uv" | "pipx" => Some("python"),
        "node" | "deno" | "bun" | "npm" | "npx" | "pnpm" | "yarn" => Some("node"),
        "bash" | "sh" | "zsh" | "fish" | "dash" | "ksh" => Some("shell"),
        "ruby" | "irb" => Some("ruby"),
        "go" => Some("go"),
        "java" | "kotlin" | "scala" => Some("jvm"),
        "perl" => Some("perl"),
        "powershell" | "pwsh" | "cmd.exe" => Some("powershell"),
        _ => None,
    }
}

/// Best-effort categorization of the immediate parent process when no
/// known agent is found anywhere in the ancestry. Used to bucket the
/// `agent_unknown:<kind>` fallback so even unidentified harnesses give us
/// a useful axis (custom Python tooling vs Node-based agent vs raw shell
/// scripts).
fn parent_process_kind() -> Option<&'static str> {
    let snapshot = process_snapshot();
    let me = snapshot.get(&std::process::id())?;
    let parent = snapshot.get(&me.ppid)?;
    parent_kind_from_command(&parent.command)
}

// ---------------------------------------------------------------------------
// Layer 4 — CI provider detection
// ---------------------------------------------------------------------------

/// Match a known CI provider via stable env vars. Falls through (returns
/// None) for environments that only set the generic `CI=true` so we can
/// pick that up as the catch-all `ci` bucket without claiming a specific
/// provider we can't prove.
fn ci_provider_from_env() -> Option<&'static str> {
    if env_var_is_truthy("GITHUB_ACTIONS") {
        return Some("github_actions");
    }
    if env_var_is_truthy("GITLAB_CI") {
        return Some("gitlab");
    }
    if env_var_is_truthy("CIRCLECI") {
        return Some("circle");
    }
    if env_var_is_truthy("BUILDKITE") {
        return Some("buildkite");
    }
    if std::env::var("JENKINS_URL").is_ok() {
        return Some("jenkins");
    }
    if env_var_is_truthy("TRAVIS") {
        return Some("travis");
    }
    if std::env::var("TEAMCITY_VERSION").is_ok() {
        return Some("teamcity");
    }
    if env_var_is_truthy("TF_BUILD") {
        return Some("azure_pipelines");
    }
    if std::env::var("BITBUCKET_BUILD_NUMBER").is_ok() {
        return Some("bitbucket");
    }
    if env_var_is_truthy("DRONE") {
        return Some("drone");
    }
    if env_var_is_truthy("SEMAPHORE") {
        return Some("semaphore");
    }
    if std::env::var("CODEBUILD_BUILD_ID").is_ok() {
        return Some("codebuild");
    }
    if env_var_is_truthy("NETLIFY") {
        return Some("netlify");
    }
    if env_var_is_truthy("VERCEL") {
        return Some("vercel");
    }
    if std::env::var("RAILWAY_ENVIRONMENT_ID").is_ok()
        || std::env::var("RAILWAY_PROJECT_ID").is_ok()
    {
        return Some("railway");
    }
    None
}

// ---------------------------------------------------------------------------
// Layer 5 — AI-IDE host detection (TTY-side discriminator)
// ---------------------------------------------------------------------------

/// Detect the host IDE/terminal regardless of whether a human or an agent
/// is driving. Used in combination with the TTY check to produce
/// `tty:<ide>` (human in IDE terminal) or `agent_unknown:<ide>`
/// (subprocess inside that IDE with no agent fingerprint).
fn ai_ide_host_from_env() -> Option<&'static str> {
    // macOS `__CFBundleIdentifier` is the most authoritative discriminator
    // among VS Code / Cursor / Windsurf / Zed / Claude Desktop, since they
    // all share `TERM_PROGRAM=vscode` (or similar) but each has a unique
    // bundle ID.
    if let Ok(bundle) = std::env::var("__CFBundleIdentifier") {
        let b = bundle.to_ascii_lowercase();
        if b.contains("todesktop") || b.contains("cursor") {
            return Some("cursor");
        }
        if b.contains("exafunction.windsurf") || b.contains("windsurf") {
            return Some("windsurf");
        }
        if b.contains("vscodeinsiders") {
            return Some("vscode_insiders");
        }
        if b.contains("microsoft.vscode") || b.contains("visualstudio.code") {
            return Some("vscode");
        }
        if b.contains("dev.zed.zed") || b.starts_with("dev.zed") {
            return Some("zed");
        }
        if b.contains("anthropic.claude") {
            return Some("claude_desktop");
        }
        if b.contains("jetbrains")
            || b.contains("intellij")
            || b.contains("pycharm")
            || b.contains("webstorm")
            || b.contains("goland")
            || b.contains("clion")
            || b.contains("rustrover")
            || b.contains("datagrip")
            || b.contains("phpstorm")
            || b.contains("rider")
        {
            return Some("jetbrains");
        }
    }

    // JetBrains' own canonical signal (cross-platform).
    if let Ok(emu) = std::env::var("TERMINAL_EMULATOR") {
        if emu.to_ascii_lowercase().contains("jetbrains") {
            return Some("jetbrains");
        }
    }

    // Cursor sets `CURSOR_TRACE_ID` in every Cursor process; Layer 1 catches
    // this for agent contexts but it also fires in plain Cursor terminals.
    if std::env::var("CURSOR_TRACE_ID").is_ok() {
        return Some("cursor");
    }

    if env_var_is_truthy("POSITRON") {
        return Some("positron");
    }

    if let Ok(prod) = std::env::var("TERM_PRODUCT") {
        if prod.eq_ignore_ascii_case("trae") {
            return Some("trae");
        }
    }

    if std::env::var("ZED_SESSION_ID").is_ok() {
        return Some("zed");
    }

    if std::env::var("XCODE_VERSION_ACTUAL").is_ok() {
        return Some("xcode");
    }

    // Generic VS Code-family signal. Specific fork is unknown without the
    // bundle ID, so we tag it `vscode` (covers VS Code stable, Cursor,
    // Windsurf, and other forks running outside macOS where bundle ID is
    // unavailable).
    if let Ok(prog) = std::env::var("TERM_PROGRAM") {
        let p = prog.to_ascii_lowercase();
        if p == "vscode" {
            return Some("vscode");
        }
        if p == "cursor" {
            return Some("cursor");
        }
        if p == "zed" {
            return Some("zed");
        }
        if p == "warpterminal" {
            return Some("warp");
        }
        if p == "ghostty" {
            return Some("ghostty");
        }
        if p == "iterm.app" {
            return Some("iterm");
        }
        if p == "apple_terminal" {
            return Some("apple_terminal");
        }
        if p.starts_with("sublime") {
            return Some("sublime");
        }
    }

    // Fallback: VS Code-family environment variables (set by both VS Code
    // and its forks even when TERM_PROGRAM has been overridden by tooling).
    if std::env::var("VSCODE_PID").is_ok()
        || std::env::var("VSCODE_INJECTION").is_ok()
        || std::env::var("VSCODE_GIT_IPC_HANDLE").is_ok()
    {
        return Some("vscode");
    }

    None
}

// ---------------------------------------------------------------------------
// Layer 6 — MCP client info (authoritative for MCP path)
// ---------------------------------------------------------------------------

/// Map an MCP `clientInfo.name` value (sent verbatim by the client during
/// the JSON-RPC `initialize` handshake) to our canonical caller slug. This
/// is the strongest signal we have for any MCP-driven event because the
/// client identifies itself explicitly per the MCP spec.
fn caller_from_mcp_client_name(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    // Order: longer / more specific patterns first.
    if lower == "claude-ai" {
        // Claude Desktop and Claude Code both report `claude-ai`. Disambiguate
        // by checking the env: Claude Code sets `CLAUDECODE=1` in the spawned
        // MCP server's environment, Claude Desktop does not.
        if env_var_is_truthy("CLAUDECODE") || std::env::var("CLAUDE_CODE_SESSION_ID").is_ok() {
            return "claude_code";
        }
        return "claude_desktop";
    }
    if lower == "codex-mcp-client" || lower.contains("codex") {
        return "codex";
    }
    if lower == "cline" {
        return "cline";
    }
    if lower == "roo code" || lower == "roo-code" {
        return "roo_code";
    }
    if lower == "kilo" || lower.starts_with("kilo") {
        return "kilo_code";
    }
    if lower == "opencode" {
        return "opencode";
    }
    if lower == "continue-client" || lower.contains("continue") {
        return "continue_dev";
    }
    if lower.starts_with("visual studio code") {
        if lower.contains("insiders") {
            return "vscode_insiders";
        }
        return "vscode_copilot";
    }
    if lower.contains("windsurf") {
        return "windsurf";
    }
    if lower.contains("cursor") {
        return "cursor";
    }
    if lower.contains("goose") {
        return "goose";
    }
    if lower.contains("firebender") {
        return "firebender";
    }
    if lower.contains("gemini") {
        return "gemini_cli";
    }
    if lower.contains("zed") {
        return "zed_agent";
    }
    if lower.contains("jetbrains") || lower.contains("intellij") {
        return "jetbrains_ai";
    }
    // Unrecognized — surface the raw client name (lowercased, sanitized) so
    // we can iterate without re-shipping the CLI when new clients appear.
    "mcp_unknown"
}

// ---------------------------------------------------------------------------
// Persistent agent session file
// ---------------------------------------------------------------------------
//
// Recovers a stable `agent_session_id` across `railway` invocations when no
// harness env var (RAILWAY_AGENT_SESSION, CLAUDE_CODE_SESSION_ID, ...) is
// present. Keyed by the immediate parent process identity (pid + boot time +
// argv0) so concurrent agents and shells get separate sessions automatically.
//
// File layout: `~/.railway/sessions/<16-hex-of-sha256>.session` containing a
// small JSON blob (id + parent invariants + creation timestamp).
//
// Lifecycle:
//   - Created on first agent-attributed CLI invocation with a recognizable
//     parent process.
//   - Reused as long as the parent pid is still alive and its boot time
//     matches the persisted value (defends against PID reuse).
//   - Hard age cap of 7 days as a backstop against very-long-lived parents
//     or boot-time detection drift.
//   - Cleaned opportunistically on every CLI invocation: stale files
//     (parent gone, btime mismatch, age cap exceeded) are deleted; the
//     directory is also capped at SESSION_DIR_FILE_LIMIT files with oldest-
//     by-mtime evicted first.

const SESSION_FILE_MAX_AGE_DAYS: i64 = 7;
const SESSION_DIR_FILE_LIMIT: usize = 100;

#[derive(Serialize, Deserialize)]
struct PersistedSession {
    agent_session_id: String,
    parent_pid: u32,
    parent_btime: u64,
    created_at: String,
}

struct ParentIdentity {
    pid: u32,
    btime: u64,
    argv0: String,
}

fn sessions_dir() -> Option<PathBuf> {
    if let Ok(override_dir) = std::env::var("RAILWAY_SESSIONS_DIR") {
        return Some(PathBuf::from(override_dir));
    }
    dirs::home_dir().map(|h| h.join(".railway/sessions"))
}

fn parent_identity() -> Option<ParentIdentity> {
    let snapshot = process_snapshot();
    if snapshot.is_empty() {
        return None;
    }
    let me = snapshot.get(&std::process::id())?;
    if me.ppid == 0 {
        return None;
    }
    let parent = snapshot.get(&me.ppid)?;
    let argv0 = parent
        .command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    let btime = parent_boot_time(me.ppid)?;
    Some(ParentIdentity {
        pid: me.ppid,
        btime,
        argv0,
    })
}

#[cfg(target_os = "linux")]
fn parent_boot_time(pid: u32) -> Option<u64> {
    // /proc/<pid>/stat field 22 (1-indexed) is starttime in clock ticks
    // since system boot. The comm field at position 2 is parenthesized and
    // may contain spaces, so we slice from the last `)` to avoid tokenizing
    // through it.
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    let rest = &stat[close + 2..];
    // After comm, fields are: state, ppid, pgrp, session, tty_nr, tpgid,
    // flags, minflt, cminflt, majflt, cmajflt, utime, stime, cutime,
    // cstime, priority, nice, num_threads, itrealvalue, starttime
    // → starttime is index 19 in the post-comm tokens.
    rest.split_whitespace().nth(19)?.parse::<u64>().ok()
}

#[cfg(target_os = "macos")]
fn parent_boot_time(pid: u32) -> Option<u64> {
    // macOS doesn't expose start time via /proc; use `ps -o lstart=` which
    // prints a stable per-process start timestamp. Hash it to a u64 so the
    // on-disk schema doesn't depend on the textual format.
    let out = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return None;
    }
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let bytes = h.finalize();
    let mut out8 = [0u8; 8];
    out8.copy_from_slice(&bytes[..8]);
    Some(u64::from_be_bytes(out8))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn parent_boot_time(_pid: u32) -> Option<u64> {
    None
}

fn session_filename(parent: &ParentIdentity) -> String {
    let mut h = Sha256::new();
    h.update(parent.pid.to_be_bytes());
    h.update(parent.btime.to_be_bytes());
    h.update(parent.argv0.as_bytes());
    let bytes = h.finalize();
    let mut hex = String::with_capacity(16);
    for byte in &bytes[..8] {
        hex.push_str(&format!("{:02x}", byte));
    }
    format!("{hex}.session")
}

/// Generate a v4 UUID without pulling in the `uuid` crate.
fn new_session_uuid() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    // Set version (4) and variant (RFC 4122) bits per UUID v4 spec.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

fn is_session_too_old(created_at: &str) -> bool {
    match chrono::DateTime::parse_from_rfc3339(created_at) {
        Ok(t) => {
            let age = chrono::Utc::now().signed_duration_since(t.with_timezone(&chrono::Utc));
            age > chrono::Duration::days(SESSION_FILE_MAX_AGE_DAYS)
        }
        Err(_) => true,
    }
}

fn read_persisted_session(path: &std::path::Path, parent: &ParentIdentity) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    let persisted: PersistedSession = serde_json::from_str(&contents).ok()?;
    if persisted.parent_pid != parent.pid {
        return None;
    }
    if persisted.parent_btime != parent.btime {
        return None;
    }
    if is_session_too_old(&persisted.created_at) {
        return None;
    }
    safe_telemetry_value(&persisted.agent_session_id)
}

fn write_persisted_session(
    path: &std::path::Path,
    parent: &ParentIdentity,
    id: &str,
) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("create sessions dir")?;
    }
    let payload = PersistedSession {
        agent_session_id: id.to_string(),
        parent_pid: parent.pid,
        parent_btime: parent.btime,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let json = serde_json::to_string(&payload)?;
    crate::util::write_atomic(path, &json)
}

/// Best-effort cleanup: drop session files whose parent is gone or whose
/// recorded invariants don't match a live process, plus a defense-in-depth
/// directory size cap. Silent on errors — telemetry should never fail loud.
fn cleanup_stale_sessions(dir: &std::path::Path) {
    let snapshot = process_snapshot();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut surviving: Vec<(PathBuf, std::time::SystemTime)> = vec![];
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("session") {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::UNIX_EPOCH);
        let contents = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                let _ = fs::remove_file(&path);
                continue;
            }
        };
        let persisted: PersistedSession = match serde_json::from_str(&contents) {
            Ok(p) => p,
            Err(_) => {
                let _ = fs::remove_file(&path);
                continue;
            }
        };
        let parent_alive = snapshot.get(&persisted.parent_pid).is_some();
        let too_old = is_session_too_old(&persisted.created_at);
        if !parent_alive || too_old {
            let _ = fs::remove_file(&path);
        } else {
            surviving.push((path, mtime));
        }
    }
    if surviving.len() > SESSION_DIR_FILE_LIMIT {
        surviving.sort_by_key(|(_, mtime)| *mtime);
        let excess = surviving.len() - SESSION_DIR_FILE_LIMIT;
        for (path, _) in surviving.iter().take(excess) {
            let _ = fs::remove_file(path);
        }
    }
}

/// Read or mint a persistent agent session ID for the current parent process.
/// Cached for the lifetime of the CLI invocation. Returns None when parent
/// identity can't be determined (non-unix, missing /proc, etc.); callers
/// fall back to the per-process `cli_<base64>` mint.
fn persistent_agent_session_id() -> Option<String> {
    static CACHED: OnceLock<Option<String>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let dir = sessions_dir()?;
            let parent = parent_identity()?;
            let path = dir.join(session_filename(&parent));

            // Opportunistic cleanup — runs once per CLI invocation.
            cleanup_stale_sessions(&dir);

            if let Some(id) = read_persisted_session(&path, &parent) {
                return Some(id);
            }
            let id = new_session_uuid();
            let _ = write_persisted_session(&path, &parent, &id);
            Some(id)
        })
        .clone()
}

// ---------------------------------------------------------------------------
// Composer — main detection entry point
// ---------------------------------------------------------------------------

/// Compute the caller bucket for this CLI invocation. Cached for the
/// lifetime of the process so repeated calls are free and stable across a
/// session (important for long-lived processes like the local MCP server,
/// where the same caller value is reported across many tool events).
fn detect_caller() -> String {
    static CALLER: OnceLock<String> = OnceLock::new();
    CALLER.get_or_init(detect_caller_uncached).clone()
}

fn detect_caller_uncached() -> String {
    // 1. Explicit user override — always wins.
    if let Some(v) = safe_env(RAILWAY_CALLER_ENV) {
        return v;
    }

    // 2. Strong agent env signal (CLAUDECODE, CURSOR_AGENT, PI_CODING_AGENT,
    //    __COG_BASHRC_SOURCED, etc.). This wins over both process-tree and
    //    cloud-IDE detection because an agent running inside Codespaces is
    //    still that agent — the IDE host is a less interesting axis.
    if let Some(agent) = agent_from_strong_env() {
        return agent.to_string();
    }

    // 3. Process-tree walk. Catches CLIs whose env we can't fingerprint
    //    (Codex, Goose, Aider, Junie, OpenCode in older versions, ...).
    if let Some(agent) = agent_from_process_tree() {
        return agent.to_string();
    }

    let interactive = std::io::stdout().is_terminal();

    // 4. AI-IDE host detection. Combined with the TTY check, this gives us
    //    `tty:<ide>` (human in IDE terminal — axis for "AI IDE adoption")
    //    or `agent_unknown:<ide>` (subprocess inside that IDE with no
    //    agent fingerprint — likely an agent extension we don't yet
    //    catalog, e.g. Cline / Roo Code / Continue running inside VS Code
    //    when we couldn't grab the MCP clientInfo or env).
    if let Some(ide) = ai_ide_host_from_env() {
        if interactive {
            return format!("tty:{}", ide);
        }
        return format!("agent_unknown:{}", ide);
    }

    // 5. Cloud IDE / sandbox env — same shape as the IDE-host bucket but
    //    the host is a remote workspace rather than a local Electron app.
    if let Some(host) = cloud_ide_from_env() {
        if interactive {
            return format!("tty:{}", host);
        }
        return format!("cloud_ide:{}", host);
    }

    // 6. CI provider — only after we've ruled out interactive use, since
    //    several agents set `CI=true` to suppress prompts.
    if !interactive {
        if let Some(provider) = ci_provider_from_env() {
            return format!("ci:{}", provider);
        }
        if Configs::env_is_ci() {
            return "ci".to_string();
        }
    }

    // 7. Final fallback. Interactive shell with no IDE → plain `tty`.
    //    Subprocess with no agent / IDE / CI fingerprint → bucket by the
    //    immediate parent's interpreter kind so we can distinguish
    //    "Python script driving us" from "Node tooling" from "raw shell
    //    pipeline" without claiming knowledge we don't have.
    if interactive {
        return "tty".to_string();
    }
    if let Some(kind) = parent_process_kind() {
        return format!("agent_unknown:{}", kind);
    }
    "agent_unknown".to_string()
}

/// True when the caller represents agentic / automated invocation rather
/// than a human at a terminal. Drives `agent_session_id` synthesis: agent
/// callers without an explicit `RAILWAY_AGENT_SESSION` env get the local
/// session ID so events from one CLI invocation correlate downstream.
fn is_agent_caller(caller: &str) -> bool {
    // Plain human terminals (including humans typing in IDE terminals — we
    // want those NOT to synthesize an agent_session_id, since they aren't
    // part of an agent loop).
    if caller == "tty" || caller.starts_with("tty:") {
        return false;
    }
    // CI is automation but not agentic; `is_ci=true` already records it.
    if caller == "ci" || caller.starts_with("ci:") {
        return false;
    }
    // Cloud IDE host with no agent identified (interactive caught above
    // returns `tty:<host>`; the `cloud_ide:<host>` branch only fires for
    // non-interactive subprocess use, which is agent-like).
    true
}

fn error_class(message: Option<&str>) -> String {
    let Some(message) = message else {
        return "UNKNOWN".to_string();
    };

    let message = message.to_ascii_lowercase();
    let class = if message.contains("not authorized")
        || message.contains("unauthorized")
        || message.contains("forbidden")
        || message.contains("access denied")
    {
        "AUTHORIZATION"
    } else if message.contains("login")
        || message.contains("authenticated")
        || message.contains("authentication")
        || message.contains("token")
    {
        "AUTHENTICATION"
    } else if message.contains("not found") || message.contains("no linked project") {
        "NOT_FOUND"
    } else if message.contains("invalid")
        || message.contains("required")
        || message.contains("must")
    {
        "VALIDATION"
    } else if message.contains("rate limit") || message.contains("ratelimit") {
        "RATE_LIMITED"
    } else if message.contains("timeout") || message.contains("timed out") {
        "TIMEOUT"
    } else {
        "UNKNOWN"
    };

    class.to_string()
}

impl TelemetryContext {
    fn current(configs: &Configs) -> Self {
        Self::current_with_caller(configs, None)
    }

    /// Build the per-event telemetry context, optionally overriding the
    /// detected caller. The override is used by the MCP path to substitute
    /// the JSON-RPC `clientInfo`-derived caller, which is authoritative for
    /// MCP-driven events and supersedes any env/process-tree detection.
    fn current_with_caller(configs: &Configs, caller_override: Option<String>) -> Self {
        let session_id = session_id();
        let caller = caller_override
            .and_then(|c| safe_telemetry_value(&c))
            .or_else(|| MCP_CLIENT_CALLER.get().cloned())
            .unwrap_or_else(detect_caller);
        let linked_project = configs.get_local_linked_project().ok();
        let agent_session_id = safe_env(RAILWAY_AGENT_SESSION_ENV)
            .or_else(|| safe_env("COPILOT_AGENT_SESSION_ID"))
            .or_else(|| safe_env("CLAUDE_CODE_SESSION_ID"))
            // Verified 2026-05-11 via live env capture: Codex exports
            // CODEX_THREAD_ID as a UUID v7 in every spawned bash; previous
            // CODEX_SESSION_ID guess did not exist in the env at all (which
            // matched the 100% per-process fallback observed in the warehouse).
            .or_else(|| safe_env("CODEX_THREAD_ID"))
            // Verified 2026-05-11 via live env capture: OpenCode exports
            // OPENCODE_RUN_ID as a UUID v4. The previous OPENCODE_SESSION_ID
            // entry was checking for a variable that does not exist, which
            // matched the ~100% per-process fallback observed for opencode.
            .or_else(|| safe_env("OPENCODE_RUN_ID"))
            // Verified 2026-05-11 via a live interactive Amp session.
            .or_else(|| safe_env("AMP_CURRENT_THREAD_ID"))
            // Verified 2026-05-11: Cursor does NOT propagate a session
            // identifier into its bash tool — only CURSOR_AGENT=1 (presence
            // flag, used for caller detection) and CURSOR_SANDBOX metadata.
            // CURSOR_TRACE_ID is documented for the IDE but does not appear
            // in the agent's spawned subprocess env. Kept here as a no-cost
            // forward-compat hook in case Cursor adds propagation later.
            .or_else(|| safe_env("CURSOR_TRACE_ID"))
            // Cross-agent convention exposed by Amp (verified) and observed
            // in some other harnesses' docs. Late in the precedence chain
            // so harness-specific IDs win when both are set; catches any
            // future agent that adopts this generic name.
            .or_else(|| safe_env("AGENT_THREAD_ID"))
            .or_else(|| {
                if is_agent_caller(&caller) {
                    // Try the persistent session-file fallback first. It
                    // recovers a stable UUID across `railway` invocations
                    // spawned by the same parent process when the harness
                    // doesn't propagate its session env var (e.g. when an
                    // agent's bash tool doesn't whitelist its own session
                    // ID). Falls through to the per-process mint when
                    // parent identity can't be determined (non-unix, etc.).
                    persistent_agent_session_id().or(Some(session_id.clone()))
                } else {
                    None
                }
            });

        Self {
            session_id,
            caller,
            agent_session_id,
            install_request_id: safe_env(RAILWAY_INSTALL_REQUEST_ID_ENV),
            project_id: Configs::get_railway_project_id()
                .and_then(|id| safe_telemetry_value(&id))
                .or_else(|| {
                    linked_project
                        .as_ref()
                        .and_then(|p| safe_telemetry_value(&p.project))
                }),
            environment_id: Configs::get_railway_environment_id()
                .and_then(|id| safe_telemetry_value(&id))
                .or_else(|| {
                    linked_project
                        .as_ref()
                        .and_then(|p| p.environment.as_deref())
                        .and_then(safe_telemetry_value)
                }),
            service_id: Configs::get_railway_service_id()
                .and_then(|id| safe_telemetry_value(&id))
                .or_else(|| {
                    linked_project
                        .as_ref()
                        .and_then(|p| p.service.as_deref())
                        .and_then(safe_telemetry_value)
                }),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    #[serde(default)]
    pub telemetry_disabled: bool,
    #[serde(default)]
    pub auto_update_disabled: bool,
}

impl Preferences {
    fn path() -> Option<std::path::PathBuf> {
        dirs::home_dir().map(|h| h.join(".railway/preferences.json"))
    }

    pub fn read() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn write(&self) -> anyhow::Result<()> {
        let path = Self::path().context("Failed to determine home directory")?;
        let contents = serde_json::to_string(self)?;
        crate::util::write_atomic(&path, &contents)
    }
}

pub fn is_telemetry_disabled_by_env() -> bool {
    env_var_is_truthy("DO_NOT_TRACK") || env_var_is_truthy("RAILWAY_NO_TELEMETRY")
}

pub fn is_auto_update_disabled_by_env() -> bool {
    env_var_is_truthy("RAILWAY_NO_AUTO_UPDATE")
}

pub fn is_auto_update_disabled() -> bool {
    is_auto_update_disabled_by_env()
        || Preferences::read().auto_update_disabled
        || crate::config::Configs::env_is_ci()
}

fn is_telemetry_disabled() -> bool {
    is_telemetry_disabled_by_env() || Preferences::read().telemetry_disabled
}

async fn post_telemetry_body(client: &reqwest::Client, url: String, body: Value) -> bool {
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), async move {
        let response = client.post(url).json(&body).send().await?;
        if !response.status().is_success() {
            return Ok::<bool, reqwest::Error>(false);
        }

        let response_body: Value = response.json().await?;
        Ok(response_body.get("errors").is_none())
    })
    .await;

    matches!(result, Ok(Ok(true)))
}

pub async fn send(event: CliTrackEvent) {
    send_with_caller_override(event, None).await;
}

async fn send_with_caller_override(event: CliTrackEvent, caller_override: Option<String>) {
    if is_telemetry_disabled() {
        return;
    }

    let configs = match Configs::new() {
        Ok(c) => c,
        Err(_) => return,
    };

    let client = match GQLClient::new_authorized(&configs) {
        Ok(c) => c,
        Err(_) => return,
    };

    let context = TelemetryContext::current_with_caller(&configs, caller_override);
    let error_class = if event.success {
        None
    } else {
        Some(error_class(event.error_message.as_deref()))
    };
    let input = CliEventTrackInput {
        command: event.command.clone(),
        sub_command: event.sub_command.clone(),
        duration_ms: event.duration_ms as i64,
        success: event.success,
        error_message: event.error_message.clone(),
        os: event.os.to_string(),
        arch: event.arch.to_string(),
        cli_version: event.cli_version.to_string(),
        is_ci: event.is_ci,
        session_id: context.session_id,
        caller: context.caller,
        agent_session_id: context.agent_session_id,
        install_request_id: context.install_request_id,
        project_id: context.project_id,
        environment_id: context.environment_id,
        service_id: context.service_id,
        error_class,
    };

    let body = json!({
        "query": "mutation CliEventTrack($input: CliEventTrackInput!) { cliEventTrack(input: $input) }",
        "variables": { "input": input },
    });

    if !post_telemetry_body(&client, configs.get_backboard(), body).await {
        let legacy_input = LegacyCliEventTrackInput {
            command: event.command,
            sub_command: event.sub_command,
            duration_ms: event.duration_ms as i64,
            success: event.success,
            error_message: event.error_message,
            os: event.os.to_string(),
            arch: event.arch.to_string(),
            cli_version: event.cli_version.to_string(),
            is_ci: event.is_ci,
        };
        let legacy_body = json!({
            "query": "mutation CliEventTrack($input: CliEventTrackInput!) { cliEventTrack(input: $input) }",
            "variables": { "input": legacy_input },
        });

        let _ = post_telemetry_body(&client, configs.get_backboard(), legacy_body).await;
    }
}

/// Process-scoped MCP client caller, captured the first time a tool call
/// arrives with `clientInfo`. Lets the server-lifecycle telemetry::send
/// emitted by the dispatch macro at process exit attribute itself to the
/// MCP client even though no `clientInfo` is in scope at that point.
static MCP_CLIENT_CALLER: OnceLock<String> = OnceLock::new();

fn record_mcp_client_caller(client: &McpClientInfo) {
    let _ = MCP_CLIENT_CALLER.set(caller_from_mcp_client_name(&client.name).to_string());
}

/// Send MCP tool telemetry. The caller is derived from the JSON-RPC
/// `clientInfo` when provided (authoritative per the MCP spec) and falls back
/// to env/process-tree detection otherwise.
pub async fn send_mcp_tool_with_client(
    tool_name: String,
    duration_ms: u64,
    success: bool,
    error_message: Option<String>,
    mcp_client: Option<McpClientInfo>,
) {
    if let Some(c) = mcp_client.as_ref() {
        record_mcp_client_caller(c);
    }
    let caller_override = mcp_client
        .as_ref()
        .map(|c| caller_from_mcp_client_name(&c.name).to_string());
    send_with_caller_override(
        CliTrackEvent {
            command: "mcp".to_string(),
            sub_command: Some(tool_name),
            duration_ms,
            success,
            error_message,
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            cli_version: env!("CARGO_PKG_VERSION"),
            is_ci: Configs::env_is_ci(),
        },
        caller_override,
    )
    .await;
}

pub async fn send_setup_agent(event: SetupAgentTrackEvent) {
    if is_telemetry_disabled() {
        return;
    }

    let configs = match Configs::new() {
        Ok(c) => c,
        Err(_) => return,
    };

    let client = GQLClient::new_authorized(&configs)
        .or_else(|_| GQLClient::new_public())
        .ok();
    let Some(client) = client else {
        return;
    };

    let context = TelemetryContext::current(&configs);
    let input = SetupAgentEventTrackInput {
        phase: match event.phase {
            SetupAgentPhase::Start => "start",
            SetupAgentPhase::Finish => "finish",
        },
        success: event.success,
        error_message: event.error_message,
        configured_clients: event.configured_clients,
        session_id: context.session_id,
        caller: context.caller,
        agent_session_id: context.agent_session_id,
        install_request_id: context.install_request_id,
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        is_ci: Configs::env_is_ci(),
    };

    let body = json!({
        "query": "mutation SetupAgentEventTrack($input: SetupAgentEventTrackInput!) { setupAgentEventTrack(input: $input) }",
        "variables": { "input": input },
    });

    let _ = post_telemetry_body(&client, configs.get_backboard(), body).await;
}

#[cfg(test)]
mod tests {
    use super::{
        caller_from_mcp_client_name, caller_from_process_name, is_agent_caller, new_session_uuid,
        parent_kind_from_command,
    };

    #[test]
    fn detects_pi_process_name() {
        assert_eq!(caller_from_process_name("pi"), Some("pi"));
        assert_eq!(caller_from_process_name("/usr/local/bin/pi"), Some("pi"));
        assert_eq!(
            caller_from_process_name("node /opt/pi-coding-agent/main.js"),
            Some("pi")
        );
    }

    #[test]
    fn detects_amp_process_name() {
        assert_eq!(caller_from_process_name("amp"), Some("amp"));
        assert_eq!(caller_from_process_name("/usr/local/bin/amp"), Some("amp"));
    }

    #[test]
    fn detects_aider_process_name() {
        assert_eq!(caller_from_process_name("aider"), Some("aider"));
        assert_eq!(
            caller_from_process_name("/usr/local/bin/aider --yes"),
            Some("aider")
        );
    }

    #[test]
    fn detects_replit_process_name() {
        assert_eq!(
            caller_from_process_name("replit-agent"),
            Some("replit_agent")
        );
        assert_eq!(
            caller_from_process_name("/usr/local/bin/replit"),
            Some("replit_agent")
        );
    }

    #[test]
    fn detects_copilot_process_name() {
        assert_eq!(caller_from_process_name("copilot"), Some("copilot_cli"));
        assert_eq!(
            caller_from_process_name("/usr/local/bin/copilot"),
            Some("copilot_cli")
        );
    }

    #[test]
    fn detects_gemini_process_name() {
        assert_eq!(caller_from_process_name("gemini"), Some("gemini_cli"));
        assert_eq!(
            caller_from_process_name("node /usr/local/bin/gemini-cli/index.js"),
            Some("gemini_cli")
        );
    }

    #[test]
    fn detects_factory_droid_process_name() {
        assert_eq!(
            caller_from_process_name("factory-droid"),
            Some("factory_droid")
        );
        assert_eq!(
            caller_from_process_name("/usr/local/bin/factory_droid"),
            Some("factory_droid")
        );
        assert_eq!(caller_from_process_name("droid run"), Some("factory_droid"));
    }

    #[test]
    fn detects_codex_via_full_argv() {
        // macOS `comm` would be just `codex`; full argv carries more context.
        assert_eq!(caller_from_process_name("codex --remote"), Some("codex"));
        assert_eq!(
            caller_from_process_name("/usr/local/bin/codex"),
            Some("codex")
        );
    }

    #[test]
    fn detects_node_bundled_agents_via_full_argv() {
        // Cursor agent and similar node-bundled agents have `comm=node` but
        // the full argv carries the agent path. The full-cmdline matching
        // that drove this redesign catches them.
        assert_eq!(
            caller_from_process_name("node /Applications/Cursor.app/.../cursor-agent"),
            Some("cursor")
        );
        assert_eq!(
            caller_from_process_name("/Users/x/.opencode/bin/opencode start"),
            Some("opencode")
        );
        assert_eq!(
            caller_from_process_name("node /usr/local/lib/claude-code/cli.js"),
            Some("claude_code")
        );
    }

    #[test]
    fn does_not_detect_short_agent_names_as_substrings() {
        assert_eq!(caller_from_process_name("pilot"), None);
        assert_eq!(caller_from_process_name("example"), None);
    }

    #[test]
    fn maps_mcp_client_info_to_caller() {
        // claude-ai disambiguation depends on env (CLAUDECODE etc.). This
        // test pins the subset that doesn't need env state.
        assert_eq!(caller_from_mcp_client_name("codex-mcp-client"), "codex");
        assert_eq!(caller_from_mcp_client_name("Cline"), "cline");
        assert_eq!(caller_from_mcp_client_name("Roo Code"), "roo_code");
        assert_eq!(caller_from_mcp_client_name("kilo"), "kilo_code");
        assert_eq!(caller_from_mcp_client_name("opencode"), "opencode");
        assert_eq!(
            caller_from_mcp_client_name("continue-client"),
            "continue_dev"
        );
        assert_eq!(
            caller_from_mcp_client_name("Visual Studio Code"),
            "vscode_copilot"
        );
        assert_eq!(
            caller_from_mcp_client_name("Visual Studio Code - Insiders"),
            "vscode_insiders"
        );
        assert_eq!(caller_from_mcp_client_name("cursor-vscode"), "cursor");
        assert_eq!(caller_from_mcp_client_name("Windsurf"), "windsurf");
        assert_eq!(caller_from_mcp_client_name("goose"), "goose");
        assert_eq!(caller_from_mcp_client_name("firebender"), "firebender");
        assert_eq!(
            caller_from_mcp_client_name("totally-unknown-client"),
            "mcp_unknown"
        );
    }

    #[test]
    fn parent_kind_buckets_known_interpreters() {
        assert_eq!(
            parent_kind_from_command("python3 deploy.py"),
            Some("python")
        );
        assert_eq!(
            parent_kind_from_command("/usr/bin/uv run script"),
            Some("python")
        );
        assert_eq!(parent_kind_from_command("node deploy.js"), Some("node"));
        assert_eq!(parent_kind_from_command("npx some-tool"), Some("node"));
        assert_eq!(
            parent_kind_from_command("bash -c 'do stuff'"),
            Some("shell")
        );
        assert_eq!(parent_kind_from_command("zsh"), Some("shell"));
        assert_eq!(parent_kind_from_command("ruby script.rb"), Some("ruby"));
        assert_eq!(parent_kind_from_command("pwsh"), Some("powershell"));
        assert_eq!(parent_kind_from_command("/usr/bin/unknown-binary"), None);
    }

    #[cfg(unix)]
    #[test]
    fn parses_real_ps_snapshot() {
        // Sample lines from `ps -A -o pid=,ppid=,command=` on macOS, with
        // the variable-width right-padding the parser has to tolerate.
        let sample = "\
  4901     1 /Applications/Cursor.app/Contents/Frameworks/Electron Framework.framework/Helpers/chrome_crashpad_handler --no-rate-limit
   220 99993 claude --dangerously-skip-permissions
99993 99992 -/bin/zsh
\n";
        // Re-implement the parser inline to keep this test pure (no
        // process-spawning) while still exercising the exact loop body.
        let mut map = std::collections::HashMap::new();
        for line in sample.lines() {
            let mut tokens = line.split_whitespace();
            let Some(pid) = tokens.next().and_then(|t| t.parse::<u32>().ok()) else {
                continue;
            };
            let Some(ppid) = tokens.next().and_then(|t| t.parse::<u32>().ok()) else {
                continue;
            };
            let command: String = tokens.collect::<Vec<_>>().join(" ").to_ascii_lowercase();
            map.insert(pid, (ppid, command));
        }
        assert_eq!(map.len(), 3);
        assert_eq!(map.get(&4901).map(|(p, _)| *p), Some(1));
        assert!(
            map.get(&4901)
                .map(|(_, c)| c.contains("cursor.app"))
                .unwrap_or(false)
        );
        assert_eq!(map.get(&220).map(|(p, _)| *p), Some(99993));
        assert!(
            map.get(&220)
                .map(|(_, c)| c.starts_with("claude"))
                .unwrap_or(false)
        );
    }

    #[test]
    fn agent_caller_excludes_humans_and_ci() {
        assert!(!is_agent_caller("tty"));
        assert!(!is_agent_caller("tty:cursor"));
        assert!(!is_agent_caller("tty:vscode"));
        assert!(!is_agent_caller("ci"));
        assert!(!is_agent_caller("ci:github_actions"));
        assert!(is_agent_caller("claude_code"));
        assert!(is_agent_caller("agent_unknown:python"));
        assert!(is_agent_caller("agent_unknown:vscode"));
        assert!(is_agent_caller("cloud_ide:codespaces"));
    }

    #[test]
    fn claude_substring_no_longer_overmatches() {
        // Before tightening: every one of these returned Some("claude_code"),
        // over-attributing Claude Desktop helpers / MCP binaries / install
        // paths to the claude_code agent.
        assert_eq!(
            caller_from_process_name("/Applications/Claude.app/Contents/Helpers/claude-helper"),
            None
        );
        assert_eq!(
            caller_from_process_name("node /opt/anthropic-claude-mcp/server.js"),
            None
        );
        assert_eq!(caller_from_process_name("bash ~/.claude/scripts/setup.sh"), None);

        // True positives still match.
        assert_eq!(
            caller_from_process_name("node /usr/local/lib/claude-code/cli.js"),
            Some("claude_code")
        );
        assert_eq!(
            caller_from_process_name("/usr/local/bin/claude --dangerously-skip-permissions"),
            Some("claude_code")
        );
        assert_eq!(
            caller_from_process_name("/anthropic.claude-code/bin/runner"),
            Some("claude_code")
        );
    }

    #[test]
    fn new_session_uuid_is_v4_format() {
        // Generated IDs must parse as v4 UUIDs so the dbt mart's
        // is_unstitched_agent_session() macro treats them as stitched
        // (not the `cli_<base64>` per-process fallback regex).
        let id = new_session_uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(id.chars().filter(|&c| c == '-').count(), 4);
        // Version nibble at position 14 must be '4'.
        assert_eq!(id.chars().nth(14), Some('4'));
        // Variant high bits at position 19 must be 8/9/a/b.
        let variant = id.chars().nth(19).unwrap();
        assert!(['8', '9', 'a', 'b'].contains(&variant), "variant char was {variant}");
        // Two successive calls produce different IDs.
        assert_ne!(id, new_session_uuid());
    }

    #[test]
    fn new_session_uuid_does_not_match_cli_fallback_regex() {
        // Cross-check against the dbt-side regex
        // `^cli_[A-Za-z0-9_-]{22}$` from
        // dbt-analytics/macros/normalize_caller.sql::is_unstitched_agent_session
        // — a UUID must NOT match (otherwise the dbt fix would re-bin the
        // persistent ID as unstitched, defeating the whole point).
        let id = new_session_uuid();
        assert!(
            !id.starts_with("cli_"),
            "persistent session id must not start with cli_ (would be re-flagged as fallback): {id}"
        );
    }
}
