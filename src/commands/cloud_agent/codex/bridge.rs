//! Per-pane loopback bridge for the native Codex client. Observe only the
//! replies to that client's thread selections, so /new and /resume update the
//! CA row without guessing from account-wide activity or terminal text.
use anyhow::{Context, Result};
use async_tungstenite::tungstenite::handshake::server::{Request, Response};
use futures_util::{SinkExt, StreamExt};
use reqwest_websocket::{Message, RequestBuilderExt};
use serde_json::Value;
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio_util::compat::TokioAsyncReadCompatExt;

pub(crate) struct Bridge {
    pub url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Bridge {
    pub(crate) fn start(
        connection: super::Connection,
        notify: impl Fn(super::super::client_sessions::Thread) + Send + Sync + 'static,
    ) -> Result<Self> {
        super::validate_url(&connection.url)?;
        Self::bind(connection, notify)
    }

    fn bind(
        connection: super::Connection,
        notify: impl Fn(super::super::client_sessions::Thread) + Send + Sync + 'static,
    ) -> Result<Self> {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let url = format!("ws://{}", listener.local_addr()?);
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let notify = Arc::new(notify);
        let task = tokio::spawn(async move {
            let mut peers = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    incoming = listener.accept() => {
                        let Ok((stream, _)) = incoming else { break; };
                        let connection = connection.clone();
                        let notify = notify.clone();
                        peers.spawn(async move { let _ = relay(stream, &connection, notify.as_ref()).await; });
                    }
                    _ = peers.join_next(), if !peers.is_empty() => {}
                }
            }
        });
        Ok(Self { url, task })
    }
}

// Tungstenite's handshake callback requires an unboxed HTTP error response.
#[allow(clippy::result_large_err)]
async fn relay(
    stream: tokio::net::TcpStream,
    connection: &super::Connection,
    notify: &(impl Fn(super::super::client_sessions::Thread) + Send + Sync),
) -> Result<()> {
    let expected = format!("Bearer {}", connection.token);
    let mut local = tokio::time::timeout(
        Duration::from_secs(10),
        async_tungstenite::accept_hdr_async(
            stream.compat(),
            move |request: &Request, response: Response| {
                if request.uri().path() != "/"
                    || request
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        != Some(expected.as_str())
                {
                    let mut denied = async_tungstenite::tungstenite::http::Response::new(Some(
                        "Unauthorized".into(),
                    ));
                    *denied.status_mut() =
                        async_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED;
                    return Err(denied);
                }
                Ok(response)
            },
        ),
    )
    .await
    .context("Codex local handshake timed out")??;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut remote = tokio::time::timeout(Duration::from_secs(10), async {
        client
            .get(&connection.url)
            .bearer_auth(&connection.token)
            .upgrade()
            .send()
            .await?
            .into_websocket()
            .await
    })
    .await
    .context("Codex remote connection timed out")??;
    let mut selections = Selections::default();
    loop {
        tokio::select! {
            frame = local.next() => {
                let Some(frame) = frame else { break; };
                let message: Message = frame?.try_into()?;
                if let Message::Text(text) = &message { selections.request(text); }
                remote.send(message).await?;
            }
            frame = remote.next() => {
                let Some(frame) = frame else { break; };
                let message = frame?;
                if let Message::Text(text) = &message
                    && let Some(thread) = selections.response(text) { notify(thread); }
                local.send(message.into()).await?;
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct Selections {
    pending: HashSet<String>,
}

impl Selections {
    fn request(&mut self, text: &str) {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return;
        };
        // The native TUI generates titles with an ephemeral thread/start on
        // this same connection. That internal request never changes its view.
        if value["params"]["ephemeral"].as_bool() == Some(true) {
            return;
        }
        if matches!(
            value["method"].as_str(),
            Some("thread/start" | "thread/resume" | "thread/fork")
        ) && let Some(id) = value.get("id")
        {
            self.pending.insert(id.to_string());
        }
    }

    fn response(&mut self, text: &str) -> Option<super::super::client_sessions::Thread> {
        let value: Value = serde_json::from_str(text).ok()?;
        if !self.pending.remove(&value.get("id")?.to_string()) {
            return None;
        }
        super::super::client_sessions::parse_codex(&value["result"]["thread"]).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_this_clients_successful_selection_changes_its_row() {
        let mut selections = Selections::default();
        selections.request(r#"{"id":1,"method":"thread/start"}"#);
        selections.request(r#"{"id":2,"method":"thread/list"}"#);
        let reply = |id| {
            serde_json::json!({"id":id,"result":{"thread":{"id":"thread-1","cwd":"/app","preview":"Repair build"}}}).to_string()
        };
        assert!(selections.response(&reply(2)).is_none());
        assert!(
            selections
                .response(
                    r#"{"method":"thread/started","params":{"thread":{"id":"someone-else"}}}"#
                )
                .is_none()
        );
        assert_eq!(selections.response(&reply(1)).unwrap().id, "thread-1");
        assert!(selections.response(&reply(1)).is_none());
        selections.request(r#"{"id":"title-generator","method":"thread/start","params":{"ephemeral":true,"threadSource":"system"}}"#);
        assert!(selections.response(r#"{"id":"title-generator","result":{"thread":{"id":"internal-title-thread","cwd":"/app"}}}"#).is_none());
        selections.request(r#"{"id":"next","method":"thread/resume"}"#);
        assert!(
            selections
                .response(r#"{"id":"next","error":{"code":-1}}"#)
                .is_none()
        );
        selections.request(r#"{"id":3,"method":"thread/resume"}"#);
        assert_eq!(
            selections.response(&reply(3)).unwrap().title,
            "Repair build"
        );
    }

    #[tokio::test]
    #[allow(clippy::result_large_err)]
    async fn bridge_authenticates_forwards_and_closes_with_its_pane() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = async_tungstenite::accept_hdr_async(
                stream.compat(),
                |request: &Request, response: Response| {
                    assert_eq!(request.headers()["authorization"], "Bearer test-token");
                    Ok(response)
                },
            )
            .await
            .unwrap();
            let request = ws.next().await.unwrap().unwrap();
            let body: Value = serde_json::from_str(request.to_text().unwrap()).unwrap();
            assert_eq!(body["method"], "thread/start");
            ws.send(async_tungstenite::tungstenite::Message::Text(serde_json::json!({"id":body["id"],"result":{"thread":{"id":"thread-1","cwd":"/app","name":"Native title"}}}).to_string().into())).await.unwrap();
            let _ = ws.next().await;
            let _ = closed_tx.send(());
        });
        let connection = super::super::Connection {
            url: format!("ws://{addr}"),
            token: "test-token".into(),
            directory: "/app".into(),
            version: "0.154.0".into(),
            reused: true,
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let bridge = Bridge::bind(connection, move |thread| {
            let _ = tx.send(thread);
        })
        .unwrap();
        let client = reqwest::Client::new();
        let denied = client.get(&bridge.url).upgrade().send().await.unwrap();
        assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);
        let mut ws = client
            .get(&bridge.url)
            .bearer_auth("test-token")
            .upgrade()
            .send()
            .await
            .unwrap()
            .into_websocket()
            .await
            .unwrap();
        ws.send(Message::Text(
            r#"{"id":7,"method":"thread/start","params":{"cwd":"/app"}}"#.into(),
        ))
        .await
        .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(reply, Message::Text(text) if text.contains("Native title")));
        let thread = rx.recv().await.unwrap();
        assert_eq!(thread.id, "thread-1");
        drop(bridge);
        tokio::time::timeout(Duration::from_secs(2), closed_rx)
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
    }
}
