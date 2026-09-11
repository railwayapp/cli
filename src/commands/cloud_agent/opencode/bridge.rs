//! A pane-scoped HTTP bridge: observe this client's successful thread actions,
//! while forwarding SSE, request bodies, and upgraded terminal streams intact.
use super::super::client_sessions::{self, Thread};
use anyhow::Result;
use base64::Engine;
use futures_util::StreamExt;
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Frame, Incoming},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use std::{
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Body = UnsyncBoxBody<Bytes, Error>;

pub(crate) struct Bridge {
    pub url: String,
    task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
    }
}

impl Bridge {
    pub(crate) fn start(
        connection: super::Connection,
        beta: bool,
        notify: impl Fn(Thread) + Send + Sync + 'static,
    ) -> Result<Self> {
        super::validate_url(&connection.url)?;
        Self::bind(connection, beta, notify)
    }

    fn bind(
        connection: super::Connection,
        beta: bool,
        notify: impl Fn(Thread) + Send + Sync + 'static,
    ) -> Result<Self> {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let url = format!("http://{}", listener.local_addr()?);
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let (shutdown, closed) = tokio::sync::watch::channel(false);
        let state = Arc::new(State {
            closed,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .build()?,
            connection,
            beta,
            notify: Box::new(notify),
            sequence: AtomicU64::new(0),
            selected: Mutex::new(None),
        });
        let task = tokio::spawn(async move {
            let mut peers = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    incoming = listener.accept() => {
                        let Ok((stream, _)) = incoming else { break; };
                        let state = state.clone();
                        peers.spawn(async move {
                            let service = service_fn(move |request| {
                                let state = state.clone();
                                async move { Ok::<_, Infallible>(relay(request, state).await.unwrap_or_else(|_| response(StatusCode::BAD_GATEWAY, "Backend unavailable"))) }
                            });
                            let _ = hyper::server::conn::http1::Builder::new()
                                .serve_connection(TokioIo::new(stream), service).with_upgrades().await;
                        });
                    }
                    _ = peers.join_next(), if !peers.is_empty() => {}
                }
            }
        });
        Ok(Self {
            url,
            task,
            shutdown,
        })
    }
}

struct State {
    closed: tokio::sync::watch::Receiver<bool>,
    client: reqwest::Client,
    connection: super::Connection,
    beta: bool,
    notify: Box<dyn Fn(Thread) + Send + Sync>,
    sequence: AtomicU64,
    selected: Mutex<Option<Thread>>,
}

impl State {
    fn select(&self, thread: Thread) {
        let mut selected = self.selected.lock().unwrap();
        *selected = Some(thread.clone());
        (self.notify)(thread);
    }

    fn event(&self, value: &serde_json::Value) {
        let mut selected = self.selected.lock().unwrap();
        if let Some(thread) = selected.as_mut()
            && update_thread(thread, value, self.beta)
        {
            (self.notify)(thread.clone());
        }
    }
}

fn update_thread(thread: &mut Thread, value: &serde_json::Value, beta: bool) -> bool {
    let value = value.get("payload").unwrap_or(value);
    let data = &value[if beta { "data" } else { "properties" }];
    let id = data["sessionID"]
        .as_str()
        .or_else(|| data["info"]["id"].as_str());
    if id != Some(thread.id.as_str()) {
        return false;
    }
    match value["type"].as_str() {
        Some("session.renamed") if beta => {
            let Some(title) = data["title"].as_str() else {
                return false;
            };
            thread.title = client_sessions::title(Some(title), &thread.title);
        }
        Some("session.updated") if !beta => {
            let Ok(info) = client_sessions::parse_opencode(&data["info"], &thread.directory) else {
                return false;
            };
            thread.title = info.title;
            thread.updated_at = info.updated_at;
        }
        Some("session.status") => {
            thread.state = match data["status"]["type"].as_str() {
                Some("busy" | "retry") => "working",
                Some("idle") => "idle",
                _ => return false,
            }
            .into();
        }
        Some("session.execution.started") if beta => thread.state = "working".into(),
        Some("session.execution.succeeded" | "session.execution.interrupted") if beta => {
            thread.state = "idle".into()
        }
        Some("session.execution.failed") if beta => thread.state = "failed".into(),
        _ => return false,
    }
    if beta {
        thread.updated_at = value["created"]
            .as_i64()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .unwrap_or_else(chrono::Utc::now)
            .to_rfc3339();
    }
    true
}

/// Observe the client's existing SSE stream without buffering its delivery.
/// Bound observation memory even for oversized transcript events.
#[derive(Default)]
struct Events {
    line: Vec<u8>,
    data: Vec<u8>,
    overflow: bool,
}
impl Events {
    fn feed(&mut self, bytes: &[u8], mut notify: impl FnMut(serde_json::Value)) {
        for byte in bytes {
            if *byte != b'\n' {
                if self.line.len() + self.data.len() < 256 * 1024 {
                    self.line.push(*byte);
                } else {
                    self.overflow = true;
                }
                continue;
            }
            if self.line.last() == Some(&b'\r') {
                self.line.pop();
            }
            if self.line.is_empty() {
                if !self.overflow
                    && let Ok(value) = serde_json::from_slice(&self.data)
                {
                    notify(value);
                }
                self.data.clear();
                self.overflow = false;
            } else if let Some(data) = self.line.strip_prefix(b"data:") {
                self.data
                    .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
                self.data.push(b'\n');
            }
            self.line.clear();
        }
    }
}

fn body(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

fn response(status: StatusCode, text: &'static str) -> Response<Body> {
    Response::builder().status(status).body(body(text)).unwrap()
}

/// An empty ID means the create/fork response supplies the newly selected ID.
fn selection(method: &str, path: &str, beta: bool) -> Option<String> {
    let path = path.strip_prefix(if beta { "/api/session" } else { "/session" })?;
    if path.is_empty() && method == "POST" {
        return Some(String::new());
    }
    let (id, action) = path.strip_prefix('/')?.split_once('/')?;
    client_sessions::validate_id(id).ok()?;
    if method == "POST" && action == "fork" {
        return Some(String::new());
    }
    ((method == "POST"
        && matches!(
            action,
            "view" | "prompt" | "prompt_async" | "command" | "shell"
        ))
        || (!beta && method == "GET" && action == "message"))
        .then(|| id.into())
}

async fn relay(mut request: Request<Incoming>, state: Arc<State>) -> Result<Response<Body>> {
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!(
            "{}:{}",
            state.connection.username, state.connection.password
        ))
    );
    let authorized = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        == Some(&expected);
    let upgrade = request
        .headers()
        .get("upgrade")
        .is_some_and(|v| v == "websocket");
    // Terminal upgrades use server-minted tickets. Forward their original
    // credentials and let the server validate them; never add Basic auth.
    if !authorized && !upgrade {
        return Ok(response(StatusCode::UNAUTHORIZED, "Unauthorized"));
    }
    let selected = selection(request.method().as_str(), request.uri().path(), state.beta);
    let sequence = selected
        .as_ref()
        .map(|_| state.sequence.fetch_add(1, Ordering::SeqCst) + 1);
    let url = format!(
        "{}{}",
        state.connection.url.trim_end_matches('/'),
        request
            .uri()
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/")
    );
    let upgraded = upgrade.then(|| hyper::upgrade::on(&mut request));
    let (parts, incoming) = request.into_parts();
    let mut headers = parts.headers;
    headers.remove("host");
    if !upgrade {
        headers.remove("connection");
    }
    let remote = state
        .client
        .request(parts.method, url)
        .headers(headers)
        .body(reqwest::Body::wrap_stream(incoming.into_data_stream()))
        .send()
        .await?;
    let mut builder = Response::builder().status(remote.status());
    for (key, value) in remote.headers() {
        if key != "transfer-encoding" && (upgrade || key != "connection") {
            builder = builder.header(key, value);
        }
    }
    if remote.status() == StatusCode::SWITCHING_PROTOCOLS
        && let Some(local) = upgraded
    {
        let mut upstream = remote.upgrade().await?;
        let mut closed = state.closed.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = closed.changed() => {},
                _ = async {
                    if let Ok(local) = local.await {
                        let _ = tokio::io::copy_bidirectional(&mut TokioIo::new(local), &mut upstream).await;
                    }
                } => {},
            }
        });
        return Ok(builder.body(body(Bytes::new()))?);
    }
    if remote.status().is_success()
        && let Some(id) = selected
    {
        if id.is_empty() {
            // Create/fork replies are metadata only; no transcript is parsed.
            let bytes = remote.bytes().await?;
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Ok(thread) = client_sessions::parse_opencode(
                    if state.beta { &value["data"] } else { &value },
                    &state.connection.directory,
                )
                && sequence == Some(state.sequence.load(Ordering::SeqCst))
            {
                state.select(thread);
            }
            return Ok(builder.body(body(bytes))?);
        }
        // Repeated message reads on the selected session need no metadata
        // request: its existing event stream supplies subsequent changes.
        if state
            .selected
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|thread| thread.id != id)
        {
            let state = state.clone();
            tokio::spawn(async move {
                let metadata = async {
                    let url = format!(
                        "{}{}/session/{id}",
                        state.connection.url.trim_end_matches('/'),
                        if state.beta { "/api" } else { "" }
                    );
                    let mut request = state
                        .client
                        .get(url)
                        .basic_auth(&state.connection.username, Some(&state.connection.password))
                        .timeout(Duration::from_secs(10));
                    if !state.beta {
                        request = request.query(&[("directory", &state.connection.directory)]);
                    }
                    let value: serde_json::Value =
                        request.send().await?.error_for_status()?.json().await?;
                    client_sessions::parse_opencode(
                        if state.beta { &value["data"] } else { &value },
                        &state.connection.directory,
                    )
                };
                if let Ok(thread) = metadata.await
                    && sequence == Some(state.sequence.load(Ordering::SeqCst))
                    && !*state.closed.borrow()
                {
                    state.select(thread);
                }
            });
        }
    }
    let event_stream = remote
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    let mut events = Events::default();
    let stream = remote.bytes_stream().map(move |chunk| {
        if event_stream && let Ok(bytes) = &chunk {
            events.feed(bytes, |event| state.event(&event));
        }
        chunk.map(Frame::data).map_err(|e| -> Error { Box::new(e) })
    });
    Ok(builder.body(BodyExt::boxed_unsync(StreamBody::new(stream)))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streamed_titles_and_status_only_update_the_selected_conversation() {
        for beta in [false, true] {
            let mut thread = client_sessions::parse_opencode(
                &serde_json::json!({"id":"ses_exact","title":"New Thread"}),
                "/app",
            )
            .unwrap();
            let value = if beta {
                serde_json::json!({"type":"session.renamed","data":{"sessionID":"ses_exact","title":"Weather 🌧"}})
            } else {
                serde_json::json!({"type":"session.updated","properties":{"info":{"id":"ses_exact","title":"Weather 🌧"}}})
            };
            let mut events = Events::default();
            let wire = format!("data: {value}\r\n\r\n");
            let mut seen = 0;
            for byte in wire.as_bytes() {
                events.feed(&[*byte], |event| {
                    assert!(update_thread(&mut thread, &event, beta));
                    seen += 1;
                });
            }
            assert_eq!(seen, 1);
            assert_eq!(thread.title, "Weather 🌧");
            let key = if beta { "data" } else { "properties" };
            assert!(!update_thread(
                &mut thread,
                &serde_json::json!({"type":"session.status",key:{"sessionID":"ses_other","status":{"type":"busy"}}}),
                beta
            ));
            assert!(update_thread(
                &mut thread,
                &serde_json::json!({"type":"session.status",key:{"sessionID":"ses_exact","status":{"type":"busy"}}}),
                beta
            ));
            assert_eq!(thread.state, "working");
            assert_eq!(thread.id, "ses_exact");
        }
    }

    #[test]
    fn browsing_and_background_events_do_not_select_a_thread() {
        for beta in [false, true] {
            let root = if beta { "/api/session" } else { "/session" };
            assert_eq!(selection("GET", root, beta), None);
            assert_eq!(selection("POST", root, beta), Some(String::new()));
            assert_eq!(
                selection("POST", &format!("{root}/ses_selected/prompt"), beta),
                Some("ses_selected".into())
            );
            assert_eq!(
                selection("POST", &format!("{root}/ses_other/rename"), beta),
                None
            );
            assert_eq!(selection("GET", &format!("{root}/ses_other"), beta), None);
            assert_eq!(
                selection("POST", &format!("{root}/ses_selected/fork"), beta),
                Some(String::new())
            );
        }
    }

    #[tokio::test]
    async fn bridge_preserves_auth_streaming_and_exact_created_thread_identity() {
        for beta in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let mut peers = tokio::task::JoinSet::new();
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    peers.spawn(async move {
                        let service = service_fn(move |request: Request<Incoming>| async move {
                            assert_eq!(request.headers()["authorization"], "Basic dXNlcjpwYXNz");
                            let event = request.uri().path().ends_with("event");
                            let (parts, incoming) = request.into_parts();
                            let payload = incoming.collect().await.unwrap().to_bytes();
                            let reply = if event {
                                let stream = futures_util::stream::once(async { Ok::<_, Error>(Frame::data(Bytes::from_static(b"data: still-live\n\n"))) })
                                    .chain(futures_util::stream::pending());
                                Response::builder().header("content-type", "text/event-stream")
                                    .body(BodyExt::boxed_unsync(StreamBody::new(stream))).unwrap()
                            } else if parts.method == "POST" {
                                assert_eq!(payload, "{\"title\":\"test\"}");
                                let info = serde_json::json!({"id":"ses_exact", "title":"Generated title", "directory":"/app", "time":{"created":1000,"updated":2000}});
                                let value = if beta { serde_json::json!({"data":info}) } else { info };
                                Response::new(body(value.to_string()))
                            } else { Response::new(body("[]")) };
                            Ok::<_, Infallible>(reply)
                        });
                        let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
                    });
                }
            });
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let bridge = Bridge::bind(
                super::super::Connection {
                    url,
                    username: "user".into(),
                    password: "pass".into(),
                    directory: "/app".into(),
                    reused: true,
                },
                beta,
                move |thread| {
                    let _ = tx.send(thread);
                },
            )
            .unwrap();
            let client = reqwest::Client::new();
            let endpoint = format!("{}{}session", bridge.url, if beta { "/api/" } else { "/" });
            assert_eq!(
                client.get(&endpoint).send().await.unwrap().status(),
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                client
                    .get(&endpoint)
                    .basic_auth("user", Some("pass"))
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap(),
                "[]"
            );
            assert!(rx.try_recv().is_err());
            client
                .post(&endpoint)
                .basic_auth("user", Some("pass"))
                .body("{\"title\":\"test\"}")
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            let selected = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(selected.id, "ses_exact");
            assert_eq!(selected.title, "Generated title");
            let mut events = client
                .get(format!("{}/event", bridge.url))
                .basic_auth("user", Some("pass"))
                .send()
                .await
                .unwrap()
                .bytes_stream();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), events.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                "data: still-live\n\n"
            );
            drop(bridge);
            assert!(
                tokio::time::timeout(Duration::from_secs(2), events.next())
                    .await
                    .is_ok()
            );
            server.abort();
        }
    }
}
