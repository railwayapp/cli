use anyhow::{Result, bail};

use crate::{
    Configs,
    client::post_graphql,
    errors::RailwayError,
    gql::{mutations, queries},
    util::prompt::prompt_text,
};

/// Validates 2FA if enabled for the current user.
/// Skips check for token-based auth (API tokens bypass 2FA on the backend) and for
/// OAuth tokens (backboard exempts OAuth from 2FA, and the twoFactorInfoValidate
/// mutation requires a browser session that OAuth does not have).
/// For session-based auth, prompts for 2FA code if enabled, or uses provided code.
pub async fn validate_two_factor_if_enabled(
    client: &reqwest::Client,
    configs: &Configs,
    is_terminal: bool,
    two_factor_code: Option<String>,
) -> Result<()> {
    // Skip 2FA check for token-based auth and OAuth tokens — backboard does not
    // enforce 2FA for either, and the validate mutation is unreachable without a session.
    if Configs::is_using_token_auth() || configs.has_oauth_token() {
        return Ok(());
    }

    let is_two_factor_enabled = {
        let vars = queries::two_factor_info::Variables {};
        let info = post_graphql::<queries::TwoFactorInfo, _>(client, configs.get_backboard(), vars)
            .await?
            .two_factor_info;
        info.is_verified
    };

    if is_two_factor_enabled {
        let token = if let Some(code) = two_factor_code {
            code
        } else if is_terminal {
            prompt_text("Enter your 2FA code")?
        } else {
            bail!(
                "2FA is enabled and requires interactive mode. Use --2fa-code <CODE> or an API token for non-interactive operations."
            );
        };

        let vars = mutations::validate_two_factor::Variables { token };

        let valid =
            post_graphql::<mutations::ValidateTwoFactor, _>(client, configs.get_backboard(), vars)
                .await?
                .two_factor_info_validate;

        if !valid {
            return Err(RailwayError::InvalidTwoFactorCode.into());
        }
    }

    Ok(())
}
