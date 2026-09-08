use std::time::Duration;

use crate::commands::Configs;
use anyhow::{Result, bail};
use futures::{SinkExt, StreamExt};
use graphql_client::GraphQLQuery;
use graphql_ws_client::{Client, Subscription, graphql::StreamingOperation};
use reqwest_websocket::{RequestBuilderExt, WebSocket};

pub async fn subscribe_graphql<T: GraphQLQuery + Send + Sync + Unpin + 'static>(
    variables: T::Variables,
) -> Result<Subscription<StreamingOperation<T>>>
where
    <T as GraphQLQuery>::Variables: Send + Sync + Unpin,
    <T as GraphQLQuery>::ResponseData: std::fmt::Debug,
{
    let mut configs = Configs::new()?;
    let hostname = configs.get_host();
    let oauth_base_url = crate::oauth::get_oauth_base_url(hostname);
    let (header_name, header_value) = connect_auth_header(&mut configs, &oauth_base_url).await?;

    let client = reqwest::Client::default();
    // This timeout covers the whole upgrade handshake (DNS + TCP + TLS +
    // 101). On a CPU-starved machine — a CI runner at full load, or a
    // Railway VM mid-build — 1s is routinely missed even when the network
    // is fine, and every retry misses it the same way.
    let request = client
        .get(format!("wss://backboard.{hostname}/graphql/v2"))
        .timeout(Duration::from_secs(10))
        .header(header_name, header_value);

    let resp = request
        .upgrade()
        .protocols(["graphql-transport-ws"])
        .send()
        .await?;
    resp.error_for_status_ref()?;
    let web_socket = resp.into_websocket().await?;

    Ok(Client::build(GraphQLWebSocket(web_socket))
        .subscribe(StreamingOperation::<T>::new(variables))
        .await?)
}

/// The credential header a (re)connect should present, after making sure it is
/// current.
///
/// A WebSocket authenticates once, at the upgrade — so a connection opened with
/// a good token keeps working past its expiry, and the staleness only bites on
/// reconnect. `stream_http_logs_inner` retries a dropped stream up to twelve
/// times, and without this refresh every one of those attempts re-presents the
/// same expired bearer and fails the handshake identically: a `logs -f` that
/// outlives its token dies at the first network blip. That gets worse, not
/// better, with a subscription meant to stay open for a whole session.
///
/// A refresh failure is deliberately not fatal. The stored token may still be
/// good (this only fires once local expiry has passed, which is conservative),
/// and the handshake reports a genuinely dead credential better than a
/// speculative refresh does.
///
/// Split out from [`subscribe_graphql`] so the refresh can be tested without
/// standing up a WebSocket server.
pub(crate) async fn connect_auth_header(
    configs: &mut Configs,
    oauth_base_url: &str,
) -> Result<(&'static str, String)> {
    let _ = crate::client::ensure_valid_token_at(configs, oauth_base_url).await;

    if let Some(token) = Configs::get_railway_token() {
        return Ok(("project-access-token", token));
    }
    if let Some(token) = configs.get_railway_auth_token() {
        return Ok(("authorization", format!("Bearer {token}")));
    }
    bail!("Not authorized")
}

struct GraphQLWebSocket(WebSocket);

impl graphql_ws_client::Connection for GraphQLWebSocket {
    fn receive(&mut self) -> impl Future<Output = Option<graphql_ws_client::Message>> + Send {
        use graphql_ws_client::Message as M2;
        use reqwest_websocket::Message as M1;
        async {
            let message = self.0.next().await?.ok()?;
            Some(match message {
                M1::Text(t) => M2::Text(t),
                M1::Binary(_) => None?,
                M1::Ping(_) => M2::Ping,
                M1::Pong(_) => M2::Pong,
                M1::Close { code, reason } => M2::Close {
                    code: Some(code.into()),
                    reason: Some(reason),
                },
            })
        }
    }

    fn send(
        &mut self,
        message: graphql_ws_client::Message,
    ) -> impl Future<Output = std::result::Result<(), graphql_ws_client::Error>> + Send {
        use graphql_ws_client::{Error as E2, Message as M2};
        use reqwest_websocket::{Error as E1, Message as M1};
        async {
            let message = match message {
                M2::Text(t) => M1::Text(t),
                M2::Close { code, reason } => M1::Close {
                    code: code.unwrap_or(0).into(),
                    reason: reason.unwrap_or_default(),
                },
                M2::Ping => M1::Ping(Default::default()),
                M2::Pong => M1::Pong(Default::default()),
            };

            self.0.send(message).await.map_err(|e| match e {
                E1::Handshake(handshake_error) => {
                    E2::Custom("Handshake Error".into(), handshake_error.to_string())
                }
                E1::Reqwest(error) => E2::Custom("Reqwest Error".into(), error.to_string()),
                E1::Tungstenite(error) => E2::Custom("Tungstenite Error".into(), error.to_string()),
                e => E2::Send(e.to_string()),
            })?;

            Ok(())
        }
    }
}
