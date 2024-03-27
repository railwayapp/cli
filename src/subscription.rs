use crate::commands::Configs;
use anyhow::{bail, Result};
use async_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue};
use graphql_client::GraphQLQuery;
use graphql_ws_client::{graphql::StreamingOperation, Client, Subscription};

pub async fn subscribe_graphql<T: GraphQLQuery + Send + Sync + Unpin + 'static>(
    variables: T::Variables,
) -> Result<Subscription<StreamingOperation<T>>>
where
    <T as GraphQLQuery>::Variables: Send + Sync + Unpin,
    <T as GraphQLQuery>::ResponseData: std::fmt::Debug,
{
    let configs = Configs::new()?;
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

    Ok(Client::build(connection)
        .subscribe(StreamingOperation::<T>::new(variables))
        .await?)
}
