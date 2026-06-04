use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use whoami::print_user;

use crate::{
    consts::{RAILWAY_API_TOKEN_ENV, RAILWAY_TOKEN_ENV},
    controllers::user::get_user,
    errors::RailwayError,
    oauth,
    util::progress::create_spinner,
};

use super::*;

/// Sign in to Railway — also creates a new account if you don't have one.
///
/// Uses a single OAuth flow for both sign-in and sign-up. Brand-new
/// accounts are detected automatically and land on a welcome page;
/// existing users see the standard sign-in confirmation. Use
/// --browserless for SSH sessions, remote dev boxes, or any
/// environment where a local browser can't open.
#[derive(Parser)]
pub struct Args {
    /// Use a device-code flow instead of opening a browser. Prints
    /// a verification URL + short code to paste into a browser on
    /// any device. Required for SSH/headless sessions.
    #[clap(short, long)]
    pub browserless: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let mut configs = Configs::new()?;

    // Check for env var tokens first
    let token_name = if Configs::get_railway_token().is_some() {
        Some(RAILWAY_TOKEN_ENV)
    } else if Configs::get_railway_api_token().is_some() {
        Some(RAILWAY_API_TOKEN_ENV)
    } else {
        None
    };

    if let Some(token_name) = token_name {
        if let Ok(client) = GQLClient::new_authorized(&configs) {
            match get_user(&client, &configs).await {
                Ok(user) => {
                    let _ = configs.save_user_id(&user.id);
                    println!("{} found", token_name.bold());
                    print_user(user);
                    return Ok(());
                }
                Err(_e) => {
                    return Err(RailwayError::InvalidRailwayToken(token_name.to_string()).into());
                }
            }
        }
    }

    let host = configs.get_host();

    // `login` is an explicit auth request, so we always attempt it —
    // only the transport varies. Device-code when the user asked for it
    // (--browserless) or no local browser is reachable (CI/SSH/no
    // DISPLAY); otherwise open a browser. browser_login itself falls
    // back to device-code if `open` fails. Neither path needs a TTY.
    let ctx = crate::exec_context::ExecutionContext::detect(false, false);
    let (transport, result) = match ctx.login_transport(args.browserless) {
        crate::exec_context::AuthTransport::DeviceCode => {
            ("device_code", device_flow_login(host).await)
        }
        crate::exec_context::AuthTransport::Browser => ("browser", browser_login(host).await),
    };

    // Funnel telemetry: record the auth-attempt outcome. Sent via a
    // public mutation so timeouts/failures (no token yet) are captured,
    // attributed by caller/agent-session. Fires from here regardless of
    // whether `login` was invoked directly or chained from an unauthed
    // `up`.
    let outcome = match &result {
        Ok(_) => "succeeded",
        // Device-code expiry is a typed error; the browser path's
        // timeout is an anyhow context string ("Authentication timed
        // out…"), so check both.
        Err(e)
            if matches!(
                e.downcast_ref::<crate::errors::RailwayError>(),
                Some(crate::errors::RailwayError::OAuthDeviceCodeExpired)
            ) || e.to_string().contains("timed out") =>
        {
            "timed_out"
        }
        Err(_) => "failed",
    };
    crate::telemetry::send_auth_event(crate::telemetry::CliAuthTrackEvent {
        transport,
        outcome,
        success: result.is_ok(),
        error_message: result.as_ref().err().map(|e| e.to_string()),
    })
    .await;

    let token_resp = result?;

    configs.save_oauth_tokens(
        &token_resp.access_token,
        token_resp.refresh_token.as_deref(),
        token_resp.expires_in,
    )?;

    let client = GQLClient::new_authorized(&configs)?;
    let vars = queries::user_meta::Variables {};
    let me = post_graphql::<queries::UserMeta, _>(&client, configs.get_backboard(), vars)
        .await?
        .me;

    if let Err(e) = configs.save_user_id(&me.id) {
        eprintln!("{}: {e}", "Warning: failed to persist user id".yellow());
    }

    if let Some(name) = me.name {
        println!(
            "  {} Signed in as {} ({})",
            "✓".green(),
            name.bold(),
            me.email,
        );
    } else {
        println!("  {} Signed in as {}", "✓".green(), me.email);
    }

    Ok(())
}

/// Browser flow: Authorization Code + PKCE with localhost redirect.
async fn browser_login(host: &str) -> Result<oauth::TokenResponse> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let pkce = oauth::generate_pkce();
    let state = oauth::generate_state();
    let auth_url = oauth::get_authorization_url(host, &redirect_uri, &pkce, &state);
    // Sign-up vs sign-in is handled entirely server-side (the consent
    // screen adapts for brand-new accounts); the CLI doesn't declare
    // its intent up front.

    // Tell the user before we steal window focus, and show the URL up
    // front as a copy-paste fallback (wrong browser/profile/tab opened,
    // or debugging).
    println!();
    println!(
        "  {} Opening your browser to sign in — finish there.",
        "→".cyan()
    );
    println!("    {}", auth_url.bold().underline());
    println!();

    // If we can't open a browser, fall back to device-code flow (which
    // uses a different URL that doesn't need a localhost callback, and
    // prints its own verification URL + short code).
    if ::open::that(&auth_url).is_err() {
        println!(
            "  {} Couldn't open a browser — use the sign-in code below instead.",
            "→".cyan(),
        );
        drop(listener);
        return device_flow_login(host).await;
    }

    let spinner = create_spinner("Waiting for sign-in...".into());

    let result = tokio::time::timeout(
        Duration::from_secs(300),
        wait_for_callback(listener, &state, host),
    )
    .await;

    spinner.finish_and_clear();

    let code =
        result.context("Authentication timed out — no callback received after 5 minutes")??;

    oauth::exchange_authorization_code(host, &code, &redirect_uri, &pkce.code_verifier).await
}

/// Wait for the OAuth callback on the local TCP listener. Returns the authorization code.
/// Accepts connections in a loop so that browser preconnects or stray requests don't
/// consume the single chance to receive the real callback.
async fn wait_for_callback(
    listener: TcpListener,
    expected_state: &str,
    host: &str,
) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;

        let mut buf = Vec::with_capacity(4096);
        let mut tmp = [0u8; 1024];
        const MAX_REQUEST_SIZE: usize = 8192;

        let headers_ok = loop {
            let n = match stream.read(&mut tmp).await {
                Ok(0) | Err(_) => break false,
                Ok(n) => n,
            };
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break true;
            }
            if buf.len() > MAX_REQUEST_SIZE {
                break false;
            }
        };

        if !headers_ok {
            // Incomplete/oversized request — ignore and wait for the real callback
            continue;
        }

        let request = String::from_utf8_lossy(&buf);

        // Parse "GET /callback?code=xxx&state=yyy HTTP/1.1"
        let path = match request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
        {
            Some(p) if p.starts_with("/callback") => p,
            _ => continue, // Not our callback path — ignore
        };

        let parsed = url::Url::parse(&format!("http://localhost{path}"))
            .context("Failed to parse callback URL")?;

        // Check for OAuth error response
        if let Some((_, err)) = parsed.query_pairs().find(|(k, _)| k == "error") {
            let desc = parsed
                .query_pairs()
                .find(|(k, _)| k == "error_description")
                .map(|(_, v)| v.to_string())
                .unwrap_or_default();
            send_response(&mut stream, "Authentication failed", false, host).await;
            bail!("OAuth error: {err}: {desc}");
        }

        // Verify state parameter to prevent CSRF
        let received_state = parsed
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string());
        match received_state {
            Some(s) if s == expected_state => {}
            _ => {
                send_response(&mut stream, "Authentication failed", false, host).await;
                bail!("OAuth state parameter mismatch (possible CSRF attack)");
            }
        }

        let code = parsed
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.to_string())
            .context("No authorization code in callback")?;

        send_response(&mut stream, "Authentication successful!", true, host).await;

        return Ok(code);
    }
}

async fn send_response(
    stream: &mut tokio::net::TcpStream,
    message: &str,
    success: bool,
    host: &str,
) {
    let icon = if success { "&#10003;" } else { "&#10007;" };
    let accent = if success {
        "color: #22c55e"
    } else {
        "color: #ef4444"
    };
    let dots_url = format!("https://{host}/dots-oxipng.png");

    // On success, redirect the browser to the dashboard after a short
    // pause; the CLI's localhost callback doesn't need the tab to stay
    // open past this point — the token was captured before we serve
    // this response.
    let refresh_meta = if success {
        format!(r#"<meta http-equiv="refresh" content="2;url=https://{host}/dashboard">"#)
    } else {
        String::new()
    };
    let body_copy = if success {
        "Taking you to your dashboard…"
    } else {
        "You can close this window and return to your terminal."
    };

    let body = format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
{refresh_meta}
<title>Railway CLI</title>
<style>
  @import url('https://fonts.googleapis.com/css2?family=IBM+Plex+Serif:wght@600&display=swap');
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 100vh;
    background: #ffffff;
    color: #13111C;
    position: relative;
  }}
  body::before {{
    content: '';
    position: absolute;
    inset: 0;
    background-image: url({dots_url});
    background-size: cover;
    background-position: bottom;
    opacity: 0.08;
  }}
  @media (prefers-color-scheme: dark) {{
    body {{ background: #13111C; color: #ffffff; }}
    body::before {{ opacity: 0.2; }}
  }}
  .card {{
    position: relative;
    z-index: 1;
    text-align: center;
    padding: 3rem 2.5rem;
    border-radius: 0.75rem;
    max-width: 420px;
    width: 100%;
    margin: 1rem;
    background: linear-gradient(to bottom, #ffffff, #fafafa);
    border: 1px solid rgba(0, 0, 0, 0.05);
    box-shadow: 0 4px 24px rgba(0, 0, 0, 0.06);
  }}
  @media (prefers-color-scheme: dark) {{
    .card {{
      background: linear-gradient(to bottom, #2D2A3C, #292538);
      border: 1px solid rgba(255, 255, 255, 0.05);
      box-shadow: 0px 13px 29px rgba(20, 17, 29, 0.15),
                  0px 53px 53px rgba(20, 17, 29, 0.13);
    }}
  }}
  .icon {{ font-size: 2.5rem; margin-bottom: 1rem; {accent}; }}
  h1 {{
    font-family: 'IBM Plex Serif', Georgia, serif;
    font-weight: 600;
    font-size: 1.5rem;
    margin-bottom: 0.75rem;
  }}
  p {{
    font-size: 0.875rem;
    opacity: 0.6;
  }}
</style>
</head>
<body>
  <div class="card">
    <div class="icon">{icon}</div>
    <h1>{message}</h1>
    <p>{body_copy}</p>
  </div>
</body>
</html>"#
    );

    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.flush().await;
}

/// Browserless flow: Device Authorization Grant (RFC 8628).
async fn device_flow_login(host: &str) -> Result<oauth::TokenResponse> {
    let device_auth = oauth::request_device_code(host).await?;

    println!();
    println!("  {} Sign in at:", "→".cyan());
    println!("    {}", device_auth.verification_uri.bold().underline());
    println!();
    println!("  {} Enter this code:", "→".cyan());
    println!("    {}", device_auth.user_code.bold().purple());
    println!();

    let spinner = create_spinner("Waiting for sign-in...".into());

    let token_resp = oauth::poll_for_token(host, &device_auth).await?;

    spinner.finish_and_clear();
    Ok(token_resp)
}

/// Detect environments where opening a local browser will fail or
/// be useless, AND where we shouldn't try to show an interactive
/// picker. Used to skip the `open` attempt and go straight to
/// device-code flow, and to gate clack-style prompts in other commands.
///
/// Heuristics (intentionally conservative — false positives just
/// route the user to a non-interactive path, which still works):
/// - $CI is set to any non-empty value (CI runners have no GUI;
///   handles `CI=1`, `CI=true`, `CI=yes`, etc.)
/// - $SSH_CONNECTION is set (remote shell, browser would open on the
///   wrong machine)
/// - On Linux, $DISPLAY is empty (no X11 / Wayland session)
pub fn is_likely_headless() -> bool {
    if std::env::var("CI").is_ok() {
        return true;
    }
    if std::env::var("SSH_CONNECTION").is_ok() {
        return true;
    }
    if cfg!(target_os = "linux") && std::env::var("DISPLAY").unwrap_or_default().is_empty() {
        return true;
    }
    false
}
