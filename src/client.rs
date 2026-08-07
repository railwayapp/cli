use std::time::Duration;

use colored::Colorize;
use graphql_client::GraphQLQuery;
use reqwest::{
    Client,
    header::{HeaderMap, HeaderValue},
};
use serde::de::DeserializeOwned;

use crate::{
    commands::Environment,
    config::Configs,
    consts::{self, RAILWAY_API_TOKEN_ENV, RAILWAY_TOKEN_ENV},
    errors::RailwayError,
    oauth,
};
use anyhow::Result;

use graphql_client::Response as GraphQLResponse;

pub struct GQLClient;

impl GQLClient {
    pub fn new_public() -> Result<Client, RailwayError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-source",
            HeaderValue::from_static(consts::get_user_agent()),
        );

        Ok(Self::build_client(headers))
    }

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
        Ok(Self::build_client(headers))
    }

    pub fn new_user_authorized(configs: &Configs) -> Result<Client, RailwayError> {
        let mut headers = HeaderMap::new();
        if let Some(token) = configs.get_railway_auth_token() {
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
        Ok(Self::build_client(headers))
    }

    fn build_client(headers: HeaderMap) -> Client {
        Client::builder()
            .danger_accept_invalid_certs(matches!(Configs::get_environment_id(), Environment::Dev))
            .user_agent(consts::get_user_agent())
            .default_headers(headers)
            .timeout(Duration::from_secs(resolve_timeout_secs()))
            .build()
            .unwrap()
    }
}

/// Resolve the HTTP request timeout (in seconds).
///
/// Reads the `RAILWAY_HTTP_TIMEOUT` env var as an escape hatch for long-running
/// operations (e.g. duplicating a large environment). Falls back to
/// [`consts::DEFAULT_HTTP_TIMEOUT_SECS`] when unset, and surfaces a warning
/// (rather than silently ignoring) when the value can't be parsed as a positive
/// integer number of seconds.
fn resolve_timeout_secs() -> u64 {
    parse_timeout_secs(
        std::env::var(consts::RAILWAY_HTTP_TIMEOUT_ENV)
            .ok()
            .as_deref(),
    )
}

/// Parse a `RAILWAY_HTTP_TIMEOUT` value into a timeout in seconds.
///
/// `None` (env var unset) falls back to the default. A value that can't be parsed
/// as a positive integer is surfaced as a warning (rather than silently ignored)
/// and also falls back to the default.
fn parse_timeout_secs(raw: Option<&str>) -> u64 {
    let Some(raw) = raw else {
        return consts::DEFAULT_HTTP_TIMEOUT_SECS;
    };
    match raw.trim().parse::<u64>() {
        Ok(secs) if secs > 0 => secs,
        _ => {
            eprintln!(
                "{}",
                format!(
                    "Warning: ignoring invalid {}={raw:?}; expected a positive number of seconds, using {}s",
                    consts::RAILWAY_HTTP_TIMEOUT_ENV,
                    consts::DEFAULT_HTTP_TIMEOUT_SECS
                )
                .yellow()
            );
            consts::DEFAULT_HTTP_TIMEOUT_SECS
        }
    }
}

pub async fn post_graphql<Q: GraphQLQuery, U: reqwest::IntoUrl>(
    client: &reqwest::Client,
    url: U,
    variables: Q::Variables,
) -> Result<Q::ResponseData, RailwayError> {
    let body = Q::build_query(variables);
    let response = client.post(url).json(&body).send().await?;
    parse_graphql_response(response).await
}

pub async fn post_graphql_raw<T, U: reqwest::IntoUrl>(
    client: &reqwest::Client,
    url: U,
    query: &str,
    variables: serde_json::Value,
) -> Result<T, RailwayError>
where
    T: DeserializeOwned,
{
    let body = serde_json::json!({
        "query": query,
        "variables": variables,
    });
    let response = client.post(url).json(&body).send().await?;
    parse_graphql_response(response).await
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

/// Ensures the OAuth access token is still valid, refreshing if needed.
///
/// The refresh is a read-modify-write of the on-disk credentials, and the
/// backboard server rotates (and reuse-detects) the refresh token on every
/// refresh. If two CLI invocations refresh concurrently, the second presents
/// an already-consumed refresh token and the server revokes the entire grant —
/// a hard logout. To prevent this we serialize the refresh behind an exclusive
/// file lock on a dedicated lockfile under `~/.railway/`, then re-read the
/// config and re-check expiry after acquiring the lock: whichever process wins
/// the lock performs the single refresh, and the others pick up its result.
pub async fn ensure_valid_token(configs: &mut Configs) -> Result<()> {
    // Env var tokens are not managed by us
    if Configs::is_using_token_auth() {
        return Ok(());
    }

    // Fast path: nothing to refresh.
    if !configs.has_oauth_token() || !configs.is_token_expired() {
        return Ok(());
    }

    match refresh_locked(configs, false).await {
        RefreshOutcome::Refreshed | RefreshOutcome::AlreadyFresh => Ok(()),
        RefreshOutcome::SessionExpired(e) | RefreshOutcome::Transient(e) => Err(e.into()),
        RefreshOutcome::NoRefreshToken => {
            Err(RailwayError::OAuthRefreshFailed("No refresh token available".to_string()).into())
        }
    }
}

/// What a refresh attempt did to the stored credentials.
#[derive(Debug)]
pub(crate) enum RefreshOutcome {
    /// New tokens persisted.
    Refreshed,
    /// Another process refreshed while we waited for the lock; its result is now
    /// loaded and valid, so there was nothing left to do.
    AlreadyFresh,
    /// The grant is dead and the credentials have been cleared from disk. The
    /// user must run `railway login`; retrying will never succeed.
    SessionExpired(RailwayError),
    /// Refresh failed but the credentials are intact and still worth using. The
    /// command should proceed and may well succeed.
    Transient(RailwayError),
    /// Nothing to refresh with.
    NoRefreshToken,
}

/// Backoff between transient refresh attempts.
const REFRESH_BACKOFF: Duration = Duration::from_millis(250);

/// Take the config lock, re-read credentials, and refresh.
///
/// The lock matters because the refresh is a read-modify-write of the on-disk
/// credentials and backboard reuse-detects the refresh token: two processes
/// refreshing concurrently means the second presents an already-consumed token
/// and the server revokes the whole grant — a hard logout. Whichever process
/// wins the lock performs the single refresh; the others re-read and pick up its
/// result. If the lock cannot be taken we refresh anyway rather than wedge.
///
/// `force` skips the "someone else already did it" short-circuit, for callers
/// reacting to an actual authorization failure rather than to local expiry.
async fn refresh_locked(configs: &mut Configs, force: bool) -> RefreshOutcome {
    let _lock = configs.acquire_lock().await;

    if configs.reload().is_err() {
        return RefreshOutcome::NoRefreshToken;
    }
    if !force && (!configs.has_oauth_token() || !configs.is_token_expired()) {
        return RefreshOutcome::AlreadyFresh;
    }

    let base_url = oauth::get_oauth_base_url(configs.get_host());
    refresh_with_policy(configs, &base_url, REFRESH_BACKOFF).await
}

/// Refresh unconditionally, ignoring the local expiry timestamp.
///
/// `ensure_valid_token` trusts `tokenExpiresAt`, so it does nothing when the
/// stored token merely *looks* valid. A token can die server-side well before
/// that — the grant is revoked, or evicted by the per-grant refresh-token cap —
/// and in that state a long-lived process would otherwise never recover. Call
/// this after an actual authorization failure.
pub(crate) async fn force_refresh(configs: &mut Configs) -> RefreshOutcome {
    if Configs::is_using_token_auth() {
        // Env-var tokens are not ours to refresh.
        return RefreshOutcome::NoRefreshToken;
    }
    refresh_locked(configs, true).await
}

/// Refresh against an explicit base URL and apply the credential policy:
/// clear on a dead grant, preserve on anything transient.
pub(crate) async fn refresh_with_policy(
    configs: &mut Configs,
    base_url: &str,
    backoff: Duration,
) -> RefreshOutcome {
    let Some(refresh_token) = configs.get_refresh_token().map(str::to_owned) else {
        return RefreshOutcome::NoRefreshToken;
    };

    match oauth::refresh_access_token_at(base_url, &refresh_token, backoff).await {
        Ok(token_resp) => {
            if let Err(e) = configs.save_oauth_tokens(
                &token_resp.access_token,
                token_resp.refresh_token.as_deref(),
                token_resp.expires_in,
            ) {
                return RefreshOutcome::Transient(RailwayError::OAuthRefreshFailed(format!(
                    "failed to persist tokens: {e}"
                )));
            }
            RefreshOutcome::Refreshed
        }
        // A permanently-dead credential is discarded so the next run starts
        // clean instead of replaying the same doomed refresh. Transient failures
        // keep their tokens — clearing on those would turn a brief server blip
        // into a fleet-wide forced logout.
        Err(oauth::RefreshFailure::Terminal(err)) => {
            if let Err(clear_err) = configs.clear_oauth_tokens() {
                eprintln!(
                    "{}: {clear_err}",
                    "Warning: could not clear the expired login from disk".yellow()
                );
            }
            RefreshOutcome::SessionExpired(err)
        }
        Err(oauth::RefreshFailure::Transient(err)) => RefreshOutcome::Transient(err),
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
    parse_graphql_response(response).await
}

async fn parse_graphql_response<T>(response: reqwest::Response) -> Result<T, RailwayError>
where
    T: DeserializeOwned,
{
    if response.status() == 429 {
        return Err(RailwayError::Ratelimited);
    }
    let res: GraphQLResponse<T> = response.json().await?;
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

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;
    use crate::gql::queries;

    #[test]
    fn timeout_defaults_when_unset() {
        assert_eq!(parse_timeout_secs(None), consts::DEFAULT_HTTP_TIMEOUT_SECS);
    }

    #[test]
    fn timeout_uses_valid_override() {
        assert_eq!(parse_timeout_secs(Some("300")), 300);
        assert_eq!(parse_timeout_secs(Some("  90  ")), 90);
    }

    #[test]
    fn timeout_falls_back_on_invalid_values() {
        for bad in ["0", "-5", "abc", "12.5", ""] {
            assert_eq!(
                parse_timeout_secs(Some(bad)),
                consts::DEFAULT_HTTP_TIMEOUT_SECS,
                "expected fallback for {bad:?}"
            );
        }
    }

    fn spawn_graphql_server(
        response_for_request: impl FnOnce(String) -> String + Send + 'static,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            let mut content_length = 0usize;

            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                request.push_str(&line);

                if let Some(value) = line.strip_prefix("Content-Length:") {
                    content_length = value.trim().parse().unwrap();
                }

                if line == "\r\n" {
                    break;
                }
            }

            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            request.push_str(std::str::from_utf8(&body).unwrap());

            let response_body = response_for_request(request);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        format!("http://{addr}")
    }

    #[tokio::test]
    async fn public_client_can_query_templates_without_auth_headers() {
        let server_url = spawn_graphql_server(|request| {
            assert!(
                !request.to_ascii_lowercase().contains("authorization:"),
                "public template lookup should not send auth headers"
            );

            serde_json::json!({
                "data": {
                    "template": {
                        "id": "template-id",
                        "name": "PostgreSQL",
                        "serializedConfig": null
                    }
                }
            })
            .to_string()
        });

        let client = GQLClient::new_public().unwrap();
        let response = post_graphql::<queries::TemplateDetail, _>(
            &client,
            server_url,
            queries::template_detail::Variables {
                code: "postgres".to_string(),
            },
        )
        .await
        .unwrap();

        assert_eq!(response.template.id, "template-id");
        assert_eq!(response.template.name, "PostgreSQL");
        assert_eq!(response.template.serialized_config, None);
    }

    mod graphql_layer {
        use super::*;
        use crate::testkit::MockBackboard;
        use serde_json::json;

        #[tokio::test]
        async fn success_roundtrips_data_and_sends_variables() {
            let server = MockBackboard::spawn();
            server.stub(
                "WorkflowStatus",
                json!({ "workflowStatus": { "status": "Complete", "error": null } }),
            );

            let client = reqwest::Client::new();
            let response = post_graphql::<queries::WorkflowStatus, _>(
                &client,
                server.url(),
                queries::workflow_status::Variables {
                    workflow_id: "wf-42".to_string(),
                },
            )
            .await
            .unwrap();

            use queries::workflow_status::WorkflowStatus;
            assert!(matches!(
                response.workflow_status.status,
                WorkflowStatus::Complete
            ));
            assert_eq!(
                server.variables_for("WorkflowStatus"),
                vec![json!({ "workflowId": "wf-42" })]
            );
        }

        #[tokio::test]
        async fn graphql_errors_surface_the_servers_message() {
            let server = MockBackboard::spawn();
            server.stub_graphql_error("WorkflowStatus", "workflow blew up");

            let client = reqwest::Client::new();
            let err = post_graphql::<queries::WorkflowStatus, _>(
                &client,
                server.url(),
                queries::workflow_status::Variables {
                    workflow_id: "wf-42".to_string(),
                },
            )
            .await
            .unwrap_err();

            assert!(matches!(err, RailwayError::GraphQLError(ref m) if m == "workflow blew up"));
        }

        #[tokio::test]
        async fn missing_data_without_errors_is_reported_as_such() {
            let server = MockBackboard::spawn();
            server.stub_raw("WorkflowStatus", 200, r#"{"data": null}"#.to_string());

            let client = reqwest::Client::new();
            let err = post_graphql::<queries::WorkflowStatus, _>(
                &client,
                server.url(),
                queries::workflow_status::Variables {
                    workflow_id: "wf-42".to_string(),
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(err, RailwayError::MissingResponseData));
        }

        #[tokio::test]
        async fn http_429_maps_to_ratelimited() {
            let server = MockBackboard::spawn();
            server.stub_raw("WorkflowStatus", 429, r#"{"data": null}"#.to_string());

            let client = reqwest::Client::new();
            let err = post_graphql::<queries::WorkflowStatus, _>(
                &client,
                server.url(),
                queries::workflow_status::Variables {
                    workflow_id: "wf-42".to_string(),
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(err, RailwayError::Ratelimited));
        }

        #[tokio::test]
        async fn connection_refused_maps_to_fetch_error() {
            // Port 9 (discard) is never listening locally.
            let client = reqwest::Client::new();
            let err = post_graphql::<queries::WorkflowStatus, _>(
                &client,
                "http://127.0.0.1:9/graphql/v2",
                queries::workflow_status::Variables {
                    workflow_id: "wf-42".to_string(),
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(err, RailwayError::FetchError(_)));
        }
    }
}
