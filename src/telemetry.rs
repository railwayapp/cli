use std::{io::IsTerminal, sync::OnceLock};

use anyhow::Context;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::Serialize;
use serde_json::{Value, json};

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

fn known_agent_from_env() -> Option<&'static str> {
    const ENVS: &[(&str, &str)] = &[
        ("OPENCODE", "opencode"),
        ("OPENCODE_SESSION_ID", "opencode"),
        ("CLAUDECODE", "claude_code"),
        ("CLAUDE_CODE", "claude_code"),
        ("CLAUDECODE_SESSION_ID", "claude_code"),
        ("CURSOR_TRACE_ID", "cursor"),
        ("CURSOR_AGENT", "cursor"),
        ("CODEX_SANDBOX", "codex"),
        ("OPENAI_CODEX", "codex"),
    ];

    ENVS.iter()
        .find_map(|(name, caller)| std::env::var(name).ok().map(|_| *caller))
}

fn caller_from_process_name(name: &str) -> Option<&'static str> {
    let name = name.to_ascii_lowercase();
    if name.contains("opencode") {
        Some("opencode")
    } else if name.contains("claude") {
        Some("claude_code")
    } else if name.contains("cursor") {
        Some("cursor")
    } else if name.contains("codex") {
        Some("codex")
    } else if name.contains("windsurf") {
        Some("windsurf")
    } else {
        None
    }
}

#[cfg(unix)]
fn ps_field(pid: u32, field: &str) -> Option<String> {
    let pid = pid.to_string();
    let output = std::process::Command::new("ps")
        .args(["-o", field, "-p", &pid])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(unix)]
fn detect_process_caller() -> Option<&'static str> {
    let mut pid = std::process::id();
    for _ in 0..8 {
        if let Some(comm) = ps_field(pid, "comm=") {
            if let Some(caller) = caller_from_process_name(&comm) {
                return Some(caller);
            }
        }

        let parent = ps_field(pid, "ppid=")?.trim().parse::<u32>().ok()?;
        if parent == 0 || parent == pid {
            break;
        }
        pid = parent;
    }
    None
}

#[cfg(not(unix))]
fn detect_process_caller() -> Option<&'static str> {
    None
}

fn detect_caller() -> String {
    static CALLER: OnceLock<String> = OnceLock::new();
    CALLER
        .get_or_init(|| {
            safe_env(RAILWAY_CALLER_ENV)
                .or_else(|| known_agent_from_env().map(str::to_string))
                .or_else(|| detect_process_caller().map(str::to_string))
                .unwrap_or_else(|| {
                    if Configs::env_is_ci() {
                        "ci".to_string()
                    } else if !std::io::stdout().is_terminal() {
                        "agent_subprocess".to_string()
                    } else {
                        "tty".to_string()
                    }
                })
        })
        .clone()
}

fn is_agent_caller(caller: &str) -> bool {
    !matches!(caller, "tty" | "ci")
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
        let session_id = session_id();
        let caller = detect_caller();
        let linked_project = configs.get_local_linked_project().ok();
        let agent_session_id = safe_env(RAILWAY_AGENT_SESSION_ENV).or_else(|| {
            if is_agent_caller(&caller) {
                Some(session_id.clone())
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

    let context = TelemetryContext::current(&configs);
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

pub async fn send_setup_agent(event: SetupAgentTrackEvent) {
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
