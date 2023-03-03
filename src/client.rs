use graphql_client::{GraphQLQuery, Response};
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Client,
};

use crate::{commands::Environment, config::Configs, consts};
use anyhow::{bail, Result};

pub struct GQLClient;

impl GQLClient {
    pub fn new_authorized(configs: &Configs) -> Result<Client> {
        let mut headers = HeaderMap::new();
        if let Some(token) = &Configs::get_railway_token() {
            headers.insert("project-access-token", HeaderValue::from_str(token)?);
        } else if let Some(token) = &Configs::get_railway_api_token() {
            headers.insert(
                "authorization",
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        } else if let Some(token) = &configs.root_config.user.token {
            if token.is_empty() {
                bail!("Unauthorized. Please login with `railway login`")
            }
            headers.insert(
                "authorization",
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        } else {
            bail!("Unauthorized. Please login with `railway login`")
        }
        headers.insert(
            "x-source",
            HeaderValue::from_static(consts::get_user_agent()),
        );
        let client = Client::builder()
            .danger_accept_invalid_certs(matches!(Configs::get_environment_id(), Environment::Dev))
            .user_agent(consts::get_user_agent())
            .default_headers(headers)
            .build()?;
        Ok(client)
    }

    pub fn new_unauthorized() -> Result<Client> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-source",
            HeaderValue::from_static(consts::get_user_agent()),
        );
        let client = Client::builder()
            .danger_accept_invalid_certs(matches!(Configs::get_environment_id(), Environment::Dev))
            .user_agent(consts::get_user_agent())
            .default_headers(headers)
            .build()?;
        Ok(client)
    }
}

pub async fn post_graphql<Q: GraphQLQuery, U: reqwest::IntoUrl>(
    client: &reqwest::Client,
    url: U,
    variables: Q::Variables,
) -> Result<Response<Q::ResponseData>, reqwest::Error> {
    let body = Q::build_query(variables);
    let reqwest_response = client.post(url).json(&body).send().await?;

    reqwest_response.json().await
}
