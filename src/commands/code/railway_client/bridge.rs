//! Refresh gate authentication on every TUI dial, including session switches.
//! Only WebSocket traffic crosses the public agent URL; no SSH tunnel is used.
use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use async_tungstenite::tungstenite::handshake::server::{Request, Response};
use futures_util::{SinkExt, StreamExt, future::BoxFuture};
use reqwest_websocket::{Message, WebSocket};
use tokio_util::compat::TokioAsyncReadCompatExt;

type Open = Arc<dyn Fn(Option<String>) -> BoxFuture<'static, Result<WebSocket>> + Send + Sync>;

pub(super) struct Bridge {
    pub url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Bridge {
    pub(super) fn start(target: super::Target) -> Result<Self> {
        Self::bind(Arc::new(move |session| {
            let target = target.clone();
            Box::pin(async move { target.open(session.as_deref()).await })
        }))
    }

    fn bind(open: Open) -> Result<Self> {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let token = crate::commands::cloud_agent::opencode::generate_password();
        let url = format!(
            "ws://{}/_railway/agent?token={token}",
            listener.local_addr()?
        );
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let task = tokio::spawn(async move {
            let mut peers = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    incoming = listener.accept() => {
                        let Ok((stream, _)) = incoming else { break; };
                        if peers.len() >= 8 { continue; }
                        let open = open.clone();
                        let token = token.clone();
                        peers.spawn(async move {
                            // Do not print into the active terminal UI, or expose gate URLs
                            // with credentials. Closing the peer triggers the TUI's reconnect.
                            let _ = relay(stream, &token, open).await;
                        });
                    }
                    _ = peers.join_next(), if !peers.is_empty() => {}
                }
            }
        });
        Ok(Self { url, task })
    }
}

fn local_session(request: &Request, token: &str) -> Option<Option<String>> {
    if request.uri().path() != "/_railway/agent" || request.headers().contains_key("origin") {
        return None;
    }
    let pairs: Vec<_> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes()).collect();
    let tokens: Vec<_> = pairs.iter().filter(|(key, _)| key == "token").collect();
    if tokens.len() != 1 || tokens[0].1 != token {
        return None;
    }
    let sessions: Vec<_> = pairs
        .iter()
        .filter(|(key, _)| key == "session_id")
        .collect();
    if sessions.len() > 1
        || sessions
            .first()
            .is_some_and(|(_, id)| id.is_empty() || id.len() > 256)
    {
        return None;
    }
    Some(sessions.first().map(|(_, id)| id.to_string()))
}

#[allow(clippy::result_large_err)]
async fn relay(stream: tokio::net::TcpStream, token: &str, open: Open) -> Result<()> {
    let mut session = None;
    let mut local = tokio::time::timeout(
        Duration::from_secs(10),
        async_tungstenite::accept_hdr_async(
            stream.compat(),
            |request: &Request, response: Response| match local_session(request, token) {
                Some(selected) => {
                    session = selected;
                    Ok(response)
                }
                None => {
                    let mut denied = async_tungstenite::tungstenite::http::Response::new(Some(
                        "Unauthorized".into(),
                    ));
                    *denied.status_mut() =
                        async_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED;
                    Err(denied)
                }
            },
        ),
    )
    .await
    .context("Local Railway TUI handshake timed out")??;
    // The opener mints a new 5-minute gate token each time. An already-open
    // connection stays valid; we never disconnect an active turn to rotate it.
    let mut remote = tokio::time::timeout(Duration::from_secs(30), open(session))
        .await
        .context("Railway gate connection timed out")??;
    loop {
        tokio::select! {
            frame = local.next() => {
                let Some(frame) = frame else { break; };
                let message: Message = frame?.try_into()?;
                remote.send(message).await?;
            }
            frame = remote.next() => {
                let Some(frame) = frame else { break; };
                local.send(frame?.into()).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest_websocket::RequestBuilderExt;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn local_auth_is_required_and_session_switches_preserve_only_the_session() {
        let request = Request::builder()
            .uri("/_railway/agent?session_id=a%20b&token=secret")
            .body(())
            .unwrap();
        assert_eq!(local_session(&request, "secret"), Some(Some("a b".into())));
        for uri in [
            "/_railway/agent",
            "/agent?token=secret",
            "/_railway/agent?token=wrong",
            "/_railway/agent?token=secret&token=secret",
            "/_railway/agent?token=secret&session_id=",
        ] {
            let request = Request::builder().uri(uri).body(()).unwrap();
            assert!(local_session(&request, "secret").is_none());
        }
        let request = Request::builder()
            .uri("/_railway/agent?token=secret")
            .header("origin", "https://example.com")
            .body(())
            .unwrap();
        assert!(local_session(&request, "secret").is_none());
    }

    #[tokio::test]
    async fn redials_get_fresh_credentials_and_relay_frames_in_both_directions() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let observed = received.clone();
        let upstream = tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let observed = observed.clone();
                let mut ws = async_tungstenite::accept_hdr_async(
                    stream.compat(),
                    move |request: &Request, response: Response| {
                        observed.lock().unwrap().push(request.uri().to_string());
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                let message = ws.next().await.unwrap().unwrap();
                ws.send(message).await.unwrap();
                let _ = ws.next().await;
            }
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let minted = calls.clone();
        let bridge = Bridge::bind(Arc::new(move |session| {
            let n = minted.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                let mut url = url::Url::parse(&format!("ws://{address}/agent")).unwrap();
                url.query_pairs_mut()
                    .append_pair("token", &format!("fresh-{n}"));
                if let Some(session) = session {
                    url.query_pairs_mut().append_pair("session_id", &session);
                }
                super::super::open_gate(url).await
            })
        }))
        .unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut unauthenticated = url::Url::parse(&bridge.url).unwrap();
        unauthenticated.set_query(None);
        let rejected = client.get(unauthenticated).upgrade().send().await.unwrap();
        assert_eq!(rejected.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        for session in ["first", "second"] {
            let mut url = url::Url::parse(&bridge.url).unwrap();
            url.query_pairs_mut().append_pair("session_id", session);
            let mut ws = client
                .get(url)
                .upgrade()
                .send()
                .await
                .unwrap()
                .into_websocket()
                .await
                .unwrap();
            ws.send(Message::Text(session.into())).await.unwrap();
            let echoed = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(matches!(echoed, Message::Text(text) if text == session));
            SinkExt::close(&mut ws).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), upstream)
            .await
            .unwrap()
            .unwrap();
        let received = received.lock().unwrap();
        assert_eq!(
            *received,
            [
                "/agent?token=fresh-1&session_id=first",
                "/agent?token=fresh-2&session_id=second"
            ]
        );
    }
}
