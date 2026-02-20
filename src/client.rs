use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    time::Duration,
};

use graphql_client::GraphQLQuery;
use reqwest::{
    Certificate, Client, ClientBuilder,
    header::{HeaderMap, HeaderValue},
};

use crate::{
    commands::Environment,
    config::Configs,
    consts::{self, RAILWAY_API_TOKEN_ENV, RAILWAY_TOKEN_ENV},
    errors::RailwayError,
};
use anyhow::Result;

use graphql_client::Response as GraphQLResponse;

pub struct GQLClient;
const RAILWAY_CA_CERT_FILE_ENV: &str = "RAILWAY_CA_CERT_FILE";
const RAILWAY_CA_CERT_DIR_ENV: &str = "RAILWAY_CA_CERT_DIR";

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
        let client = build_client(Client::builder())
            .danger_accept_invalid_certs(matches!(Configs::get_environment_id(), Environment::Dev))
            .user_agent(consts::get_user_agent())
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(RailwayError::FetchError)?;
        Ok(client)
    }

    pub fn new_unauthorized() -> Result<Client> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-source",
            HeaderValue::from_static(consts::get_user_agent()),
        );
        let client = build_client(Client::builder())
            .danger_accept_invalid_certs(matches!(Configs::get_environment_id(), Environment::Dev))
            .user_agent(consts::get_user_agent())
            .default_headers(headers)
            .build()?;
        Ok(client)
    }
}

fn build_client(mut builder: ClientBuilder) -> ClientBuilder {
    let native_certs = rustls_native_certs::load_native_certs();
    for native_cert in native_certs.certs {
        if let Ok(cert) = Certificate::from_der(native_cert.as_ref()) {
            builder = builder.add_root_certificate(cert);
        }
    }

    for err in native_certs.errors {
        eprintln!("warning: failed to load a native certificate: {err}");
    }

    for path in configured_ca_file_paths() {
        builder = add_root_certs_from_file(builder, &path);
    }

    for path in configured_ca_dir_paths() {
        builder = add_root_certs_from_dir(builder, &path);
    }

    builder
}

fn parse_pem_certificates(contents: &[u8]) -> Vec<Certificate> {
    let mut certs = Vec::new();

    if let Ok(cert) = Certificate::from_der(contents) {
        certs.push(cert);
    }

    if let Ok(cert) = Certificate::from_pem(contents) {
        certs.push(cert);
    }

    let mut cursor = Cursor::new(contents);
    for maybe_der in rustls_pemfile::certs(&mut cursor) {
        let Ok(der) = maybe_der else {
            continue;
        };
        if let Ok(cert) = Certificate::from_der(der.as_ref()) {
            certs.push(cert);
        }
    }

    certs
}

fn configured_ca_file_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for var in [
        RAILWAY_CA_CERT_FILE_ENV,
        "SSL_CERT_FILE",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
    ] {
        if let Some(path) = std::env::var_os(var) {
            paths.push(PathBuf::from(path));
        }
    }

    paths
}

fn configured_ca_dir_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for var in [RAILWAY_CA_CERT_DIR_ENV, "SSL_CERT_DIR"] {
        if let Some(path) = std::env::var_os(var) {
            paths.push(PathBuf::from(path));
        }
    }

    paths
}

fn add_root_certs_from_file(mut builder: ClientBuilder, ca_cert_path: &Path) -> ClientBuilder {
    match fs::read(ca_cert_path) {
        Ok(contents) => {
            let certs = parse_pem_certificates(&contents);
            if certs.is_empty() {
                eprintln!(
                    "warning: could not parse CA certs from {:?}; continuing with other trust roots",
                    ca_cert_path
                );
            } else {
                for cert in certs {
                    builder = builder.add_root_certificate(cert);
                }
            }
        }
        Err(err) => {
            eprintln!(
                "warning: failed to read CA cert file {:?}: {err}; continuing with other trust roots",
                ca_cert_path
            );
        }
    }

    builder
}

fn add_root_certs_from_dir(mut builder: ClientBuilder, ca_dir_path: &Path) -> ClientBuilder {
    let entries = match fs::read_dir(ca_dir_path) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!(
                "warning: failed to read CA cert directory {:?}: {err}; continuing with other trust roots",
                ca_dir_path
            );
            return builder;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        builder = add_root_certs_from_file(builder, &path);
    }

    builder
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
        let error = &errors[0];
        if error
            .message
            .to_lowercase()
            .contains("project token not found")
        {
            Err(RailwayError::InvalidRailwayToken(
                RAILWAY_TOKEN_ENV.to_string(),
            ))
        } else if error.message.to_lowercase().contains("not authorized") {
            // Handle unauthorized errors in a custom way
            Err(auth_failure_error())
        } else if error.message == "Two Factor Authentication Required" {
            // Extract workspace name from extensions if available
            let workspace_name = error
                .extensions
                .as_ref()
                .and_then(|ext| ext.get("workspaceName"))
                .and_then(|v| v.as_str())
                .unwrap_or("this workspace")
                .to_string();
            let security_url = get_security_url();
            Err(RailwayError::TwoFactorEnforcementRequired(
                workspace_name,
                security_url,
            ))
        } else {
            Err(RailwayError::GraphQLError(error.message.clone()))
        }
    } else if let Some(data) = res.data {
        Ok(data)
    } else {
        Err(RailwayError::MissingResponseData)
    }
}

fn get_security_url() -> String {
    let host = match Configs::get_environment_id() {
        Environment::Production => "railway.com",
        Environment::Staging => "railway-staging.com",
        Environment::Dev => "railway-develop.com",
    };
    format!("https://{}/account/security", host)
}

pub(crate) fn auth_failure_error() -> RailwayError {
    if Configs::get_railway_token().is_some() {
        RailwayError::UnauthorizedToken(RAILWAY_TOKEN_ENV.to_string())
    } else if Configs::get_railway_api_token().is_some() {
        RailwayError::UnauthorizedToken(RAILWAY_API_TOKEN_ENV.to_string())
    } else if Configs::new()
        .ok()
        .and_then(|configs| configs.get_railway_auth_token())
        .is_some()
    {
        RailwayError::UnauthorizedLogin
    } else {
        RailwayError::Unauthorized
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
        let error = &errors[0];
        if error
            .message
            .to_lowercase()
            .contains("project token not found")
        {
            Err(RailwayError::InvalidRailwayToken(
                RAILWAY_TOKEN_ENV.to_string(),
            ))
        } else if error.message.to_lowercase().contains("not authorized") {
            Err(auth_failure_error())
        } else if error.message == "Two Factor Authentication Required" {
            // Extract workspace name from extensions if available
            let workspace_name = error
                .extensions
                .as_ref()
                .and_then(|ext| ext.get("workspaceName"))
                .and_then(|v| v.as_str())
                .unwrap_or("this workspace")
                .to_string();
            let security_url = get_security_url();
            Err(RailwayError::TwoFactorEnforcementRequired(
                workspace_name,
                security_url,
            ))
        } else {
            Err(RailwayError::GraphQLError(error.message.clone()))
        }
    } else if let Some(data) = res.data {
        Ok(data)
    } else {
        Err(RailwayError::MissingResponseData)
    }
}
