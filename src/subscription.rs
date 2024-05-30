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
    let hostname = configs.get_host();
    let mut request = format!("wss://backboard.{hostname}/graphql/v2").into_client_request()?;
    let headers = request.headers_mut();
    if let Some(token) = &Configs::get_railway_token() {
        headers.insert("project-access-token", HeaderValue::from_str(token)?);
    } else if let Some(token) = configs.get_railway_auth_token() {
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
    } else {
        bail!("Not authorized");
    }
    headers.insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_str("graphql-transport-ws").unwrap(),
    );

    let (connection, _) = async_tungstenite::tokio::connect_async(request).await?;

    Ok(Client::build(connection)
        .subscribe(StreamingOperation::<T>::new(variables))
        .await?)
}
