use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    config::{Configs, Environment},
    consts,
    errors::RailwayError,
};

pub const CLI_SCOPES: &str =
    "openid email profile offline_access workspace:admin project:admin ssh_keys";

const DEFAULT_OAUTH_CLIENT_ID: &str = "rlwy_oaci_onEklvmksh1hRUiCo7E2zX12";

pub fn get_oauth_client_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        std::env::var("RAILWAY_OAUTH_CLIENT_ID")
            .unwrap_or_else(|_| DEFAULT_OAUTH_CLIENT_ID.to_string())
    })
}

pub(crate) fn get_oauth_base_url(host: &str) -> String {
    format!("https://backboard.{host}/oauth")
}

fn build_http_client() -> Result<reqwest::Client> {
    let client = reqwest::Client::builder()
        .user_agent(consts::get_user_agent())
        .danger_accept_invalid_certs(matches!(Configs::get_environment_id(), Environment::Dev))
        .timeout(Duration::from_secs(30))
        .build()?;
    Ok(client)
}

pub struct PkceChallenge {
    pub code_verifier: String,
    pub code_challenge: String,
}

pub fn generate_pkce() -> PkceChallenge {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::thread_rng();
    let code_verifier: String = (0..128)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect();

    let hash = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = URL_SAFE_NO_PAD.encode(hash);

    PkceChallenge {
        code_verifier,
        code_challenge,
    }
}

pub fn generate_state() -> String {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.r#gen::<u8>()).collect();
    URL_SAFE_NO_PAD.encode(&bytes)
}

pub fn get_authorization_url(
    host: &str,
    redirect_uri: &str,
    pkce: &PkceChallenge,
    state: &str,
    caller: &str,
) -> String {
    let base = get_oauth_base_url(host);
    let client_id = get_oauth_client_id();
    let mut url = url::Url::parse(&format!("{base}/auth")).expect("valid base URL");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", CLI_SCOPES)
        .append_pair("code_challenge", &pkce.code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("prompt", "consent")
        // Caller (agent harness / tty) that initiated this login, forwarded so
        // the OAuth grant confirm event can attribute the signup to the agent
        // that drove it (mono#30906). Server registers it as a no-op extra
        // param; analytics-only — same value we stamp on cli_submit_auth.
        .append_pair("cli_caller", caller);
    url.to_string()
}

pub async fn exchange_authorization_code(
    host: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<TokenResponse> {
    let client = build_http_client()?;
    let url = format!("{}/token", get_oauth_base_url(host));
    let client_id = get_oauth_client_id();

    let resp = client
        .post(&url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await?;

    if resp.status().is_success() {
        let token_resp: TokenResponse = resp.json().await?;
        return Ok(token_resp);
    }

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    bail!("Token exchange failed (HTTP {status}): {body}");
}

fn default_interval() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
pub struct DeviceAuthResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// RFC 8628 §3.3.1: the verification URI with the user code
    /// pre-embedded — one click instead of URL + code transcription.
    /// Optional in the spec; absent from older backends.
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: i64,
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
    error_description: Option<String>,
}

pub async fn request_device_code(host: &str, caller: &str) -> Result<DeviceAuthResponse> {
    let client = build_http_client()?;
    let url = format!("{}/device/auth", get_oauth_base_url(host));
    let client_id = get_oauth_client_id();

    let resp = client
        .post(&url)
        // `cli_caller`: see get_authorization_url — attributes the signup to
        // the agent/harness that drove it on the grant confirm event.
        .form(&[
            ("client_id", client_id),
            ("scope", CLI_SCOPES),
            ("cli_caller", caller),
        ])
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Device authorization request failed (HTTP {status}): {body}");
    }

    let device_auth: DeviceAuthResponse = resp.json().await?;
    Ok(device_auth)
}

pub async fn poll_for_token(host: &str, device_auth: &DeviceAuthResponse) -> Result<TokenResponse> {
    let client = build_http_client()?;
    let url = format!("{}/token", get_oauth_base_url(host));
    let client_id = get_oauth_client_id();

    let mut poll_interval = Duration::from_secs(device_auth.interval);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(device_auth.expires_in);

    loop {
        tokio::time::sleep(poll_interval).await;

        if tokio::time::Instant::now() >= deadline {
            return Err(RailwayError::OAuthDeviceCodeExpired.into());
        }

        let resp = client
            .post(&url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &device_auth.device_code),
                ("client_id", client_id),
            ])
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if status.is_success() {
            let token_resp: TokenResponse = serde_json::from_str(&body)
                .context(format!("Failed to parse token response: {body}"))?;
            return Ok(token_resp);
        }

        let error_resp: TokenErrorResponse = match serde_json::from_str(&body) {
            Ok(e) => e,
            Err(_) => bail!("Unexpected error response (HTTP {status}): {body}"),
        };
        match error_resp.error.as_str() {
            "authorization_pending" => {
                // Keep polling
            }
            "slow_down" => {
                poll_interval += Duration::from_secs(5);
            }
            "expired_token" => {
                return Err(RailwayError::OAuthDeviceCodeExpired.into());
            }
            "access_denied" => {
                return Err(RailwayError::OAuthAccessDenied.into());
            }
            other => {
                let desc = error_resp.error_description.unwrap_or_default();
                return Err(RailwayError::OAuthError(format!("{other}: {desc}")).into());
            }
        }
    }
}

/// Why a refresh attempt failed, and whether the stored credentials are still
/// worth keeping.
///
/// Wraps the [`RailwayError`] that [`classify_refresh_error`] already produces,
/// so the permanent-vs-retryable decision lives in exactly one place and the
/// typed error survives all the way to the caller.
#[derive(Debug)]
pub enum RefreshFailure {
    /// The grant is dead server-side. Clear the credentials and re-login.
    Terminal(RailwayError),
    /// Transient — keep the credentials and try again later.
    Transient(RailwayError),
}

impl RefreshFailure {
    fn classify(error: &str, desc: String) -> Self {
        match classify_refresh_error(error, desc) {
            permanent @ RailwayError::OAuthInvalidGrant(_) => Self::Terminal(permanent),
            other => Self::Transient(other),
        }
    }
}

/// Number of attempts for a transient failure (1 initial + 2 retries).
pub const REFRESH_MAX_ATTEMPTS: u32 = 3;

/// Refresh against an explicit OAuth base URL, retrying transient failures with
/// exponential backoff. Takes a base URL (rather than a host) so the retry and
/// classification policy can be exercised against a local server in tests.
///
/// Hand-rolled rather than using [`crate::util::retry::retry_with_backoff`]:
/// that helper retries every `Err`, and here a `Terminal` failure must abort
/// immediately — expressing that through it would mean returning
/// `Result<Result<_, RefreshFailure>>` from the closure, which reads worse than
/// the loop.
pub async fn refresh_access_token_at(
    base_url: &str,
    refresh_token: &str,
    backoff: Duration,
) -> std::result::Result<TokenResponse, RefreshFailure> {
    // One client for all attempts: building one re-parses the whole system root
    // store (~80ms) and starts with a cold connection pool, so rebuilding per
    // attempt would add ~160ms of pure setup to a retried refresh.
    let client = build_http_client()
        .map_err(|e| RefreshFailure::Transient(RailwayError::OAuthRefreshFailed(e.to_string())))?;

    for attempt in 0..REFRESH_MAX_ATTEMPTS {
        if attempt > 0 {
            // Exponential backoff. A refresh storm after an outage is itself a
            // load problem, so back off rather than hammering a recovering
            // token endpoint.
            tokio::time::sleep(backoff * (1 << (attempt - 1))).await;
        }

        match attempt_refresh(&client, base_url, refresh_token).await {
            Ok(resp) => return Ok(resp),
            // A dead grant will still be dead on the next attempt; fail fast so
            // the caller can clear credentials and prompt a login.
            Err(failure @ RefreshFailure::Terminal(_)) => return Err(failure),
            // Out of attempts — surface the last cause rather than a summary.
            Err(failure) if attempt + 1 == REFRESH_MAX_ATTEMPTS => return Err(failure),
            Err(_) => {}
        }
    }

    unreachable!("the final attempt returns rather than falling through")
}

pub(crate) async fn attempt_refresh(
    client: &reqwest::Client,
    base_url: &str,
    refresh_token: &str,
) -> std::result::Result<TokenResponse, RefreshFailure> {
    let url = format!("{base_url}/token");
    let client_id = get_oauth_client_id();

    let resp = client
        .post(&url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .await
        // Connection refused / reset / timeout: the credentials are untouched.
        .map_err(|e| {
            RefreshFailure::Transient(RailwayError::OAuthRefreshFailed(format!(
                "network error: {e}"
            )))
        })?;

    let status = resp.status();
    if status.is_success() {
        return resp.json::<TokenResponse>().await.map_err(|e| {
            RefreshFailure::Transient(RailwayError::OAuthRefreshFailed(format!(
                "malformed token response: {e}"
            )))
        });
    }

    // 5xx is the server's problem, never the token's — don't even look at the
    // body for an error code.
    if status.is_server_error() {
        return Err(RefreshFailure::Transient(RailwayError::OAuthRefreshFailed(
            format!("HTTP {status}"),
        )));
    }

    let error_resp: TokenErrorResponse = resp.json().await.unwrap_or(TokenErrorResponse {
        error: "unknown".to_string(),
        error_description: Some(format!("HTTP {status}")),
    });
    let desc = error_resp.error_description.unwrap_or_default();
    Err(RefreshFailure::classify(&error_resp.error, desc))
}

/// Split token-endpoint failures into permanent and retryable.
///
/// RFC 6749 §5.2: `invalid_grant` means the refresh token is revoked,
/// expired, or unknown to the server — no retry can ever succeed, so the
/// caller deletes the stored credential. Everything else (5xx, rate limits,
/// `unknown` from an unparseable body) may succeed later and must keep its
/// tokens: backboard serves real 500s on `/token` from Redis and Postgres
/// pool exhaustion, and treating those as permanent would sign every active
/// user out during an outage.
fn classify_refresh_error(error: &str, desc: String) -> RailwayError {
    if error == "invalid_grant" {
        return RailwayError::OAuthInvalidGrant(desc);
    }
    RailwayError::OAuthRefreshFailed(format!("{error}: {desc}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_grant_is_permanent() {
        let err = classify_refresh_error("invalid_grant", "refresh token not found".into());
        assert!(matches!(err, RailwayError::OAuthInvalidGrant(_)));
    }

    /// The property that keeps a backboard outage from becoming a mass
    /// logout: only `invalid_grant` may clear credentials.
    #[test]
    fn transient_failures_are_not_permanent() {
        for error in [
            "unknown",      // unparseable body, e.g. a 500 HTML page
            "server_error", // Redis/Prisma pool exhaustion on /token
            "temporarily_unavailable",
            "slow_down",
            "invalid_request",
            "invalid_client",
        ] {
            assert!(
                !matches!(
                    classify_refresh_error(error, "boom".into()),
                    RailwayError::OAuthInvalidGrant(_)
                ),
                "{error} must not be treated as a permanently dead credential"
            );
        }
    }
}
