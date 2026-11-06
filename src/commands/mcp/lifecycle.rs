//! Lets `railway mcp local` accept clients that open with the 2026-07-28
//! (SEP-2243) lifecycle.
//!
//! A modern client probes with `server/discover` before it sends `initialize`.
//! rmcp 1.x's server handshake only accepts `ping` ahead of `initialize`, so it
//! failed the whole process with "expect initialized request" — gemini_cli,
//! antigravity, devin and acp-mcp-bridge all hit this, ~18k times in 14 days.
//!
//! The 2026-07-28 backward-compatibility rule is that any correlated JSON-RPC
//! error to the discover probe (other than an unsupported-protocol-version
//! answer that lists a modern version) marks the server as legacy, and the
//! client falls back to `initialize` on the same connection. Both reference
//! SDKs implement it that way: rmcp 3.x's `ClientLifecycleMode::Auto` and the
//! TypeScript client's `classifyRpcError`. `-32601 Method not found` is the
//! canonical form, and it is what mcp.railway.com itself answers today (see the
//! SEP-2243 note in `proxy.rs`).
//!
//! This server only speaks the legacy session lifecycle, so answering with a
//! `DiscoverResult` would invite a client to skip `initialize`, which rmcp 1.x
//! cannot serve. The error keeps the process alive and routes the client to
//! the handshake rmcp does support.

use std::future::Future;

use rmcp::{
    RoleServer,
    model::{ClientJsonRpcMessage, ClientRequest, ErrorCode, ErrorData, ServerJsonRpcMessage},
    transport::Transport,
};

const DISCOVER_METHOD: &str = "server/discover";

/// Wraps the server's transport and answers a `server/discover` probe that
/// arrives before `initialize`, so rmcp's handshake never sees it. Every other
/// message, and everything after `initialize`, passes through untouched (a
/// later discover reaches the handler's default custom-request path, which
/// already answers method-not-found).
pub(super) struct LegacyLifecycle<T> {
    inner: T,
    initialize_seen: bool,
}

impl<T> LegacyLifecycle<T> {
    pub(super) fn new(inner: T) -> Self {
        Self {
            inner,
            initialize_seen: false,
        }
    }
}

fn discover_fallback_error() -> ErrorData {
    ErrorData::new(
        ErrorCode::METHOD_NOT_FOUND,
        "Method not found: server/discover. This server uses the initialize handshake.",
        None,
    )
}

impl<T> Transport<RoleServer> for LegacyLifecycle<T>
where
    T: Transport<RoleServer>,
{
    type Error = T::Error;

    fn send(
        &mut self,
        item: ServerJsonRpcMessage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
        loop {
            let msg = self.inner.receive().await?;
            if self.initialize_seen {
                return Some(msg);
            }
            if let ClientJsonRpcMessage::Request(req) = &msg {
                match &req.request {
                    ClientRequest::CustomRequest(custom) if custom.method == DISCOVER_METHOD => {
                        let reply =
                            ServerJsonRpcMessage::error(discover_fallback_error(), req.id.clone());
                        // A write failure means the client is gone; end the
                        // stream so rmcp reports a closed connection.
                        self.inner.send(reply).await.ok()?;
                        continue;
                    }
                    ClientRequest::InitializeRequest(_) => self.initialize_seen = true,
                    _ => {}
                }
            }
            return Some(msg);
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Configs;
    use rmcp::ServiceExt;
    use rmcp::transport::async_rw::AsyncRwTransport;
    use serde_json::{Value, json};
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::super::handler::RailwayMcp;

    async fn send_line(writer: &mut (impl AsyncWriteExt + Unpin), msg: Value) {
        writer
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .unwrap();
        writer.flush().await.unwrap();
    }

    async fn read_message(
        lines: &mut tokio::io::Lines<BufReader<impl tokio::io::AsyncRead + Unpin>>,
    ) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("server did not answer in time")
            .unwrap()
            .expect("server closed stdout");
        serde_json::from_str(&line).unwrap()
    }

    /// The exact client opening from the telemetry: `server/discover` (with
    /// empty params, as gemini_cli and friends send it), then `initialize`.
    #[tokio::test]
    async fn discover_before_initialize_falls_back_to_the_handshake() {
        let config_dir = tempfile::tempdir().unwrap();
        let configs = Configs::for_test(config_dir.path().join("config.json"));
        let handler = RailwayMcp::new(reqwest::Client::new(), configs);

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (server_read, server_write) = tokio::io::split(server_io);
        let transport =
            LegacyLifecycle::new(AsyncRwTransport::new_server(server_read, server_write));
        let server = tokio::spawn(async move { handler.serve(transport).await });

        let (client_read, mut client_write) = tokio::io::split(client_io);
        let mut lines = BufReader::new(client_read).lines();

        send_line(
            &mut client_write,
            json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {}}),
        )
        .await;
        let discover = read_message(&mut lines).await;
        assert_eq!(discover["id"], 1);
        assert_eq!(discover["error"]["code"], -32601);
        assert!(discover.get("result").is_none());

        send_line(
            &mut client_write,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "gemini-cli-mcp-client", "version": "0.1.0"}
                }
            }),
        )
        .await;
        let init = read_message(&mut lines).await;
        assert_eq!(init["id"], 2);
        assert_eq!(init["result"]["serverInfo"]["name"], "railway");
        assert!(init["result"]["capabilities"]["tools"].is_object());

        // The handshake completed, so the service is up and serving requests.
        let service = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("handshake did not complete")
            .unwrap()
            .expect("server failed to start after server/discover");
        send_line(
            &mut client_write,
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
        send_line(
            &mut client_write,
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
        )
        .await;
        let tools = read_message(&mut lines).await;
        assert_eq!(tools["id"], 3);
        assert!(!tools["result"]["tools"].as_array().unwrap().is_empty());

        service.cancel().await.unwrap();
    }

    /// A discover after the handshake is the handler's business, not the
    /// shim's: it must pass through (and still not kill the session).
    #[tokio::test]
    async fn discover_after_initialize_passes_through() {
        let config_dir = tempfile::tempdir().unwrap();
        let configs = Configs::for_test(config_dir.path().join("config.json"));
        let handler = RailwayMcp::new(reqwest::Client::new(), configs);

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (server_read, server_write) = tokio::io::split(server_io);
        let transport =
            LegacyLifecycle::new(AsyncRwTransport::new_server(server_read, server_write));
        let server = tokio::spawn(async move { handler.serve(transport).await });

        let (client_read, mut client_write) = tokio::io::split(client_io);
        let mut lines = BufReader::new(client_read).lines();

        send_line(
            &mut client_write,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "0"}
                }
            }),
        )
        .await;
        assert_eq!(read_message(&mut lines).await["id"], 1);
        let service = server.await.unwrap().unwrap();
        send_line(
            &mut client_write,
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
        send_line(
            &mut client_write,
            json!({"jsonrpc": "2.0", "id": 2, "method": "server/discover", "params": {}}),
        )
        .await;
        let discover = read_message(&mut lines).await;
        assert_eq!(discover["id"], 2);
        assert_eq!(discover["error"]["code"], -32601);

        service.cancel().await.unwrap();
    }
}
