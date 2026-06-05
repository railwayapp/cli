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
    let configs = Configs::new()?;
    let hostname = configs.get_host();
    let client = reqwest::Client::default();
    let mut request = client
        .get(format!("wss://backboard.{hostname}/graphql/v2"))
        .timeout(Duration::from_secs(1));

    if let Some(token) = &Configs::get_railway_token() {
        request = request.header("project-access-token", token);
    } else if let Some(token) = configs.get_railway_auth_token() {
        request = request.header("authorization", format!("Bearer {token}"));
    } else {
        bail!("Not authorized");
    };

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
