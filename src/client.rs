use std::time::Duration;

use graphql_client::GraphQLQuery;
use reqwest::{
    Client,
    header::{HeaderMap, HeaderValue},
};

use crate::{commands::Environment, config::Configs, consts, errors::RailwayError};
use anyhow::Result;

use graphql_client::Response as GraphQLResponse;

pub struct GQLClient;

impl GQLClient {
    pub fn new_authorized(configs: &Configs) -> Result<Client, RailwayError> {
        let mut headers = HeaderMap::new();
        if let Some(token) = &Configs::get_railway_token() {
            headers.insert("project-access-token", HeaderValue::from_str(token)?);
        } else if let Some(token) = configs.get_railway_auth_token() {
            headers.insert(
                "authorization",
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        } else {
            return Err(RailwayError::Unauthorized);
        }
        headers.insert(
            "x-source",
            HeaderValue::from_static(consts::get_user_agent()),
        );
        let client = Client::builder()
            .danger_accept_invalid_certs(matches!(Configs::get_environment_id(), Environment::Dev))
            .user_agent(consts::get_user_agent())
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
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
) -> Result<Q::ResponseData, RailwayError> {
    let body = Q::build_query(variables);
    let response = client.post(url).json(&body).send().await?;
    if response.status() == 429 {
        return Err(RailwayError::Ratelimited);
    }
    let res: GraphQLResponse<Q::ResponseData> = response.json().await?;
    if let Some(errors) = res.errors {
        if errors[0].message.to_lowercase().contains("not authorized") {
            // Handle unauthorized errors in a custom way
            Err(RailwayError::Unauthorized)
        } else {
            Err(RailwayError::GraphQLError(errors[0].message.clone()))
        }
    } else if let Some(data) = res.data {
        Ok(data)
    } else {
        Err(RailwayError::MissingResponseData)
    }
}

/// Like post_graphql, but removes null values from the variables object before sending.
///
/// This is needed because graphql-client 0.14.0 has a bug where skip_serializing_none
/// doesn't work for root-level variables (only nested ones). This causes None values
/// to be serialized as null, which tells the Railway API to unset fields.
///
/// By stripping nulls from the JSON, we ensure the API receives undefined instead,
/// which preserves existing values (e.g., cron schedules on function updates).
pub async fn post_graphql_skip_none<Q: GraphQLQuery, U: reqwest::IntoUrl>(
    client: &reqwest::Client,
    url: U,
    variables: Q::Variables,
) -> Result<Q::ResponseData, RailwayError> {
    let body = Q::build_query(variables);

    let mut body_json =
        serde_json::to_value(&body).expect("Failed to serialize GraphQL query body");

    if let Some(obj) = body_json.as_object_mut() {
        if let Some(vars) = obj.get_mut("variables").and_then(|v| v.as_object_mut()) {
            vars.retain(|_, v| !v.is_null());
        }
    }

    let response = client.post(url).json(&body_json).send().await?;
    if response.status() == 429 {
        return Err(RailwayError::Ratelimited);
    }
    let res: GraphQLResponse<Q::ResponseData> = response.json().await?;
    if let Some(errors) = res.errors {
        if errors[0].message.to_lowercase().contains("not authorized") {
            Err(RailwayError::Unauthorized)
        } else {
            Err(RailwayError::GraphQLError(errors[0].message.clone()))
        }
    } else if let Some(data) = res.data {
        Ok(data)
    } else {
        Err(RailwayError::MissingResponseData)
    }
}
