use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    config::{Configs, Environment},
    errors::RailwayError,
};

pub const CLI_SCOPES: &str = "openid email profile offline_access workspace:admin project:admin";

const DEFAULT_OAUTH_CLIENT_ID: &str = "rlwy_oaci_onEklvmksh1hRUiCo7E2zX12";

pub fn get_oauth_client_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        std::env::var("RAILWAY_OAUTH_CLIENT_ID")
            .unwrap_or_else(|_| DEFAULT_OAUTH_CLIENT_ID.to_string())
    })
}

fn get_oauth_base_url(host: &str) -> String {
    format!("https://backboard.{host}/oauth")
}

fn build_http_client() -> Result<reqwest::Client> {
    let client = reqwest::Client::builder()
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
        .append_pair("prompt", "consent");
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

pub async fn request_device_code(host: &str) -> Result<DeviceAuthResponse> {
    let client = build_http_client()?;
    let url = format!("{}/device/auth", get_oauth_base_url(host));
    let client_id = get_oauth_client_id();

    let resp = client
        .post(&url)
        .form(&[("client_id", client_id), ("scope", CLI_SCOPES)])
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

pub async fn refresh_access_token(host: &str, refresh_token: &str) -> Result<TokenResponse> {
    let client = build_http_client()?;
    let url = format!("{}/token", get_oauth_base_url(host));
    let client_id = get_oauth_client_id();

    let resp = client
        .post(&url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .await?;

    let status = resp.status();
    if status.is_success() {
        let token_resp: TokenResponse = resp.json().await?;
        return Ok(token_resp);
    }

    let error_resp: TokenErrorResponse = resp.json().await.unwrap_or(TokenErrorResponse {
        error: "unknown".to_string(),
        error_description: Some(format!("HTTP {status}")),
    });
    let desc = error_resp.error_description.unwrap_or_default();
    Err(RailwayError::OAuthRefreshFailed(format!("{}: {desc}", error_resp.error)).into())
}
