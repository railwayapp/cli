use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

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
            crate::util::reporter::warn(
                "INVALID_HTTP_TIMEOUT",
                format!(
                    "ignoring invalid {}={raw:?}; expected a positive number of seconds, using {}s",
                    consts::RAILWAY_HTTP_TIMEOUT_ENV,
                    consts::DEFAULT_HTTP_TIMEOUT_SECS
                ),
                None,
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
    let body = serde_json::to_value(Q::build_query(variables))
        .expect("Failed to serialize GraphQL query body");
    post_graphql_for_current_session(client, url.into_url()?, &body).await
}

/// Send a GraphQL request that must go out unauthenticated, for the queries
/// backboard answers for anyone (template lookup and search).
///
/// Deliberately separate from [`post_graphql`] rather than a property of the
/// client: [`post_graphql`] keeps the caller's bearer current by setting it per
/// request (see [`current_bearer`]), and a `reqwest::Client` cannot be asked
/// whether it was built with one. So the two paths are told apart here, at the
/// call site that knows — which also keeps a logged-out user's template search
/// on exactly the same code path as a logged-in user's.
pub async fn post_graphql_public<Q: GraphQLQuery, U: reqwest::IntoUrl>(
    client: &reqwest::Client,
    url: U,
    variables: Q::Variables,
) -> Result<Q::ResponseData, RailwayError> {
    let body = serde_json::to_value(Q::build_query(variables))
        .expect("Failed to serialize GraphQL query body");
    post_graphql_value(client, url.into_url()?, &body, None).await
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
    post_graphql_for_current_session(client, url.into_url()?, &body).await
}

/// Send a GraphQL request with the disambiguation wired to the current
/// process's stored session and environment. See [`post_graphql_value`].
async fn post_graphql_for_current_session<T: DeserializeOwned>(
    client: &reqwest::Client,
    url: reqwest::Url,
    body: &serde_json::Value,
) -> Result<T, RailwayError> {
    let mut configs = Configs::new().ok();
    let oauth_base_url = configs
        .as_ref()
        .map(|c| oauth::get_oauth_base_url(c.get_host()));
    // Refresh before sending rather than after being refused. The fast path is
    // a timestamp comparison on the config this function already read, so it
    // costs nothing while the token is good; when it is not, one refresh here
    // replaces a 401 round-trip plus a probe plus a retry. A failure is not
    // fatal — the request still goes out and the "Not Authorized" handling
    // below produces the same story it always did.
    if let Some(configs) = configs.as_mut() {
        let _ = ensure_valid_token(configs).await;
    }
    post_graphql_value(
        client,
        url,
        body,
        configs.as_mut().zip(oauth_base_url.as_deref()),
    )
    .await
}

/// The `authorization` header the stored session would present right now, or
/// `None` when this process authenticates some other way.
///
/// Callers build a client once and keep it: `railway ca`'s TUI holds a single
/// one for its entire run, cloned into every sweep and prefetch. The bearer
/// baked into that client's default headers is the one that existed at
/// startup, so an hour later — once the access token expires — every request
/// it sends carries a dead token, 401s, and pays for a probe and a retry that
/// leave the client just as stale for the next one. Setting the header per
/// request instead lets a long-lived client pick up refreshes: reqwest applies
/// a default header only when the request has not already set that key.
///
/// `RAILWAY_TOKEN` authenticates with `project-access-token` and no bearer at
/// all, so that case is left entirely alone.
fn current_bearer(configs: &Configs) -> Option<HeaderValue> {
    if Configs::get_railway_token().is_some() {
        return None;
    }
    let token = configs.get_railway_auth_token()?;
    HeaderValue::from_str(&format!("Bearer {token}")).ok()
}

/// What a token-endpoint probe says about the stored session after the API
/// answered "Not Authorized".
enum SessionProbe {
    /// A refresh just succeeded, so the grant is alive: the rejection was
    /// about the resource, not the session.
    Alive,
    /// The grant is dead (`invalid_grant`); the stored credentials have been
    /// cleared by the refresh policy.
    Dead,
    /// Nothing to probe with (token auth, no refresh token) or the probe
    /// itself failed transiently — nothing can be concluded.
    Inconclusive,
}

/// Ask the token endpoint whether the stored session is still alive, under
/// the config lock (a refresh is a read-modify-write of the credential file).
async fn probe_session_liveness(configs: &mut Configs, oauth_base_url: &str) -> SessionProbe {
    if Configs::is_using_token_auth() {
        // Env-var tokens have no refresh token to probe with.
        return SessionProbe::Inconclusive;
    }
    match refresh_locked_at(configs, oauth_base_url, true).await {
        RefreshOutcome::Refreshed | RefreshOutcome::AlreadyFresh => SessionProbe::Alive,
        RefreshOutcome::SessionExpired(_) => SessionProbe::Dead,
        RefreshOutcome::Transient(_) | RefreshOutcome::NoRefreshToken => SessionProbe::Inconclusive,
    }
}

/// Send a GraphQL request, disambiguating "Not Authorized".
///
/// The server renders two very different failures identically: a dead
/// session (re-login fixes it) and an authorization denial on the resource —
/// e.g. an OAuth grant limited to specific workspaces (re-login can NEVER
/// fix it: consent reuses the same partially-scoped grant). Telling the
/// second group to "run `railway login` again" wedged them permanently; see
/// mono's docs/investigations/cli-logout-issue/.
///
/// On "Not Authorized" with an OAuth session, probe the token endpoint — a
/// refresh succeeds only for a live grant — then retry the request once with
/// the refreshed credentials:
/// - retry succeeds → the access token had died server-side while looking
///   fresh locally; the user never sees an error (previously this wedged
///   every command until local expiry, up to an hour);
/// - retry still unauthorized → the session is alive and the denial is about
///   the resource: report [`RailwayError::OAuthInsufficientGrant`] instead
///   of a re-login prompt;
/// - probe says the grant is dead → the re-login prompt stands (and the
///   refresh policy has already cleared the dead credentials).
pub(crate) async fn post_graphql_value<T: DeserializeOwned>(
    client: &reqwest::Client,
    url: reqwest::Url,
    body: &serde_json::Value,
    session: Option<(&mut Configs, &str)>,
) -> Result<T, RailwayError> {
    let mut request = client.post(url.clone()).json(body);
    // Override whatever bearer the client was built with; see [`current_bearer`].
    if let Some((configs, _)) = &session {
        if let Some(bearer) = current_bearer(configs) {
            request = request.header(reqwest::header::AUTHORIZATION, bearer);
        }
    }
    let response = request.send().await?;
    let err = match parse_graphql_response(response).await {
        Err(e @ (RailwayError::Unauthorized | RailwayError::UnauthorizedLogin)) => e,
        other => return other,
    };

    let Some((configs, oauth_base_url)) = session else {
        return Err(err);
    };
    match probe_session_liveness(configs, oauth_base_url).await {
        SessionProbe::Dead | SessionProbe::Inconclusive => Err(err),
        SessionProbe::Alive => {
            // Carry the refreshed bearer; the original client's is baked in.
            let Ok(fresh_client) = GQLClient::new_authorized(configs) else {
                return Err(err);
            };
            let response = fresh_client.post(url).json(body).send().await?;
            match parse_graphql_response(response).await {
                Err(RailwayError::Unauthorized | RailwayError::UnauthorizedLogin) => {
                    Err(RailwayError::OAuthInsufficientGrant)
                }
                other => other,
            }
        }
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
    let base_url = oauth::get_oauth_base_url(configs.get_host());
    ensure_valid_token_at(configs, &base_url).await
}

/// [`ensure_valid_token`] against an explicit token endpoint, for callers that
/// have already resolved one (and for tests, which point at a scripted one).
pub(crate) async fn ensure_valid_token_at(configs: &mut Configs, base_url: &str) -> Result<()> {
    // Env var tokens are not managed by us
    if Configs::is_using_token_auth() {
        return Ok(());
    }

    // Fast path: nothing to refresh.
    if !configs.has_oauth_token() || !configs.is_token_expired() {
        return Ok(());
    }

    match refresh_locked_at(configs, base_url, false).await {
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

/// Serialises refreshes within this process, ahead of the cross-process file
/// lock.
///
/// The file lock alone does not do this. `fs2` locks are per open file
/// description, so two tasks in one process contend exactly as two processes
/// would — and a long-lived TUI is far more concurrent than a one-shot command:
/// `railway ca` runs an account-wide refresh on a timer, watch ticks at 1.5s,
/// and sweeps that fan out per environment. Let those all queue on the file
/// lock and each waiter sits through every earlier waiter's token round-trip,
/// blows the lock's timeout, and prints a warning — for a refresh the first one
/// already performed. Taking this gate first means at most one task in the
/// process ever waits on the file lock, and the rest arrive after the config
/// has been rewritten and find nothing left to do.
static REFRESH_GATE: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);

/// When this process last minted tokens for a given config file. Keyed by path
/// rather than kept as a single global so tests, which each drive their own
/// temporary config, cannot see one another's refreshes.
static LAST_REFRESH: LazyLock<std::sync::Mutex<HashMap<PathBuf, Instant>>> =
    LazyLock::new(Default::default);

/// How recently a refresh has to have happened for a *forced* one to stand
/// down. Long enough to absorb a burst of simultaneous 401s, short enough that
/// a grant revoked moments after a refresh is still discovered promptly.
const RECENT_REFRESH_WINDOW: Duration = Duration::from_secs(10);

/// How long to wait for [`REFRESH_GATE`] before proceeding without it.
///
/// A refresh normally takes well under a second, but a hung token endpoint can
/// hold the gate for the full retry budget — three attempts against a 30s HTTP
/// timeout, plus backoff, so about 90s. Waiting that out would make one stuck
/// refresh stall every other request in the process, which is what the old
/// unbounded file-lock wait did. Waiters give up here instead and use whatever
/// credentials are on disk; they do NOT refresh on their own, since two
/// concurrent rotations are exactly the hard-logout this gate exists to avoid.
const REFRESH_GATE_TIMEOUT: Duration = Duration::from_secs(15);

fn refreshed_recently(path: &std::path::Path) -> bool {
    LAST_REFRESH.lock().is_ok_and(|seen| {
        seen.get(path)
            .is_some_and(|at| at.elapsed() < RECENT_REFRESH_WINDOW)
    })
}

fn mark_refreshed(path: &std::path::Path) {
    if let Ok(mut seen) = LAST_REFRESH.lock() {
        seen.insert(path.to_path_buf(), Instant::now());
    }
}

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
    let base_url = oauth::get_oauth_base_url(configs.get_host());
    refresh_locked_at(configs, &base_url, force).await
}

/// [`refresh_locked`] against an explicit token endpoint, so the probe path can
/// pass the base URL it already resolved (and tests can point at a scripted
/// one) instead of re-deriving it.
async fn refresh_locked_at(configs: &mut Configs, base_url: &str, force: bool) -> RefreshOutcome {
    let Ok(_gate) = tokio::time::timeout(REFRESH_GATE_TIMEOUT, REFRESH_GATE.lock()).await else {
        // Someone else's refresh is wedged. Adopt whatever it has written so
        // far and report a transient failure: the caller proceeds with the
        // stored credentials, and a request that still comes back unauthorized
        // says so plainly instead of being explained away as a grant problem.
        let _ = configs.reload();
        return RefreshOutcome::Transient(RailwayError::OAuthRefreshFailed(
            "timed out waiting for an in-flight token refresh".to_string(),
        ));
    };
    let _lock = configs.acquire_lock().await;

    if configs.reload().is_err() {
        return RefreshOutcome::NoRefreshToken;
    }
    if !force && (!configs.has_oauth_token() || !configs.is_token_expired()) {
        return RefreshOutcome::AlreadyFresh;
    }
    // A forced refresh answers an actual 401, and concurrent requests all get
    // refused at the same moment. The first through the gate rotates the
    // token; the rest have just reloaded its result and would otherwise spend a
    // rotation each — every one of them a chance for backboard's reuse
    // detection to see a consumed token and revoke the grant.
    if force && refreshed_recently(configs.config_path()) {
        return RefreshOutcome::AlreadyFresh;
    }

    let outcome = refresh_with_policy(configs, base_url, REFRESH_BACKOFF).await;
    if matches!(outcome, RefreshOutcome::Refreshed) {
        mark_refreshed(configs.config_path());
    }
    outcome
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
                crate::util::reporter::warn(
                    "CREDENTIAL_CLEAR_FAILED",
                    format!("could not clear the expired login from disk: {clear_err}"),
                    None,
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

    post_graphql_for_current_session(client, url.into_url()?, &body_json).await
}

async fn parse_graphql_response<T>(response: reqwest::Response) -> Result<T, RailwayError>
where
    T: DeserializeOwned,
{
    if response.status() == 429 {
        // Backboard sets `Retry-After` on every 429 (see getRateLimitHeaders).
        let retry_after_secs = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        return Err(RailwayError::Ratelimited { retry_after_secs });
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
        let response = post_graphql_public::<queries::TemplateDetail, _>(
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
            assert!(matches!(err, RailwayError::Ratelimited { .. }));
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
