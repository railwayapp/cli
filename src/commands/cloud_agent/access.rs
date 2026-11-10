//! The `CLOUD_AGENTS` preflight.
//!
//! Cloud agents are behind a Priority Boarding flag. Without it the API refuses
//! the create — but only once the launch has already resolved a credential, a
//! target and an agent, so what someone without the flag saw was a one-line
//! rejection at the end of a pipeline that looked like it was working. This
//! asks first, and answers with the two things that line didn't have: that the
//! flag is what is missing, and where to turn it on.
//!
//! Advisory, not a gate. Only a query that succeeds and comes back without the
//! flag stops a launch; anything else (a project token, which cannot read `me`,
//! or a transient failure) proceeds and lets the real call decide. A preflight
//! that blocks work the API would have allowed is worse than the error it
//! replaces.

use anyhow::{Result, bail};
use colored::Colorize;

use crate::client::post_graphql;
use crate::config::Configs;
use crate::gql::queries;

/// Stop before provisioning when the account doesn't have `CLOUD_AGENTS`.
pub async fn ensure_enabled(client: &reqwest::Client, configs: &Configs) -> Result<()> {
    let flags = match post_graphql::<queries::CloudAgentAccess, _>(
        client,
        configs.get_backboard(),
        queries::cloud_agent_access::Variables {},
    )
    .await
    {
        Ok(data) => data.me.feature_flags,
        Err(_) => return Ok(()),
    };
    if has_cloud_agents(&flags) {
        return Ok(());
    }
    super::telemetry::track_access_blocked().await;
    bail!(not_enabled_message(configs.get_host()));
}

fn has_cloud_agents(flags: &[queries::cloud_agent_access::ActiveFeatureFlag]) -> bool {
    use queries::cloud_agent_access::ActiveFeatureFlag;
    flags
        .iter()
        .any(|flag| matches!(flag, ActiveFeatureFlag::CLOUD_AGENTS))
}

/// What to say instead of the API's rejection. Names the flag, says where it
/// lives, and gives the URL — a flag nobody can find is the same as no flag.
fn not_enabled_message(host: &str) -> String {
    format!(
        "Cloud agents are not enabled for your account yet.\n\n  {} Turn on {} in Priority Boarding:\n    {}\n\nThen run this command again.",
        "→".cyan(),
        "Cloud Agents".bold(),
        format!("https://{host}/account/feature-flags").underline()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use queries::cloud_agent_access::ActiveFeatureFlag;

    #[test]
    fn the_flag_is_matched_exactly() {
        assert!(has_cloud_agents(&[
            ActiveFeatureFlag::PRIORITY_BOARDING,
            ActiveFeatureFlag::CLOUD_AGENTS,
        ]));
        // Priority Boarding on its own is the door, not the room behind it.
        assert!(!has_cloud_agents(&[ActiveFeatureFlag::PRIORITY_BOARDING]));
        assert!(!has_cloud_agents(&[]));
        // An unknown flag from a newer API must not read as a match.
        assert!(!has_cloud_agents(&[ActiveFeatureFlag::Other(
            "CLOUD_AGENTS_V2".into()
        )]));
    }

    #[test]
    fn the_message_points_at_the_flag_and_the_page() {
        let message = not_enabled_message("railway.com");
        assert!(message.contains("Priority Boarding"), "{message}");
        assert!(
            message.contains("https://railway.com/account/feature-flags"),
            "{message}"
        );
    }
}
