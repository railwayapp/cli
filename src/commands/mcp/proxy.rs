//! Stdio ↔ streamable-HTTP proxy for the remote Railway MCP server.
//!
//! Bridges a harness speaking MCP over stdio (Claude Code, Cursor, …) to
//! `mcp.railway.com`, attaching a fresh `Authorization: Bearer` from the CLI's
//! stored login on every request. This lets users who have already run
//! `railway login` use the remote MCP server without going through the
//! harness's OAuth (DCR + browser consent) flow, and without ever writing a
//! long-lived credential into the harness config.
//!
//! Auth freshness is delegated to [`crate::client::ensure_valid_token`], which
//! serializes refreshes across concurrent CLI processes via the config
//! lockfile — the proxy can safely run alongside the local `railway mcp`
//! server and any other CLI invocations.
//!
//! The remote server currently runs the streamable-HTTP transport statelessly
//! (no `Mcp-Session-Id` issued), but the proxy tracks a session id anyway and
//! transparently re-initializes + retries once if the server ever reports a
//! missing/expired session. Server-initiated messages (the optional GET SSE
//! stream) are not proxied; nothing in the current tool surface relies on
//! them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde_json::{Value as JsonValue, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};

use crate::client::ensure_valid_token;
use crate::commands::Environment;
use crate::config::Configs;
use crate::consts;
use crate::telemetry;

/// JSON-RPC error code for auth failures surfaced by the proxy itself.
const AUTH_ERROR_CODE: i64 = -32001;

/// Hard ceiling on a single upstream response (one SSE stream, or a non-SSE
/// body). The proxy runs long-lived and attaches a live credential on every
/// request, so a compromised edge (or a dev/http override MITM) streaming a
/// boundary-less or unbounded body must not be able to grow memory without
/// limit. Generous enough for the largest legitimate tool payloads.
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

const LOGIN_HINT: &str = "Not logged in to Railway. Run `railway login` in a terminal, then retry \
     — the proxy picks up the new login automatically, no restart needed.";

struct ProxyState {
    http: reqwest::Client,
    url: String,
    configs: Mutex<Configs>,
    session: Mutex<SessionMeta>,
    /// Resolved once at startup: the proxy's working directory is fixed for
    /// the life of the process, so the link cannot change under it.
    link: LinkContext,
}

#[derive(Default)]
struct SessionMeta {
    id: Option<String>,
    /// The harness's `initialize` request, kept so the proxy can re-establish
    /// an upstream session (expiry, or a degraded logged-out start) without
    /// involving the harness.
    init_request: Option<JsonValue>,
    /// Header-safe harness identity extracted from `initialize.params.clientInfo.name`
    /// and attached to every upstream request as `x-railway-mcp-client`.
    client_name: Option<String>,
    /// Which context parameters each remote tool declares, learned from the
    /// `tools/list` result. Injection only fills a parameter a tool actually
    /// accepts, so this has to come from the server rather than a list baked
    /// into the CLI that would drift as tools change.
    tool_params: HashMap<String, HashSet<String>>,
}

/// The project/environment/service this invocation targets.
///
/// The local MCP server resolves these from `railway link`; the remote server
/// is a different machine and never can. Roughly 44% of successful local tool
/// calls pass no projectId and rely on exactly this, so without it the remote
/// path is not a drop-in replacement.
///
/// Covers the two sources `get_linked_project` resolves without I/O — the
/// RAILWAY_PROJECT_ID/ENVIRONMENT_ID/SERVICE_ID env vars and the directory
/// link. Deliberately NOT covered: resolving a project from a RAILWAY_TOKEN,
/// which costs a GraphQL round trip. Doing that here would put a network call
/// (and a 15s connect timeout on a bad one) in front of proxy startup, which
/// the harness is waiting on. Project-token users without a directory link or
/// env vars get no injection and must pass ids explicitly.
#[derive(Clone, Default)]
struct LinkContext {
    project_id: Option<String>,
    environment_id: Option<String>,
    service_id: Option<String>,
}

impl LinkContext {
    fn value_for(&self, param: &str) -> Option<&str> {
        match param {
            "projectId" => self.project_id.as_deref(),
            "environmentId" => self.environment_id.as_deref(),
            "serviceId" => self.service_id.as_deref(),
            _ => None,
        }
    }

    fn is_empty(&self) -> bool {
        self.project_id.is_none() && self.environment_id.is_none() && self.service_id.is_none()
    }
}

/// Context parameters the proxy will fill in. Ordered widest-first purely for
/// readable logs; injection is per-parameter and independent.
const INJECTABLE_PARAMS: [&str; 3] = ["projectId", "environmentId", "serviceId"];

/// Marks traffic as coming through `railway mcp proxy` so remote MCP telemetry
/// can separate it from editor OAuth and other direct clients.
const MCP_TRANSPORT_HEADER: &str = "x-railway-mcp-transport";
const MCP_TRANSPORT_VALUE: &str = "cli-proxy";
const MCP_CLIENT_HEADER: &str = "x-railway-mcp-client";

type Out = mpsc::UnboundedSender<String>;

pub async fn serve_proxy() -> Result<()> {
    let configs = Configs::new()?;
    let url = resolve_mcp_url(&configs)?;

    let http = reqwest::Client::builder()
        .danger_accept_invalid_certs(matches!(Configs::get_environment_id(), Environment::Dev))
        .user_agent(consts::get_user_agent())
        .connect_timeout(Duration::from_secs(15))
        // An MCP JSON-RPC POST is never legitimately redirected. Following
        // redirects on a request that carries a Bearer is unnecessary attack
        // surface — reqwest strips the token cross-host, but a same-host 307/308
        // would re-send the body and the redirected response is relayed blind.
        // Refuse redirects outright.
        .redirect(reqwest::redirect::Policy::none())
        // No overall timeout: tool calls (e.g. railway-agent) can legitimately
        // stream for minutes.
        .build()
        .context("Failed to build HTTP client")?;

    let link = read_link_context(&configs);
    let state = Arc::new(ProxyState {
        http,
        url,
        configs: Mutex::new(configs),
        session: Mutex::new(SessionMeta::default()),
        link,
    });

    // All stdout writes go through one task so concurrent responses can't
    // interleave within a line.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = rx.recv().await {
            let _ = stdout.write_all(line.as_bytes()).await;
            let _ = stdout.write_all(b"\n").await;
            let _ = stdout.flush().await;
        }
    });

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    // Process messages inline until the MCP handshake completes so the
    // initialize → initialized ordering is preserved upstream, then handle
    // messages concurrently (harnesses issue parallel tool calls).
    let mut handshake_done = false;

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: JsonValue = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("railway mcp proxy: ignoring unparseable message: {e}");
                continue;
            }
        };

        let mut msg = msg;
        if method_of(&msg) == Some("initialize") {
            let mut session = state.session.lock().await;
            session.init_request = Some(msg.clone());
            session.client_name = extract_mcp_client_header(&msg);
        } else if method_of(&msg) == Some("tools/call") {
            let session = state.session.lock().await;
            inject_link_context(&state.link, &session.tool_params, &mut msg);
        }

        if handshake_done {
            let state = state.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                handle_message(&state, msg, &tx).await;
            });
        } else {
            let completes_handshake = method_of(&msg) == Some("notifications/initialized");
            handle_message(&state, msg, &tx).await;
            if completes_handshake {
                handshake_done = true;
            }
        }
    }

    // stdin closed: the harness is gone. Best-effort end of the remote
    // session, then a bounded wait for in-flight tasks — a stalled upstream
    // stream must not keep an orphaned proxy alive after the harness exits.
    end_session(&state).await;
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
    Ok(())
}

fn method_of(msg: &JsonValue) -> Option<&str> {
    msg.get("method").and_then(JsonValue::as_str)
}

/// Read the directory link the same way the local MCP server does, so the two
/// surfaces resolve the same project. Absent link (or an unreadable config) is
/// normal — injection simply does nothing.
fn read_link_context(configs: &Configs) -> LinkContext {
    let linked = configs.get_local_linked_project().ok();

    // Env-var targeting wins over the directory link, matching
    // `get_linked_project`. Mixing the two would silently pair project A with
    // project B's environment, so an explicit RAILWAY_PROJECT_ID discards the
    // directory link unless both name the same project.
    let env_project = Configs::get_railway_project_id().filter(|s| !s.is_empty());
    let linked_for_env = linked
        .as_ref()
        .filter(|p| env_project.as_ref().is_none_or(|id| &p.project == id));

    let project_id = env_project
        .clone()
        .or_else(|| linked.as_ref().map(|p| p.project.clone()))
        .filter(|s| !s.is_empty());

    let environment_id = Configs::get_railway_environment_id()
        .or_else(|| linked_for_env.and_then(|p| p.environment.clone()))
        .filter(|s| !s.is_empty());

    let service_id = Configs::get_railway_service_id()
        .or_else(|| linked_for_env.and_then(|p| p.service.clone()))
        .filter(|s| !s.is_empty());

    LinkContext {
        project_id,
        environment_id,
        service_id,
    }
}

/// Learn each tool's declared parameters from a `tools/list` result.
///
/// A result carrying a `tools` array of `{name, inputSchema}` is unambiguous,
/// so this needs no id correlation with the originating request.
fn record_tool_params(session: &mut SessionMeta, msg: &JsonValue) {
    let Some(tools) = msg.pointer("/result/tools").and_then(JsonValue::as_array) else {
        return;
    };
    for tool in tools {
        let Some(name) = tool.get("name").and_then(JsonValue::as_str) else {
            continue;
        };
        let declared = tool
            .pointer("/inputSchema/properties")
            .and_then(JsonValue::as_object)
            .map(|props| props.keys().cloned().collect::<HashSet<String>>())
            .unwrap_or_default();
        session.tool_params.insert(name.to_string(), declared);
    }
}

/// Fill in linked project/environment/service on a `tools/call` the harness
/// left them off.
///
/// Deliberately conservative in three ways: it only fills a parameter the tool
/// declares (so a docs or workspace tool is untouched), never overwrites a
/// value the caller supplied, and does nothing at all until `tools/list` has
/// been seen. An unknown tool is left exactly as the harness sent it.
fn inject_link_context(
    link: &LinkContext,
    tool_params: &HashMap<String, HashSet<String>>,
    msg: &mut JsonValue,
) {
    if link.is_empty() || method_of(msg) != Some("tools/call") {
        return;
    }
    let Some(tool_name) = msg
        .pointer("/params/name")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    let Some(declared) = tool_params.get(&tool_name) else {
        return;
    };

    let missing: Vec<(&str, String)> = INJECTABLE_PARAMS
        .iter()
        .filter(|param| declared.contains(**param))
        .filter_map(|param| {
            let already_set = msg
                .pointer(&format!("/params/arguments/{param}"))
                .is_some_and(|v| !v.is_null());
            if already_set {
                return None;
            }
            link.value_for(param).map(|v| (*param, v.to_string()))
        })
        .collect();

    if missing.is_empty() {
        return;
    }

    let Some(params) = msg.get_mut("params").and_then(JsonValue::as_object_mut) else {
        return;
    };
    let arguments = params
        .entry("arguments")
        .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
    let Some(arguments) = arguments.as_object_mut() else {
        return;
    };
    for (param, value) in missing {
        arguments.insert(param.to_string(), JsonValue::String(value));
    }
}

/// Request ids awaiting a response in this message — one for a plain request,
/// several for a JSON-RPC batch (protocol ≤2025-03-26 allows top-level
/// arrays), none for notifications. Error paths must answer every id or the
/// harness waits forever.
fn ids_of(msg: &JsonValue) -> Vec<JsonValue> {
    match msg {
        JsonValue::Array(items) => items
            .iter()
            .filter_map(|m| m.get("id").cloned().filter(|id| !id.is_null()))
            .collect(),
        _ => msg
            .get("id")
            .cloned()
            .filter(|id| !id.is_null())
            .into_iter()
            .collect(),
    }
}

fn resolve_mcp_url(configs: &Configs) -> Result<String> {
    let is_dev = matches!(Configs::get_environment_id(), Environment::Dev);
    if let Ok(raw) = std::env::var("RAILWAY_MCP_URL") {
        if let Some(url) = validate_mcp_override(&raw, is_dev)? {
            return Ok(url);
        }
    }
    Ok(format!("https://mcp.{}", configs.get_host()))
}

/// Validate a `RAILWAY_MCP_URL` override. Returns the normalized URL, `None`
/// when the value is blank (caller falls back to the default), or an error
/// when it would send the Bearer over a non-TLS connection.
///
/// The Bearer is attached to every request to this URL, and the cross-host
/// redirect strip does not protect the *first* hop — so a plaintext target
/// leaks the credential outright. Require https except in the local Dev
/// environment, where a plaintext raildev endpoint is expected.
fn validate_mcp_override(raw: &str, is_dev: bool) -> Result<Option<String>> {
    let url = raw.trim();
    if url.is_empty() {
        return Ok(None);
    }
    if !url.starts_with("https://") && !is_dev {
        anyhow::bail!(
            "RAILWAY_MCP_URL must be an https:// URL (got {url:?}); refusing to send credentials over a non-TLS connection."
        );
    }
    Ok(Some(url.trim_end_matches('/').to_string()))
}

async fn handle_message(state: &ProxyState, msg: JsonValue, out: &Out) {
    let ids = ids_of(&msg);

    let Some(token) = fresh_token(state).await else {
        respond_unauthenticated(&msg, &ids, out);
        return;
    };

    if let Err(e) = forward(state, &msg, &token, out).await {
        if ids.is_empty() {
            eprintln!("railway mcp proxy: {e:#}");
        }
        for id in &ids {
            send_error(out, id, -32603, &format!("Railway MCP proxy error: {e:#}"));
        }
    }
}

/// Get a currently-valid auth token, refreshing the stored OAuth credentials
/// if they have expired. Returns `None` when the user has no usable login.
async fn fresh_token(state: &ProxyState) -> Option<String> {
    let mut configs = state.configs.lock().await;
    // A proxy started before `railway login` has no token in memory, and
    // `ensure_valid_token`'s fast path never re-reads the config in that
    // state. Reload from disk so a login that happened after startup is
    // picked up — this is what makes the LOGIN_HINT's "no restart needed"
    // promise true.
    if configs.get_railway_auth_token().is_none() {
        if let Err(e) = configs.reload() {
            eprintln!("railway mcp proxy: config reload failed: {e:#}");
        }
    }
    if let Err(e) = ensure_valid_token(&mut configs).await {
        // On `invalid_grant` the dead credential has already been cleared, so
        // `get_railway_auth_token()` below returns None and the caller answers
        // with LOGIN_HINT — actionable inside an MCP harness, where this
        // stderr line is invisible. Previously the stale token was handed back
        // and every tool call failed as an opaque "Unauthorized" instead.
        eprintln!("railway mcp proxy: token refresh failed: {e:#}");
    }
    configs.get_railway_auth_token()
}

/// Without a login the proxy still completes the MCP handshake (a crashed
/// server renders as an opaque failure in most harnesses) and answers every
/// request with an actionable error. Once the user logs in, the next request
/// heals automatically via the re-initialize path in [`forward`].
fn respond_unauthenticated(msg: &JsonValue, ids: &[JsonValue], out: &Out) {
    let [id] = ids else {
        // Batch or notification: answer every id (none for a notification).
        for id in ids {
            send_error(out, id, AUTH_ERROR_CODE, LOGIN_HINT);
        }
        return;
    };

    if method_of(msg) == Some("initialize") {
        let protocol = msg
            .pointer("/params/protocolVersion")
            .and_then(JsonValue::as_str)
            .unwrap_or("2025-03-26");
        let result = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": protocol,
                "capabilities": { "tools": { "listChanged": true } },
                "serverInfo": {
                    "name": "railway",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": LOGIN_HINT,
            }
        });
        let _ = out.send(result.to_string());
    } else {
        send_error(out, id, AUTH_ERROR_CODE, LOGIN_HINT);
    }
}

async fn forward(state: &ProxyState, msg: &JsonValue, token: &str, out: &Out) -> Result<()> {
    let is_initialize = method_of(msg) == Some("initialize");
    let session_id = state.session.lock().await.id.clone();

    let resp = post_message(
        state,
        msg,
        token,
        if is_initialize {
            None
        } else {
            session_id.as_deref()
        },
    )
    .await?;

    // Session lost or never established upstream (server-side expiry, or the
    // proxy started degraded while logged out): re-initialize with the
    // captured initialize request and retry once.
    let status = resp.status();
    let can_reinit = { state.session.lock().await.init_request.is_some() };
    if !is_initialize && (status == 404 || status == 400) && can_reinit {
        let _ = resp.bytes().await;
        reinitialize(state, token).await?;
        let session_id = state.session.lock().await.id.clone();
        let resp = post_message(state, msg, token, session_id.as_deref()).await?;
        return consume_response(state, resp, msg, is_initialize, out).await;
    }

    consume_response(state, resp, msg, is_initialize, out).await
}

async fn post_message(
    state: &ProxyState,
    msg: &JsonValue,
    token: &str,
    session_id: Option<&str>,
) -> Result<reqwest::Response> {
    let client_name = state.session.lock().await.client_name.clone();
    let mut req = state
        .http
        .post(&state.url)
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json, text/event-stream")
        .header("x-source", consts::get_user_agent())
        .header(MCP_TRANSPORT_HEADER, MCP_TRANSPORT_VALUE);
    if let Some(client) = client_name.as_deref() {
        req = req.header(MCP_CLIENT_HEADER, client);
    }
    if let Some(sid) = session_id {
        req = req.header("mcp-session-id", sid);
    }
    req.json(msg)
        .send()
        .await
        .context("failed to reach the remote MCP server")
}

/// Pull a telemetry-safe client identity out of an MCP `initialize` request.
fn extract_mcp_client_header(msg: &JsonValue) -> Option<String> {
    let name = msg
        .pointer("/params/clientInfo/name")
        .and_then(JsonValue::as_str)?;
    telemetry::mcp_client_header_value(name)
}

/// Re-run the MCP handshake upstream using the harness's captured `initialize`
/// request, discarding the result (the harness already completed its own
/// handshake). Serialized behind the session lock so concurrent failures
/// don't stampede.
async fn reinitialize(state: &ProxyState, token: &str) -> Result<()> {
    let mut session = state.session.lock().await;
    let init = session
        .init_request
        .clone()
        .context("no initialize request captured yet")?;

    let resp = post_message(state, &init, token, None).await?;
    anyhow::ensure!(
        resp.status().is_success(),
        "re-initialize failed with HTTP {}",
        resp.status()
    );
    session.id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let _ = resp.bytes().await;
    let session_id = session.id.clone();
    drop(session);

    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let resp = post_message(state, &initialized, token, session_id.as_deref()).await?;
    let _ = resp.bytes().await;
    Ok(())
}

async fn consume_response(
    state: &ProxyState,
    resp: reqwest::Response,
    msg: &JsonValue,
    is_initialize: bool,
    out: &Out,
) -> Result<()> {
    let status = resp.status();

    if is_initialize {
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            state.session.lock().await.id = Some(sid.to_string());
        }
    }

    if status == 401 || status == 403 {
        let _ = resp.bytes().await;
        for id in &ids_of(msg) {
            send_error(
                out,
                id,
                AUTH_ERROR_CODE,
                "Railway rejected the CLI's credentials. Run `railway login` and try again.",
            );
        }
        return Ok(());
    }

    // Accepted notification/response with no body.
    if status == 202 || status == 204 {
        return Ok(());
    }

    if !status.is_success() {
        let body = read_body_capped(resp).await.unwrap_or_default();
        anyhow::bail!(
            "remote MCP server returned HTTP {status}: {}",
            truncate(&body, 300)
        );
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // A tools/list result tells us which context parameters each tool accepts,
    // which is what makes link-context injection safe. Learned from whichever
    // transport the server answered on.
    let learn_tools = method_of(msg) == Some("tools/list");

    if content_type.starts_with("text/event-stream") {
        stream_sse(state, resp, out, learn_tools).await
    } else {
        let body = read_body_capped(resp).await?;
        if let Some(parsed) = emit_json_line(body.trim(), out)
            && learn_tools
        {
            record_tool_params(&mut *state.session.lock().await, &parsed);
        }
        Ok(())
    }
}

/// Read a full (non-streaming) response body, refusing to buffer more than
/// [`MAX_RESPONSE_BYTES`]. `reqwest`'s `text()`/`bytes()` have no size cap, so
/// a compromised or MITM'd upstream could otherwise stream an unbounded body
/// into a long-lived proxy and exhaust memory.
async fn read_body_capped(resp: reqwest::Response) -> Result<String> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading response from remote MCP server")?;
        if buf.len() + chunk.len() > MAX_RESPONSE_BYTES {
            anyhow::bail!(
                "remote MCP server response exceeded {MAX_RESPONSE_BYTES} bytes; aborting."
            );
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Relay every SSE `data:` payload to stdout as its own JSON-RPC line. The
/// server closes the per-request stream after the final response message.
async fn stream_sse(
    state: &ProxyState,
    resp: reqwest::Response,
    out: &Out,
    learn_tools: bool,
) -> Result<()> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading SSE stream from remote MCP server")?;
        buf.extend_from_slice(&chunk);
        while let Some((event_len, boundary_end)) = find_event_boundary(&buf) {
            let event: Vec<u8> = buf.drain(..boundary_end).collect();
            if let Some(parsed) = emit_sse_event(&event[..event_len], out)
                && learn_tools
            {
                record_tool_params(&mut *state.session.lock().await, &parsed);
            }
        }
        // A boundary-less stream (or one giant event) would otherwise grow buf
        // without limit. Cap it: past the ceiling, no legitimate single SSE
        // event is pending — abort rather than let a bad upstream OOM us.
        if buf.len() > MAX_RESPONSE_BYTES {
            anyhow::bail!(
                "remote MCP server SSE event exceeded {MAX_RESPONSE_BYTES} bytes; aborting."
            );
        }
    }
    if !buf.is_empty()
        && let Some(parsed) = emit_sse_event(&buf, out)
        && learn_tools
    {
        record_tool_params(&mut *state.session.lock().await, &parsed);
    }
    Ok(())
}

/// Find the end of the next SSE event: a blank line, i.e. `\n\n` or
/// `\r\n\r\n`. Returns (event bytes length, total length including boundary).
fn find_event_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    for i in 0..buf.len() {
        if buf[i] != b'\n' {
            continue;
        }
        if buf.get(i + 1) == Some(&b'\n') {
            return Some((i, i + 2));
        }
        if buf.get(i + 1) == Some(&b'\r') && buf.get(i + 2) == Some(&b'\n') {
            return Some((i, i + 3));
        }
    }
    None
}

fn emit_sse_event(raw: &[u8], out: &Out) -> Option<JsonValue> {
    let text = String::from_utf8_lossy(raw);
    let data_lines: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|rest| rest.strip_prefix(' ').unwrap_or(rest))
        .collect();
    if data_lines.is_empty() {
        return None;
    }
    emit_json_line(&data_lines.join("\n"), out)
}

/// Write one JSON-RPC message as a single stdout line. Payloads are compacted
/// through serde so an upstream message containing raw newlines can't corrupt
/// the newline-delimited stdio framing.
fn emit_json_line(payload: &str, out: &Out) -> Option<JsonValue> {
    if payload.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<JsonValue>(payload).ok();
    let line = parsed
        .as_ref()
        .map(|v| v.to_string())
        .unwrap_or_else(|| payload.replace(['\n', '\r'], " "));
    let _ = out.send(line);
    parsed
}

fn send_error(out: &Out, id: &JsonValue, code: i64, message: &str) {
    let err = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    });
    let _ = out.send(err.to_string());
}

/// Best-effort session teardown when the harness disconnects.
async fn end_session(state: &ProxyState) {
    let (session_id, client_name) = {
        let session = state.session.lock().await;
        (session.id.clone(), session.client_name.clone())
    };
    let Some(session_id) = session_id else { return };
    let token = { state.configs.lock().await.get_railway_auth_token() };
    let Some(token) = token else { return };
    let mut req = state
        .http
        .delete(&state.url)
        .header("authorization", format!("Bearer {token}"))
        .header("mcp-session-id", session_id)
        .header(MCP_TRANSPORT_HEADER, MCP_TRANSPORT_VALUE)
        .timeout(Duration::from_secs(5));
    if let Some(client) = client_name.as_deref() {
        req = req.header(MCP_CLIENT_HEADER, client);
    }
    let _ = req.send().await;
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(line) = rx.try_recv() {
            out.push(line);
        }
        out
    }

    #[test]
    fn mcp_override_rejects_plaintext_outside_dev() {
        // http:// would send the Bearer in the clear — refused in prod/staging.
        assert!(validate_mcp_override("http://evil.example/mcp", false).is_err());
        // Blank falls back to the default (Ok(None), not an error).
        assert_eq!(validate_mcp_override("  ", false).unwrap(), None);
        // https is accepted and trailing slashes normalized.
        assert_eq!(
            validate_mcp_override("https://mcp.railway.com/", false).unwrap(),
            Some("https://mcp.railway.com".to_string()),
        );
        // Dev allows plaintext for a local raildev endpoint.
        assert_eq!(
            validate_mcp_override("http://localhost:8080", true).unwrap(),
            Some("http://localhost:8080".to_string()),
        );
    }

    #[test]
    fn sse_event_boundary_handles_lf_and_crlf() {
        assert_eq!(find_event_boundary(b"data: {}\n\nrest"), Some((8, 10)));
        assert_eq!(find_event_boundary(b"data: {}\r\n\r\nrest"), Some((9, 12)));
        assert_eq!(find_event_boundary(b"data: {}"), None);
    }

    #[test]
    fn sse_event_extracts_data_payload() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        emit_sse_event(b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1}", &tx);
        assert_eq!(collect(&mut rx), vec![r#"{"id":1,"jsonrpc":"2.0"}"#]);
    }

    #[test]
    fn sse_event_joins_multiline_data() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        emit_sse_event(b"data: {\"a\":\ndata: 1}", &tx);
        assert_eq!(collect(&mut rx), vec![r#"{"a":1}"#]);
    }

    #[test]
    fn sse_event_without_data_is_dropped() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        emit_sse_event(b"event: ping\nid: 4", &tx);
        assert!(collect(&mut rx).is_empty());
    }

    #[test]
    fn json_lines_are_compacted_to_one_line() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        emit_json_line("{\n  \"jsonrpc\": \"2.0\",\n  \"id\": 7\n}", &tx);
        let lines = collect(&mut rx);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains('\n'));
    }

    #[test]
    fn unauthenticated_initialize_fabricates_result() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" },
        });
        respond_unauthenticated(&msg, &ids_of(&msg), &tx);
        let lines = collect(&mut rx);
        assert_eq!(lines.len(), 1);
        let parsed: JsonValue = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(
            parsed.pointer("/result/protocolVersion").unwrap(),
            "2025-06-18"
        );
        assert!(
            parsed
                .pointer("/result/instructions")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("railway login")
        );
    }

    #[test]
    fn unauthenticated_request_gets_actionable_error() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let msg = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" });
        respond_unauthenticated(&msg, &ids_of(&msg), &tx);
        let lines = collect(&mut rx);
        assert_eq!(lines.len(), 1);
        let parsed: JsonValue = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(parsed.pointer("/error/code").unwrap(), AUTH_ERROR_CODE);
        assert!(
            parsed
                .pointer("/error/message")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("railway login")
        );
    }

    #[test]
    fn unauthenticated_notification_is_dropped() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let msg = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        respond_unauthenticated(&msg, &ids_of(&msg), &tx);
        assert!(collect(&mut rx).is_empty());
    }

    #[test]
    fn extracts_known_mcp_client_from_initialize() {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "claude-code", "version": "1.0.0" }
            }
        });
        assert_eq!(
            extract_mcp_client_header(&msg).as_deref(),
            Some("claude_code")
        );
    }

    #[test]
    fn extracts_unknown_mcp_client_as_slug() {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": { "clientInfo": { "name": "Totally New IDE" } }
        });
        assert_eq!(
            extract_mcp_client_header(&msg).as_deref(),
            Some("mcp_unknown:totally-new-ide")
        );
    }

    #[test]
    fn missing_client_info_yields_no_header() {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": { "protocolVersion": "2025-03-26" }
        });
        assert_eq!(extract_mcp_client_header(&msg), None);
    }

    #[test]
    fn unauthenticated_batch_answers_every_id() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let msg = json!([
            { "jsonrpc": "2.0", "id": 1, "method": "tools/list" },
            { "jsonrpc": "2.0", "method": "notifications/progress" },
            { "jsonrpc": "2.0", "id": "two", "method": "tools/call" },
        ]);
        let ids = ids_of(&msg);
        assert_eq!(ids, vec![json!(1), json!("two")]);
        respond_unauthenticated(&msg, &ids, &tx);
        let lines = collect(&mut rx);
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let parsed: JsonValue = serde_json::from_str(line).unwrap();
            assert_eq!(parsed.pointer("/error/code").unwrap(), AUTH_ERROR_CODE);
        }
    }
}

#[cfg(test)]
mod link_context_tests {
    use super::*;

    fn link() -> LinkContext {
        LinkContext {
            project_id: Some("proj-1".into()),
            environment_id: Some("env-1".into()),
            service_id: Some("svc-1".into()),
        }
    }

    /// What the server reports for a project-scoped tool.
    fn params_for(tool: &str, declared: &[&str]) -> HashMap<String, HashSet<String>> {
        let mut m = HashMap::new();
        m.insert(
            tool.to_string(),
            declared.iter().map(|s| s.to_string()).collect(),
        );
        m
    }

    fn call(tool: &str, arguments: JsonValue) -> JsonValue {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        })
    }

    #[test]
    fn fills_the_context_a_tool_declares_but_the_caller_omitted() {
        // The gap this closes: ~44% of successful local MCP calls pass no
        // projectId and rely on `railway link`, which the remote server cannot
        // see.
        let params = params_for("list-services", &["projectId", "environmentId"]);
        let mut msg = call("list-services", json!({}));

        inject_link_context(&link(), &params, &mut msg);

        assert_eq!(
            msg.pointer("/params/arguments/projectId").unwrap(),
            "proj-1"
        );
        assert_eq!(
            msg.pointer("/params/arguments/environmentId").unwrap(),
            "env-1"
        );
        // Not declared by this tool, so not invented.
        assert!(msg.pointer("/params/arguments/serviceId").is_none());
    }

    #[test]
    fn never_overwrites_what_the_caller_supplied() {
        let params = params_for("list-services", &["projectId"]);
        let mut msg = call("list-services", json!({ "projectId": "explicit" }));

        inject_link_context(&link(), &params, &mut msg);

        assert_eq!(
            msg.pointer("/params/arguments/projectId").unwrap(),
            "explicit"
        );
    }

    #[test]
    fn leaves_tools_that_declare_no_context_alone() {
        let params = params_for("search-docs", &["query"]);
        let mut msg = call("search-docs", json!({ "query": "volumes" }));

        inject_link_context(&link(), &params, &mut msg);

        assert_eq!(
            msg.pointer("/params/arguments").unwrap(),
            &json!({ "query": "volumes" })
        );
    }

    #[test]
    fn does_nothing_before_tools_list_has_been_seen() {
        // An unknown tool means no schema yet; guessing could send a parameter
        // the tool does not accept.
        let mut msg = call("list-services", json!({}));

        inject_link_context(&link(), &HashMap::new(), &mut msg);

        assert_eq!(msg.pointer("/params/arguments").unwrap(), &json!({}));
    }

    #[test]
    fn does_nothing_without_a_directory_link() {
        let params = params_for("list-services", &["projectId"]);
        let mut msg = call("list-services", json!({}));

        inject_link_context(&LinkContext::default(), &params, &mut msg);

        assert_eq!(msg.pointer("/params/arguments").unwrap(), &json!({}));
    }

    #[test]
    fn creates_the_arguments_object_when_the_caller_sent_none() {
        let params = params_for("list-services", &["projectId"]);
        let mut msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "list-services" },
        });

        inject_link_context(&link(), &params, &mut msg);

        assert_eq!(
            msg.pointer("/params/arguments/projectId").unwrap(),
            "proj-1"
        );
    }

    #[test]
    fn ignores_messages_that_are_not_tool_calls() {
        let params = params_for("list-services", &["projectId"]);
        let mut msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });

        inject_link_context(&link(), &params, &mut msg);

        assert!(msg.pointer("/params").is_none());
    }

    #[test]
    fn learns_declared_parameters_from_a_tools_list_result() {
        let mut session = SessionMeta::default();
        record_tool_params(
            &mut session,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "tools": [
                    { "name": "list-services", "inputSchema": { "properties": {
                        "projectId": {}, "environmentId": {}
                    }}},
                    { "name": "whoami", "inputSchema": { "properties": {} } }
                ]}
            }),
        );

        assert!(session.tool_params["list-services"].contains("projectId"));
        assert!(session.tool_params["whoami"].is_empty());
    }

    /// Serialized: these mutate process env, which is global.
    #[test]
    fn env_var_targeting_overrides_and_does_not_mix_with_a_stale_link() {
        use std::sync::Mutex as StdMutex;
        static ENV_LOCK: StdMutex<()> = StdMutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // RAILWAY_PROJECT_ID naming a different project than the directory
        // link must not inherit that link's environment — pairing project A
        // with project B's environment is exactly the silent-wrong-target bug
        // injection is supposed to avoid.
        let linked = LinkContext {
            project_id: Some("proj-from-dir".into()),
            environment_id: Some("env-from-dir".into()),
            service_id: Some("svc-from-dir".into()),
        };
        assert_eq!(linked.value_for("projectId"), Some("proj-from-dir"));
        assert_eq!(linked.value_for("unknownParam"), None);
    }

    #[test]
    fn does_not_inject_into_a_jsonrpc_batch() {
        // Known, deliberate gap: method_of() sees no method on a top-level
        // array, so a batched tools/call is forwarded untouched. Fail-closed
        // is the right side to err on, and batches are vanishingly rare.
        let params = params_for("list-services", &["projectId"]);
        let mut msg = json!([
            { "jsonrpc": "2.0", "id": 1, "method": "tools/call",
              "params": { "name": "list-services", "arguments": {} } }
        ]);

        inject_link_context(&link(), &params, &mut msg);

        assert_eq!(msg.pointer("/0/params/arguments").unwrap(), &json!({}));
    }

    #[test]
    fn survives_malformed_params_and_arguments() {
        let params = params_for("list-services", &["projectId"]);

        // params is not an object
        let mut a = json!({ "method": "tools/call", "params": "nope" });
        inject_link_context(&link(), &params, &mut a);
        assert_eq!(a.pointer("/params").unwrap(), "nope");

        // arguments is an array rather than an object
        let mut b = json!({
            "method": "tools/call",
            "params": { "name": "list-services", "arguments": [1, 2] }
        });
        inject_link_context(&link(), &params, &mut b);
        assert_eq!(b.pointer("/params/arguments").unwrap(), &json!([1, 2]));

        // no tool name at all
        let mut c = json!({ "method": "tools/call", "params": { "arguments": {} } });
        inject_link_context(&link(), &params, &mut c);
        assert_eq!(c.pointer("/params/arguments").unwrap(), &json!({}));
    }

    #[test]
    fn treats_an_explicit_null_as_absent() {
        let params = params_for("list-services", &["projectId"]);
        let mut msg = call("list-services", json!({ "projectId": null }));

        inject_link_context(&link(), &params, &mut msg);

        assert_eq!(
            msg.pointer("/params/arguments/projectId").unwrap(),
            "proj-1"
        );
    }

    #[test]
    fn injects_only_what_the_link_actually_has() {
        // A directory can be linked to a project without an environment.
        let partial = LinkContext {
            project_id: Some("proj-1".into()),
            environment_id: None,
            service_id: None,
        };
        let params = params_for("list-services", &["projectId", "environmentId"]);
        let mut msg = call("list-services", json!({}));

        inject_link_context(&partial, &params, &mut msg);

        assert_eq!(
            msg.pointer("/params/arguments/projectId").unwrap(),
            "proj-1"
        );
        // Left for the server to default rather than invented here.
        assert!(msg.pointer("/params/arguments/environmentId").is_none());
    }

    #[test]
    fn ignores_results_that_are_not_tool_listings() {
        let mut session = SessionMeta::default();
        record_tool_params(&mut session, &json!({ "result": { "content": [] } }));
        assert!(session.tool_params.is_empty());
    }
}
