use crate::{commands::Configs, tokio_spawner::TokioSpawner};
use anyhow::{bail, Result};
use async_tungstenite::tungstenite::Message;
use graphql_client::GraphQLQuery;
use graphql_ws_client::{
    graphql::{GraphQLClient, StreamingOperation},
    AsyncWebsocketClient, SubscriptionStream,
};

pub async fn subscribe_graphql<T: GraphQLQuery + Send + Sync + Unpin + 'static>(
    variables: T::Variables,
) -> Result<(
    AsyncWebsocketClient<GraphQLClient, Message>,
    SubscriptionStream<GraphQLClient, StreamingOperation<T>>,
)>
where
    <T as GraphQLQuery>::Variables: Send + Sync + Unpin,
    <T as GraphQLQuery>::ResponseData: std::fmt::Debug,
{
    let configs = Configs::new()?;

    use async_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue};
    use futures::StreamExt;
    use graphql_ws_client::GraphQLClientClientBuilder;
    let Some(token) = configs.root_config.user.token.clone() else {
      bail!("Unauthorized. Please login with `railway login`")
    };
    let bearer = format!("Bearer {token}");
    let hostname = configs.get_host();
    let mut request = format!("wss://backboard.{hostname}/graphql/v2").into_client_request()?;

    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_str("graphql-transport-ws").unwrap(),
    );
    request
        .headers_mut()
        .insert("Authorization", HeaderValue::from_str(&bearer)?);

    let (connection, _) = async_tungstenite::tokio::connect_async(request).await?;

    let (sink, stream) = connection.split::<Message>();

    let mut client = GraphQLClientClientBuilder::new()
        .build(stream, sink, TokioSpawner::current())
        .await?;
    let stream = client
        .streaming_operation(StreamingOperation::<T>::new(variables))
        .await?;

    Ok((client, stream))
}
