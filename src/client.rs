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
/// If the lock can't be acquired we return [`RailwayError::ConfigLockBusy`]
/// rather than ever refreshing unlocked.
pub async fn ensure_valid_token(configs: &mut Configs) -> Result<()> {
    // Env var tokens are not managed by us
    if Configs::get_railway_token().is_some() || Configs::get_railway_api_token().is_some() {
        return Ok(());
    }

    // Fast path: nothing to refresh.
    if !configs.has_oauth_token() || !configs.is_token_expired() {
        return Ok(());
    }

    // Serialize the refresh across concurrent CLI processes. Refreshing
    // without the lock is never safe: a parallel refresh would present an
    // already-rotated refresh token, which the server treats as reuse and
    // revokes the entire grant (a hard logout). If we can't take the lock
    // (another process is refreshing, or the lockfile can't be created) we
    // surface a retryable "busy" error — a retry is cheap and recoverable,
    // a revoked grant is not.
    let _lock_guard = ConfigLockGuard::acquire().ok_or(RailwayError::ConfigLockBusy)?;

    // Re-read credentials now that we hold the lock: another process may have
    // already refreshed while we were waiting, in which case we skip the
    // refresh entirely and use the freshly-rotated token it wrote.
    configs.reload()?;
    if !configs.has_oauth_token() || !configs.is_token_expired() {
        return Ok(());
    }

    refresh_tokens(configs).await
    // `_lock_guard` is dropped here (or at the early return above), releasing
    // the lock only after the freshly-rotated token has been persisted.
}

/// Perform the actual OAuth refresh + persist. The caller must hold the config
/// lock and have re-checked expiry after acquiring it.
async fn refresh_tokens(configs: &mut Configs) -> Result<()> {
    let refresh_token = configs.get_refresh_token().ok_or_else(|| {
        RailwayError::OAuthRefreshFailed("No refresh token available".to_string())
    })?;

    let host = configs.get_host();
    let token_resp = oauth::refresh_access_token(host, refresh_token).await?;

    configs.save_oauth_tokens(
        &token_resp.access_token,
        token_resp.refresh_token.as_deref(),
        token_resp.expires_in,
    )?;

    Ok(())
}

/// How long to wait for the config lock before giving up and returning a
/// retryable busy error. Kept short so a stale lock can never wedge the CLI
/// for long.
const CONFIG_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll interval while waiting for the config lock.
const CONFIG_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// RAII guard around an exclusive advisory lock on the config lockfile.
/// Releasing the lock (and dropping the file handle) happens on drop, covering
/// all error paths.
struct ConfigLockGuard {
    file: std::fs::File,
}

impl ConfigLockGuard {
    /// Try to acquire the exclusive config lock, retrying up to
    /// [`CONFIG_LOCK_TIMEOUT`]. Returns `None` on any failure — the lock is
    /// held by another process, or the lockfile could not be created — and the
    /// caller turns that into a retryable error rather than refreshing
    /// unlocked.
    fn acquire() -> Option<Self> {
        let lock_path = match Configs::config_lock_path() {
            Ok(path) => path,
            Err(e) => {
                eprintln!(
                    "{}: {e}",
                    "Warning: could not determine config lock path".yellow()
                );
                return None;
            }
        };

        Self::acquire_at(&lock_path, CONFIG_LOCK_TIMEOUT, CONFIG_LOCK_POLL_INTERVAL)
    }

    /// Core acquisition against an explicit path/timeout. Factored out from
    /// [`acquire`] so the contention behaviour can be exercised hermetically
    /// against a temp lockfile in tests. Returns `None` if the lockfile can't
    /// be created or the lock is still held when the timeout elapses.
    fn acquire_at(
        lock_path: &std::path::Path,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Option<Self> {
        use fs2::FileExt;

        if let Some(parent) = lock_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "{}: {e}",
                    "Warning: could not create config directory for lock".yellow()
                );
                return None;
            }
        }

        let file = match std::fs::File::create(lock_path) {
            Ok(file) => file,
            Err(e) => {
                eprintln!(
                    "{}: {e}",
                    "Warning: could not open config lock file".yellow()
                );
                return None;
            }
        };

        let deadline = std::time::Instant::now() + timeout;
        loop {
            if file.try_lock_exclusive().is_ok() {
                return Some(Self { file });
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(poll_interval);
        }
    }
}

impl Drop for ConfigLockGuard {
    fn drop(&mut self) {
        use fs2::FileExt;
        // Best-effort unlock; the lock is also released when the file handle is
        // dropped immediately after.
        let _ = FileExt::unlock(&self.file);
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

    #[test]
    fn config_lock_acquires_when_free() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".config.lock");

        let guard =
            ConfigLockGuard::acquire_at(&path, Duration::from_secs(1), Duration::from_millis(10));

        assert!(guard.is_some(), "should acquire a free lock");
    }

    #[test]
    fn config_lock_times_out_when_held_and_never_refreshes_unlocked() {
        use fs2::FileExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".config.lock");

        // Simulate another CLI process holding the exclusive lock. flock locks
        // are per open-file-description, so a second handle to the same path
        // contends even within this process.
        let held = std::fs::File::create(&path).unwrap();
        held.lock_exclusive().unwrap();

        let timeout = Duration::from_millis(300);
        let start = std::time::Instant::now();
        let guard = ConfigLockGuard::acquire_at(&path, timeout, Duration::from_millis(20));
        let elapsed = start.elapsed();

        // The guard must be None (so the caller returns ConfigLockBusy rather
        // than performing an unlocked refresh), and only after waiting out the
        // full timeout.
        assert!(
            guard.is_none(),
            "must not acquire a lock already held by another handle"
        );
        assert!(
            elapsed >= timeout,
            "should wait the full timeout before giving up (waited {elapsed:?})"
        );

        FileExt::unlock(&held).unwrap();
    }
}
