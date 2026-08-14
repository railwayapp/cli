//! Simulation harness for the CLI's OAuth refresh behaviour.
//!
//! These tests exist to answer one question with evidence rather than code
//! reading: when a user's refresh token is dead server-side, or when the token
//! endpoint is briefly 5xx, what actually happens to the credentials on disk
//! and to the user-facing outcome?
//!
//! Every experiment runs the REAL HTTP layer ([`oauth::attempt_refresh`]) and
//! the REAL config read/modify/write cycle ([`Configs`]) against a local
//! scripted token endpoint. `legacy_policy` reproduces production's current
//! behaviour; `refresh_with_policy` is the proposed replacement. Because both
//! share the same HTTP and config code, any difference is attributable to the
//! policy alone.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::client::{RefreshOutcome, refresh_with_policy};
use crate::config::Configs;
use crate::oauth;

/// A scripted local endpoint. Serves `responses` in order, repeating the last
/// one once exhausted, and records every request it receives so tests can assert
/// both how many arrived and what headers they carried.
struct MockEndpoint {
    base_url: String,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
}

impl MockEndpoint {
    fn spawn(responses: Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let requests_for_thread = Arc::clone(&requests);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };

                // Drain headers and any body so the client never sees a broken
                // pipe on the next request.
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                let mut content_length = 0usize;
                loop {
                    let Ok(read) = stream.read(&mut tmp) else {
                        break;
                    };
                    if read == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..read]);
                    if let Some(pos) = find_headers_end(&buf) {
                        let headers = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
                        for line in headers.lines() {
                            if let Some(v) = line.strip_prefix("content-length:") {
                                content_length = v.trim().parse().unwrap_or(0);
                            }
                        }
                        if buf.len() >= pos + 4 + content_length {
                            break;
                        }
                    }
                }

                let mut seen = requests_for_thread.lock().unwrap();
                let idx = seen.len().min(responses.len().saturating_sub(1));
                seen.push(String::from_utf8_lossy(&buf).to_string());
                drop(seen);

                let (status, body) = &responses[idx];
                let reason = match status {
                    200 => "OK",
                    400 => "Bad Request",
                    500 => "Internal Server Error",
                    503 => "Service Unavailable",
                    _ => "Unknown",
                };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });

        Self {
            base_url: format!("http://127.0.0.1:{port}/oauth"),
            requests,
        }
    }

    fn hits(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The `authorization` header of each request, in arrival order.
    fn auth_headers(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|req| {
                req.lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                    .map(|l| l["authorization:".len()..].trim().to_string())
                    .unwrap_or_else(|| "(none)".to_string())
            })
            .collect()
    }
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn dead_grant() -> (u16, String) {
    (
        400,
        r#"{"error":"invalid_grant","error_description":"grant request is invalid"}"#.to_string(),
    )
}
fn server_error() -> (u16, String) {
    (500, r#"{"error":"server_error"}"#.to_string())
}
fn fresh_tokens() -> (u16, String) {
    (
        200,
        r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#
            .to_string(),
    )
}
fn ok_empty() -> (u16, String) {
    (200, r#"{"data":{}}"#.to_string())
}

/// A temp config file seeded with an expired access token and a refresh token,
/// i.e. exactly the state a CLI process is in when it starts up an hour after
/// the last command.
struct Fixture {
    path: PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut configs = Configs::for_test(path.clone());
        // expires_in of 1s, then we treat it as already past: save_oauth_tokens
        // requires a positive expires_in, so age it directly afterwards.
        configs
            .save_oauth_tokens("stale-access", Some("the-refresh-token"), 1)
            .unwrap();
        configs.root_config.user.token_expires_at = Some(0); // long expired
        configs.write().unwrap();
        Self { path, _dir: dir }
    }

    /// A freshly-loaded Configs, standing in for a brand-new CLI process.
    fn load(&self) -> Configs {
        let mut configs = Configs::for_test(self.path.clone());
        configs.reload().unwrap();
        configs
    }
}

/// Reproduction of production's CURRENT refresh policy (cli 5.30.1):
/// one attempt, no 400-vs-5xx distinction, and credentials are never cleared.
/// Mirrors what `client::refresh_tokens` did at cli 5.30.1.
async fn legacy_policy(configs: &mut Configs, base_url: &str) -> Result<(), String> {
    let refresh_token = match configs.get_refresh_token() {
        Some(t) => t.to_owned(),
        None => return Err("No refresh token available".to_string()),
    };
    let client = reqwest::Client::new();
    match oauth::attempt_refresh(&client, base_url, &refresh_token).await {
        Ok(resp) => {
            configs
                .save_oauth_tokens(
                    &resp.access_token,
                    resp.refresh_token.as_deref(),
                    resp.expires_in,
                )
                .unwrap();
            Ok(())
        }
        // Production collapses every failure into one error and leaves the
        // stored credentials exactly as they were.
        Err(f) => Err(match f {
            oauth::RefreshFailure::Terminal(e) | oauth::RefreshFailure::Transient(e) => {
                e.to_string()
            }
        }),
    }
}

const INVOCATIONS: usize = 20;

#[tokio::test]
async fn legacy_dead_grant_retries_forever_and_keeps_dead_credentials() {
    let server = MockEndpoint::spawn(vec![dead_grant()]);
    let fixture = Fixture::new();

    for _ in 0..INVOCATIONS {
        let mut configs = fixture.load();
        let result = legacy_policy(&mut configs, &server.base_url).await;
        assert!(result.is_err(), "dead grant should fail");
    }

    // Every single invocation hit the token endpoint with the same dead token.
    assert_eq!(
        server.hits(),
        INVOCATIONS,
        "legacy policy re-presents a known-dead refresh token on every invocation"
    );

    // And the dead credentials are still sitting on disk, so this never ends.
    let configs = fixture.load();
    assert!(
        configs.has_oauth_token(),
        "legacy policy leaves the dead access token on disk"
    );
    assert_eq!(
        configs.get_refresh_token(),
        Some("the-refresh-token"),
        "legacy policy leaves the dead refresh token on disk"
    );
}

#[tokio::test]
async fn fixed_dead_grant_clears_credentials_and_stops_after_one_attempt() {
    let server = MockEndpoint::spawn(vec![dead_grant()]);
    let fixture = Fixture::new();

    let mut outcomes = Vec::new();
    for _ in 0..INVOCATIONS {
        let mut configs = fixture.load();
        outcomes.push(refresh_with_policy(&mut configs, &server.base_url, Duration::ZERO).await);
    }

    // The first invocation learns the grant is dead; the rest have nothing to
    // present, so the token endpoint is never touched again.
    assert_eq!(
        server.hits(),
        1,
        "fixed policy must present a dead refresh token exactly once, got {} hits",
        server.hits()
    );
    assert!(matches!(outcomes[0], RefreshOutcome::SessionExpired(_)));
    assert!(
        outcomes[1..]
            .iter()
            .all(|o| matches!(o, RefreshOutcome::NoRefreshToken)),
        "after clearing, later invocations have no token to retry"
    );

    let configs = fixture.load();
    assert!(!configs.has_oauth_token());
    assert_eq!(configs.get_refresh_token(), None);
}

#[tokio::test]
async fn legacy_transient_5xx_is_indistinguishable_from_a_dead_grant() {
    let dead = MockEndpoint::spawn(vec![dead_grant()]);
    let flaky = MockEndpoint::spawn(vec![server_error()]);
    let f1 = Fixture::new();
    let f2 = Fixture::new();

    let dead_err = legacy_policy(&mut f1.load(), &dead.base_url)
        .await
        .unwrap_err();
    let flaky_err = legacy_policy(&mut f2.load(), &flaky.base_url)
        .await
        .unwrap_err();

    // Both surface as the same RailwayError variant, whose message tells the
    // user to run `railway login` — even though in the 5xx case the stored
    // refresh token is perfectly good.
    let dead_rendered = crate::errors::RailwayError::OAuthRefreshFailed(dead_err).to_string();
    let flaky_rendered = crate::errors::RailwayError::OAuthRefreshFailed(flaky_err).to_string();
    assert!(dead_rendered.contains("Couldn't refresh") || dead_rendered.contains("railway login"));
    assert!(
        flaky_rendered.contains("Couldn't refresh") || flaky_rendered.contains("railway login")
    );

    // And critically: exactly one attempt. No retry for a transient failure.
    assert_eq!(
        flaky.hits(),
        1,
        "legacy policy does not retry a transient 5xx"
    );
}

#[tokio::test]
async fn fixed_transient_5xx_retries_and_preserves_credentials() {
    let server = MockEndpoint::spawn(vec![server_error()]);
    let fixture = Fixture::new();

    let mut configs = fixture.load();
    let outcome = refresh_with_policy(&mut configs, &server.base_url, Duration::ZERO).await;

    assert!(
        matches!(outcome, RefreshOutcome::Transient(_)),
        "a 5xx must be transient, got {outcome:?}"
    );
    assert_eq!(
        server.hits(),
        oauth::REFRESH_MAX_ATTEMPTS as usize,
        "fixed policy retries transient failures"
    );

    // The whole point: a backboard blip must not cost the user their session.
    let reloaded = fixture.load();
    assert_eq!(reloaded.get_refresh_token(), Some("the-refresh-token"));
    assert!(reloaded.has_oauth_token());
}

#[tokio::test]
async fn fixed_policy_recovers_when_the_token_endpoint_comes_back() {
    // Mirrors the 2026-07-30/31 backboard DB incidents: the token endpoint
    // 5xx'd for a window, then recovered.
    let server = MockEndpoint::spawn(vec![server_error(), server_error(), fresh_tokens()]);
    let fixture = Fixture::new();

    let mut configs = fixture.load();
    let outcome = refresh_with_policy(&mut configs, &server.base_url, Duration::ZERO).await;

    assert!(
        matches!(outcome, RefreshOutcome::Refreshed),
        "the user should never notice a brief outage, got {outcome:?}"
    );
    let reloaded = fixture.load();
    assert_eq!(reloaded.get_refresh_token(), Some("new-refresh"));
    assert!(
        !reloaded.is_token_expired(),
        "fresh token must not be expired"
    );
}

/// The property that stops a backboard misconfiguration from becoming a mass
/// logout: only `invalid_grant` may clear credentials. `invalid_client` and
/// friends describe the client registration or the request, not the user's
/// grant, and the CLI ships one hardcoded `client_id` for everybody.
#[tokio::test]
async fn only_invalid_grant_clears_credentials() {
    for code in [
        "invalid_client",
        "unauthorized_client",
        "invalid_scope",
        "invalid_request",
        "unsupported_grant_type",
        "server_error",
        "temporarily_unavailable",
        "slow_down",
        "unknown",
    ] {
        let body = format!(r#"{{"error":"{code}","error_description":"boom"}}"#);
        let server = MockEndpoint::spawn(vec![(400, body)]);
        let fixture = Fixture::new();
        let mut configs = fixture.load();

        let outcome = refresh_with_policy(&mut configs, &server.base_url, Duration::ZERO).await;

        assert!(
            matches!(outcome, RefreshOutcome::Transient(_)),
            "{code} must not be treated as a permanently dead credential, got {outcome:?}"
        );
        assert_eq!(
            fixture.load().get_refresh_token(),
            Some("the-refresh-token"),
            "{code} must leave the refresh token on disk"
        );
    }
}

#[tokio::test]
async fn fixed_policy_treats_unparseable_4xx_as_transient() {
    // A WAF block or proxy error page must not be mistaken for a dead grant —
    // discarding a working refresh token is the expensive mistake.
    let server = MockEndpoint::spawn(vec![(400, "<html>blocked by proxy</html>".to_string())]);
    let fixture = Fixture::new();

    let mut configs = fixture.load();
    let outcome = refresh_with_policy(&mut configs, &server.base_url, Duration::ZERO).await;

    assert!(
        matches!(outcome, RefreshOutcome::Transient(_)),
        "unparseable 4xx must not clear credentials, got {outcome:?}"
    );
    assert_eq!(
        fixture.load().get_refresh_token(),
        Some("the-refresh-token")
    );
}

/// Durability of the credential clear against a concurrent stale writer.
///
/// A process can hold a `Configs` for hours (`railway mcp` holds one for a whole
/// editor session) and then write it for an unrelated reason such as linking a
/// project. Serialising its whole snapshot used to put the credentials it loaded
/// at startup back on disk, undoing another process's refresh or clear and
/// restarting the retry loop. `write` now takes the credential fields from disk,
/// so only the auth path can move them.
#[tokio::test]
async fn stale_writer_cannot_resurrect_cleared_credentials() {
    let server = MockEndpoint::spawn(vec![dead_grant()]);
    let fixture = Fixture::new();

    // Process B starts and loads the config while the credentials still exist.
    let mut stale_process = fixture.load();
    assert!(stale_process.has_oauth_token());

    // Process A discovers the grant is dead and clears.
    let mut clearing_process = fixture.load();
    let outcome =
        refresh_with_policy(&mut clearing_process, &server.base_url, Duration::ZERO).await;
    assert!(matches!(outcome, RefreshOutcome::SessionExpired(_)));
    assert_eq!(fixture.load().get_refresh_token(), None, "clear persisted");

    // Process B now writes for an unrelated reason, using its stale snapshot.
    stale_process.root_config.user.id = Some("some-user".to_string());
    stale_process.write().unwrap();

    let after = fixture.load();
    assert_eq!(
        after.get_refresh_token(),
        None,
        "the stale writer must not resurrect the dead refresh token"
    );
    assert!(!after.has_oauth_token());
    // ...while its own, non-credential change still lands.
    assert_eq!(after.root_config.user.id.as_deref(), Some("some-user"));
}

/// The mirror case: a stale writer must not undo a successful refresh either.
#[tokio::test]
async fn stale_writer_cannot_undo_a_refresh() {
    let server = MockEndpoint::spawn(vec![fresh_tokens()]);
    let fixture = Fixture::new();

    let mut stale_process = fixture.load();
    let mut refreshing = fixture.load();
    assert!(matches!(
        refresh_with_policy(&mut refreshing, &server.base_url, Duration::ZERO).await,
        RefreshOutcome::Refreshed
    ));

    stale_process.root_config.user.id = Some("some-user".to_string());
    stale_process.write().unwrap();

    let after = fixture.load();
    assert_eq!(
        after.get_refresh_token(),
        Some("new-refresh"),
        "the refreshed credentials must survive an unrelated concurrent write"
    );
    assert_eq!(
        after.get_railway_auth_token().as_deref(),
        Some("new-access")
    );
}

/// `railway logout` must still be able to erase credentials. Ordinary writes
/// adopt whatever is on disk, so logout has to go through the credential-owning
/// write — mirrors `commands::logout`.
#[tokio::test]
async fn logout_clears_credentials() {
    let fixture = Fixture::new();
    let mut configs = fixture.load();
    assert!(configs.has_oauth_token());

    configs.reset().unwrap();
    configs.write_credentials().unwrap();

    let after = fixture.load();
    assert!(
        !after.has_oauth_token(),
        "logout must erase the access token"
    );
    assert_eq!(after.get_refresh_token(), None);
}

/// The other half of that invariant: a plain `write` must NOT be able to erase
/// credentials, which is what stops a stale in-memory snapshot from clobbering
/// them.
#[tokio::test]
async fn plain_write_cannot_erase_credentials() {
    let fixture = Fixture::new();
    let mut configs = fixture.load();

    configs.reset().unwrap();
    configs.write().unwrap();

    let after = fixture.load();
    assert_eq!(
        after.get_refresh_token(),
        Some("the-refresh-token"),
        "a non-credential write must leave the stored credentials alone"
    );
}

/// The local `railway mcp` defect, isolated: `GQLClient::new_authorized` bakes
/// the bearer into the client's default headers, so a client built at process
/// start keeps sending the startup token no matter what happens on disk.
///
/// Still true of the client itself, and still the reason `post_graphql` sets
/// the bearer per request rather than trusting the one it was built with — see
/// `a_long_lived_client_sends_the_current_bearer_not_its_startup_one`, which
/// covers the same staleness through the real send path.
#[tokio::test]
async fn baked_in_bearer_ignores_new_credentials_on_disk() {
    let backboard = MockEndpoint::spawn(vec![ok_empty()]);
    let fixture = Fixture::new();

    // Startup: build the client once, exactly as serve_stdio does.
    let startup_configs = fixture.load();
    let frozen = crate::client::GQLClient::new_authorized(&startup_configs).unwrap();

    // A `railway login` in another terminal replaces the credentials on disk.
    let mut relogin = fixture.load();
    relogin
        .save_oauth_tokens("brand-new-access", Some("brand-new-refresh"), 3600)
        .unwrap();
    assert_eq!(
        fixture.load().get_railway_auth_token().as_deref(),
        Some("brand-new-access")
    );

    // The frozen client still sends the startup token.
    let _ = frozen.post(&backboard.base_url).json(&()).send().await;
    assert_eq!(
        backboard.auth_headers(),
        vec!["Bearer stale-access".to_string()],
        "EXPECTED DEFECT: the client built at startup keeps using the startup token"
    );

    // Rebuilding from the current on-disk config is what picks up the new token.
    let rebuilt = crate::client::GQLClient::new_authorized(&fixture.load()).unwrap();
    let _ = rebuilt.post(&backboard.base_url).json(&()).send().await;
    assert_eq!(
        backboard.auth_headers()[1],
        "Bearer brand-new-access",
        "a per-request client adopts the new credentials"
    );
}

/// The mid-session expiry path: an expired access token must trigger a real
/// refresh and the resulting client must carry the NEW bearer.
#[tokio::test]
async fn expired_token_refreshes_and_new_bearer_reaches_the_wire() {
    let token_endpoint = MockEndpoint::spawn(vec![fresh_tokens()]);
    let backboard = MockEndpoint::spawn(vec![ok_empty()]);
    let fixture = Fixture::new();

    let mut configs = fixture.load();
    assert!(configs.is_token_expired(), "fixture starts expired");

    let outcome = refresh_with_policy(&mut configs, &token_endpoint.base_url, Duration::ZERO).await;
    assert!(matches!(outcome, RefreshOutcome::Refreshed));

    let client = crate::client::GQLClient::new_authorized(&fixture.load()).unwrap();
    let _ = client.post(&backboard.base_url).json(&()).send().await;

    assert_eq!(
        backboard.auth_headers(),
        vec!["Bearer new-access".to_string()],
        "the refreshed token must be the one used for the request"
    );
}

/// Load guard for the per-tool-call sync: the expiry predicate that gates the
/// network call must report a freshly-saved token as valid, so the steady-state
/// path does no refresh I/O at all.
#[tokio::test]
async fn a_freshly_saved_token_is_not_considered_expired() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut configs = Configs::for_test(path.clone());
    configs
        .save_oauth_tokens("good-access", Some("good-refresh"), 3600)
        .unwrap();

    let mut reloaded = Configs::for_test(path);
    reloaded.reload().unwrap();
    assert!(
        !reloaded.is_token_expired(),
        "a token minted seconds ago must not be treated as expired, or every \
         tool call would refresh"
    );

    // ...and the 60s safety buffer still classifies a nearly-dead token as expired.
    reloaded.root_config.user.token_expires_at = Some(chrono::Utc::now().timestamp() + 30);
    assert!(reloaded.is_token_expired());
}

/// A dead grant inside a long-lived MCP session must not become a retry storm:
/// credentials are cleared once, and later tool calls have nothing to present.
#[tokio::test]
async fn dead_grant_in_a_long_session_refreshes_once_not_per_tool_call() {
    let token_endpoint = MockEndpoint::spawn(vec![dead_grant()]);
    let fixture = Fixture::new();

    for _ in 0..INVOCATIONS {
        let mut configs = fixture.load();
        if configs.get_refresh_token().is_some() {
            refresh_with_policy(&mut configs, &token_endpoint.base_url, Duration::ZERO).await;
        }
    }

    assert_eq!(
        token_endpoint.hits(),
        1,
        "a dead grant must be discovered once per session, not once per tool call"
    );
}

// ---------------------------------------------------------------------------
// "Not Authorized" disambiguation (client::post_graphql_value)
//
// The server renders a dead session and a resource-authorization denial
// identically. These experiments drive the REAL GraphQL send + probe + retry
// pipeline against a scripted backboard and a scripted token endpoint, and
// assert which story the user is told for each underlying truth.
// ---------------------------------------------------------------------------

/// A config fixture holding a LIVE session: valid access token, refresh token.
struct LiveFixture {
    path: PathBuf,
    _dir: tempfile::TempDir,
}

impl LiveFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut configs = Configs::for_test(path.clone());
        configs
            .save_oauth_tokens("live-access", Some("live-refresh"), 3600)
            .unwrap();
        Self { path, _dir: dir }
    }

    fn load(&self) -> Configs {
        let mut configs = Configs::for_test(self.path.clone());
        configs.reload().unwrap();
        configs
    }
}

fn user_meta_body() -> serde_json::Value {
    serde_json::json!({
        "operationName": "UserMeta",
        "query": "query UserMeta { me { id } }",
        "variables": {},
    })
}

async fn send_user_meta(
    configs: &mut Configs,
    backboard: &crate::testkit::MockBackboard,
    token_url: &str,
) -> Result<serde_json::Value, crate::errors::RailwayError> {
    let client = crate::client::GQLClient::new_authorized(configs).unwrap();
    crate::client::post_graphql_value(
        &client,
        reqwest::Url::parse(&backboard.url()).unwrap(),
        &user_meta_body(),
        Some((configs, token_url)),
    )
    .await
}

/// The Erik shape: the session is alive (refresh succeeds) but the server
/// keeps refusing the resource. The user must hear "insufficient access",
/// not "log in again" — re-login can never fix a partially-scoped grant.
#[tokio::test]
async fn live_session_with_persistent_denial_reports_insufficient_grant() {
    let backboard = crate::testkit::MockBackboard::spawn();
    backboard.stub_graphql_error("UserMeta", "Not Authorized");
    let token_endpoint = MockEndpoint::spawn(vec![fresh_tokens()]);
    let fixture = LiveFixture::new();
    let mut configs = fixture.load();

    let result = send_user_meta(&mut configs, &backboard, &token_endpoint.base_url).await;

    assert!(
        matches!(
            result,
            Err(crate::errors::RailwayError::OAuthInsufficientGrant)
        ),
        "expected OAuthInsufficientGrant, got {result:?}"
    );
    // Exactly one probe, exactly one retry.
    assert_eq!(token_endpoint.hits(), 1, "one liveness probe");
    assert_eq!(backboard.hits(), 2, "original request + one retry");
    // The live credentials survive; nothing tells the user to relogin.
    let after = fixture.load();
    assert!(after.has_oauth_token());
    assert!(after.get_refresh_token().is_some());
}

/// A genuinely dead grant: the probe comes back invalid_grant, so the
/// re-login prompt stands and the dead credentials are cleared — the
/// pre-existing behavior, now backed by evidence instead of assumption.
#[tokio::test]
async fn dead_session_keeps_the_relogin_prompt_and_clears_credentials() {
    let backboard = crate::testkit::MockBackboard::spawn();
    backboard.stub_graphql_error("UserMeta", "Not Authorized");
    let token_endpoint = MockEndpoint::spawn(vec![dead_grant()]);
    let fixture = LiveFixture::new();
    let mut configs = fixture.load();

    let result = send_user_meta(&mut configs, &backboard, &token_endpoint.base_url).await;

    assert!(
        matches!(
            result,
            Err(crate::errors::RailwayError::Unauthorized
                | crate::errors::RailwayError::UnauthorizedLogin)
        ),
        "a dead session keeps the relogin story, got {result:?}"
    );
    // No retry: a dead session cannot be healed by resending.
    assert_eq!(backboard.hits(), 1);
    let after = fixture.load();
    assert!(!after.has_oauth_token(), "dead credentials are cleared");
    assert_eq!(after.get_refresh_token(), None);
}

/// The access token died server-side while still looking fresh locally
/// (e.g. revoked tokens with a surviving grant). The probe mints a fresh
/// bearer and the retry succeeds: the user sees nothing at all. Previously
/// every command failed until the local expiry timestamp passed.
#[tokio::test]
async fn server_side_expired_access_token_heals_transparently() {
    let backboard = crate::testkit::MockBackboard::spawn();
    backboard.stub_graphql_error("UserMeta", "Not Authorized");
    backboard.stub("UserMeta", serde_json::json!({ "me": { "id": "user-1" } }));
    let token_endpoint = MockEndpoint::spawn(vec![fresh_tokens()]);
    let fixture = LiveFixture::new();
    let mut configs = fixture.load();

    let result = send_user_meta(&mut configs, &backboard, &token_endpoint.base_url).await;

    assert!(result.is_ok(), "the retry should succeed, got {result:?}");
    assert_eq!(token_endpoint.hits(), 1);
    assert_eq!(backboard.hits(), 2);
    // The refreshed credentials were persisted for the next invocation.
    let after = fixture.load();
    assert_eq!(after.get_refresh_token(), Some("new-refresh"));
}

/// `railway ca` builds one authorized client and keeps it for the whole
/// session — hours. The bearer baked into that client's default headers is the
/// one that existed at startup, so once anything rotates the access token,
/// every request it sends carries a dead one: a 401, a probe, a token
/// rotation and a retry each time, with the client no fresher afterwards than
/// before. Setting the bearer per request from the config on disk is what
/// stops a long-lived client from going permanently stale.
#[tokio::test]
async fn a_long_lived_client_sends_the_current_bearer_not_its_startup_one() {
    let backboard = crate::testkit::MockBackboard::spawn();
    backboard.stub("UserMeta", serde_json::json!({ "me": { "id": "user-1" } }));
    let token_endpoint = MockEndpoint::spawn(vec![fresh_tokens()]);
    let fixture = LiveFixture::new();

    // The client the TUI built at startup, from the credentials of that moment.
    let at_startup = fixture.load();
    let client = crate::client::GQLClient::new_authorized(&at_startup).unwrap();

    // Time passes and the access token is rotated — by this process, by another
    // terminal, by the dashboard. The client knows nothing about it.
    fixture
        .load()
        .save_oauth_tokens("rotated-access", Some("rotated-refresh"), 3600)
        .unwrap();

    // Every request re-reads the config (see `post_graphql_for_current_session`),
    // so this is the `Configs` the real send would be holding.
    let mut per_request = fixture.load();
    let result = crate::client::post_graphql_value::<serde_json::Value>(
        &client,
        reqwest::Url::parse(&backboard.url()).unwrap(),
        &user_meta_body(),
        Some((&mut per_request, &token_endpoint.base_url)),
    )
    .await;

    assert!(result.is_ok(), "got {result:?}");
    assert_eq!(
        backboard.auth_headers(),
        vec![Some("Bearer rotated-access".to_string())],
        "the stale client must not send the token it was built with"
    );
    assert_eq!(
        token_endpoint.hits(),
        0,
        "the stored token is live, so nothing should have been refreshed"
    );
}

/// A burst of requests refused at the same moment — the shape a TUI produces,
/// where a timer refresh, a watch tick and a sweep are all in flight — must
/// cost one token rotation between them, not one each. Every extra rotation is
/// another chance for backboard's reuse detection to see a consumed refresh
/// token and revoke the whole grant.
#[tokio::test]
async fn a_second_refusal_reuses_the_refresh_the_first_one_just_performed() {
    let backboard = crate::testkit::MockBackboard::spawn();
    backboard.stub_graphql_error("UserMeta", "Not Authorized");
    let token_endpoint = MockEndpoint::spawn(vec![fresh_tokens()]);
    let fixture = LiveFixture::new();
    let mut configs = fixture.load();

    let first = send_user_meta(&mut configs, &backboard, &token_endpoint.base_url).await;
    let second = send_user_meta(&mut configs, &backboard, &token_endpoint.base_url).await;

    // The story each caller is told is unchanged: the grant is alive, so the
    // refusal is about the resource.
    for result in [&first, &second] {
        assert!(
            matches!(
                result,
                Err(crate::errors::RailwayError::OAuthInsufficientGrant)
            ),
            "got {result:?}"
        );
    }
    assert_eq!(
        token_endpoint.hits(),
        1,
        "the second refusal should adopt the first refresh, not rotate again"
    );
    // Both requests still went out and were still retried once each.
    assert_eq!(backboard.hits(), 4);
}

/// The `railway ca` burst, at full concurrency: an hour in, the access token
/// is dead and the timer refresh, the watch tick and a sweep are all refused
/// at the same instant.
///
/// Each refusal forces a refresh, and backboard rotates and reuse-detects the
/// refresh token on every one — so N simultaneous refusals rotating N times
/// means N-1 of them present an already-consumed token, and the server revokes
/// the entire grant. That is a hard logout produced by nothing but the CLI's
/// own concurrency. One rotation between them is the whole point.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_of_simultaneous_refusals_rotates_the_token_exactly_once() {
    const BURST: usize = 8;

    let backboard = crate::testkit::MockBackboard::spawn();
    backboard.stub_graphql_error("UserMeta", "Not Authorized");
    let token_endpoint = MockEndpoint::spawn(vec![fresh_tokens()]);
    let fixture = LiveFixture::new();

    let mut tasks = Vec::new();
    for _ in 0..BURST {
        // Every request builds its own `Configs` from the same file, exactly as
        // `post_graphql_for_current_session` does on each send.
        let path = fixture.path.clone();
        let url = backboard.url();
        let token_url = token_endpoint.base_url.clone();
        tasks.push(tokio::spawn(async move {
            let mut configs = Configs::for_test(path);
            configs.reload().unwrap();
            let client = crate::client::GQLClient::new_authorized(&configs).unwrap();
            crate::client::post_graphql_value::<serde_json::Value>(
                &client,
                reqwest::Url::parse(&url).unwrap(),
                &user_meta_body(),
                Some((&mut configs, &token_url)),
            )
            .await
        }));
    }

    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.unwrap());
    }

    assert_eq!(
        token_endpoint.hits(),
        1,
        "the burst must cost one token rotation between them, not {BURST}"
    );
    // Every caller still gets the right story: the grant is alive, so the
    // refusal is about the resource. None of them is left holding an error
    // caused by the coalescing itself.
    for result in &results {
        assert!(
            matches!(
                result,
                Err(crate::errors::RailwayError::OAuthInsufficientGrant)
            ),
            "got {result:?}"
        );
    }
    // The one refresh that did happen was persisted for everyone else.
    assert_eq!(fixture.load().get_refresh_token(), Some("new-refresh"));
}

/// Nothing to probe with (legacy token, no refresh token): the original
/// error stands untouched and the token endpoint is never contacted.
#[tokio::test]
async fn no_refresh_token_leaves_the_original_error_untouched() {
    let backboard = crate::testkit::MockBackboard::spawn();
    backboard.stub_graphql_error("UserMeta", "Not Authorized");
    let token_endpoint = MockEndpoint::spawn(vec![fresh_tokens()]);

    let dir = tempfile::tempdir().unwrap();
    let mut configs = Configs::for_test(dir.path().join("config.json"));
    configs.root_config.user.token = Some("legacy-token".to_string());
    configs.write_credentials().unwrap();

    let result = send_user_meta(&mut configs, &backboard, &token_endpoint.base_url).await;

    assert!(
        matches!(
            result,
            Err(crate::errors::RailwayError::Unauthorized
                | crate::errors::RailwayError::UnauthorizedLogin)
        ),
        "got {result:?}"
    );
    assert_eq!(token_endpoint.hits(), 0, "nothing to probe with");
    assert_eq!(backboard.hits(), 1, "no retry without a live probe");
}
