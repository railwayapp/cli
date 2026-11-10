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
    /// a sign-in link + short code to use from any device. Only
    /// needed when this machine truly has no browser — the CLI
    /// already auto-detects SSH, CI, and missing DISPLAY. If a human
    /// is at this machine (including under a coding agent), omit
    /// this flag: the browser flow completes far more reliably.
    #[clap(short, long)]
    pub browserless: bool,
}

pub async fn prompt_login() -> Result<()> {
    command(Args { browserless: false }).await
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
    let transport_choice = ctx.login_transport(args.browserless);

    // Why this transport was chosen, for funnel telemetry. Env
    // constraints win over the flag: "flag_browserless" is reported
    // only when a browser was otherwise reachable, so it counts exactly
    // the sessions a bare `railway login` would have sent down the
    // (far more reliable) browser path. browser_login overwrites both
    // labels if its `open` attempt fails and it falls back.
    let mut reason = match transport_choice {
        crate::exec_context::AuthTransport::Browser => "browser",
        crate::exec_context::AuthTransport::DeviceCode => {
            headless_reason().unwrap_or(if args.browserless {
                "flag_browserless"
            } else {
                "unknown"
            })
        }
    };

    // --browserless on a machine that has a browser: honor the flag,
    // but say so in the output. Agents (the dominant source of this
    // combination) read command output at exactly this moment, and the
    // browser path completes far more often for watched sessions.
    if reason == "flag_browserless" {
        println!(
            "  {} A browser is available on this machine — `railway login` without {} opens it directly and completes more reliably.",
            "→".cyan(),
            "--browserless".bold(),
        );
    }

    let mut transport = match transport_choice {
        crate::exec_context::AuthTransport::DeviceCode => "device_code",
        crate::exec_context::AuthTransport::Browser => "browser",
    };
    let result = match transport_choice {
        crate::exec_context::AuthTransport::DeviceCode => device_flow_login(host).await,
        crate::exec_context::AuthTransport::Browser => {
            browser_login(host, &mut transport, &mut reason).await
        }
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
    // Fire-and-forget: telemetry is a side effect, not something the user's
    // result should wait on. Awaiting it inline used to put a second,
    // independent network call (with its own timeout) in the critical path
    // of surfacing the *real* outcome to the user — on a flaky or blocked
    // network path, that doubles the worst-case wait before anything is
    // printed at all. `result?` below now runs immediately after the auth
    // attempt itself resolves.
    tokio::spawn(crate::telemetry::send_auth_event(
        crate::telemetry::CliAuthTrackEvent {
            transport,
            outcome,
            transport_reason: reason,
            success: result.is_ok(),
            error_message: result.as_ref().err().map(|e| e.to_string()),
        },
    ));

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
///
/// `transport` / `reason` are the funnel-telemetry labels owned by the
/// caller; they're overwritten here when the `open` attempt fails and
/// the flow falls back to device-code, so telemetry records the
/// transport that actually ran rather than the one that was chosen.
async fn browser_login(
    host: &str,
    transport: &mut &'static str,
    reason: &mut &'static str,
) -> Result<oauth::TokenResponse> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let pkce = oauth::generate_pkce();
    let state = oauth::generate_state();
    let caller = crate::telemetry::detect_caller();
    let auth_url = oauth::get_authorization_url(host, &redirect_uri, &pkce, &state, &caller);
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
        *transport = "device_code";
        *reason = "open_failed_fallback";
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
    let caller = crate::telemetry::detect_caller();
    let device_auth = oauth::request_device_code(host, &caller).await?;

    // Prefer the one-click link (code pre-embedded): a single URL
    // survives being relayed through an agent transcript far better
    // than a URL plus a separate code to transcribe. Keep the code
    // visible beneath for cross-device entry and as a fallback.
    println!();
    if let Some(ref one_click) = device_auth.verification_uri_complete {
        println!("  {} Sign in with one click:", "→".cyan());
        println!("    {}", one_click.bold().underline());
        println!();
        println!(
            "  {} Or go to {} and enter this code:",
            "→".cyan(),
            device_auth.verification_uri.underline(),
        );
        println!("    {}", device_auth.user_code.bold().purple());
    } else {
        println!("  {} Sign in at:", "→".cyan());
        println!("    {}", device_auth.verification_uri.bold().underline());
        println!();
        println!("  {} Enter this code:", "→".cyan());
        println!("    {}", device_auth.user_code.bold().purple());
    }
    println!();

    let spinner = create_spinner("Waiting for sign-in...".into());

    let token_resp = oauth::poll_for_token(host, &device_auth).await?;

    spinner.finish_and_clear();
    Ok(token_resp)
}

/// Why a local browser can't be used, if it can't. `None` means a
/// browser is plausibly reachable. The label doubles as the
/// `transport_reason` funnel-telemetry value, so keep values in sync
/// with the backend's accepted labels.
///
/// Heuristics (intentionally conservative — false positives just
/// route the user to a non-interactive path, which still works):
/// - $CI is truthy (CI runners have no GUI). Truthy, not merely set:
///   `CI=false` / `CI=0` / `CI=""` appear in real dotfiles and must
///   not route a desktop session to device-code.
/// - $SSH_CONNECTION is set (remote shell, browser would open on the
///   wrong machine)
/// - On Linux, $DISPLAY is empty (no X11 / Wayland session)
pub fn headless_reason() -> Option<&'static str> {
    if crate::config::Configs::env_is_ci() {
        return Some("env_ci");
    }
    if std::env::var("SSH_CONNECTION").is_ok() {
        return Some("env_ssh");
    }
    if cfg!(target_os = "linux") && std::env::var("DISPLAY").unwrap_or_default().is_empty() {
        return Some("no_display");
    }
    None
}

/// Detect environments where opening a local browser will fail or
/// be useless, AND where we shouldn't try to show an interactive
/// picker. Used to skip the `open` attempt and go straight to
/// device-code flow, and to gate clack-style prompts in other commands.
pub fn is_likely_headless() -> bool {
    headless_reason().is_some()
}
