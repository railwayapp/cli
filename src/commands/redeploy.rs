use colored::*;
use is_terminal::IsTerminal;

use crate::{
    controllers::project::{
        find_service_instance, get_environment_instances, resolve_service_context,
    },
    util::{progress::create_spinner_if, prompt::prompt_confirm_with_default},
};

use super::*;
use anyhow::{anyhow, bail};

/// Redeploy the latest deployment of a service
///
/// By default the existing build or image is redeployed as-is. Pass
/// `--from-source` to pull the latest commit or image from the configured
/// source instead, or `--commit <SHA>` to deploy one specific commit once,
/// without pinning it in the environment config.
#[derive(Parser)]
pub struct Args {
    /// The service ID/name to redeploy from
    #[clap(long, short)]
    service: Option<String>,

    /// Environment to redeploy in (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    bypass: bool,

    /// Output in JSON format
    #[clap(long)]
    json: bool,

    /// Pull and deploy the latest commit or image from the configured source, instead of redeploying the existing deployment
    #[clap(long, conflicts_with = "commit")]
    from_source: bool,

    /// Deploy a specific commit from the connected repository once, without pinning it
    #[clap(long, value_name = "SHA", conflicts_with = "from_source")]
    commit: Option<String>,
}

/// What `railway redeploy` is being asked to do.
enum Mode {
    /// Redeploy the latest deployment as-is.
    Latest,
    /// Pull the latest commit or image from the configured source.
    FromSource,
    /// One-off deploy of a specific commit.
    Commit(String),
}

/// Trims, lowercases and checks that `input` looks like a git commit SHA:
/// 7 to 40 hex characters. Whether the commit exists is up to the server.
fn normalize_commit_sha(input: &str) -> Result<String> {
    let sha = input.trim().to_ascii_lowercase();
    if sha.len() < 7 || sha.len() > 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!(
            "Invalid commit SHA {:?}: expected 7 to 40 hexadecimal characters",
            input.trim()
        );
    }
    Ok(sha)
}

/// Short form of a SHA for messages, matching what git shows by default.
fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

pub async fn command(args: Args) -> Result<()> {
    let mode = match (args.from_source, args.commit.as_deref()) {
        (true, _) => Mode::FromSource,
        (false, Some(sha)) => Mode::Commit(normalize_commit_sha(sha)?),
        (false, None) => Mode::Latest,
    };

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let ctx = resolve_service_context(args.project, args.service, args.environment).await?;
    let project_id = ctx.project_id;
    let environment_id = ctx.environment_id;
    let environment_name = ctx.environment_name;
    let service_id = ctx.service_id;
    let service_name = &ctx.service_name;
    let service_node_id = service_id.clone();

    let environment_instances =
        get_environment_instances(&client, &configs, &project_id, &environment_id).await?;
    let service_in_env = find_service_instance(&environment_instances, &service_node_id)
        .ok_or_else(|| anyhow!("The service specified doesn't exist in the current environment"))?;

    let latest_deployment_id = match &mode {
        Mode::Latest => {
            let latest = service_in_env
                .latest_deployment
                .as_ref()
                .ok_or_else(|| anyhow!("No deployment found for service"))?;
            if !latest.can_redeploy {
                bail!(
                    "The latest deployment for service {service_name} cannot be redeployed. \
                    This may be because it's currently building, deploying, or was removed."
                );
            }
            Some(latest.id.clone())
        }
        Mode::FromSource => None,
        Mode::Commit(_) => {
            let has_repo = service_in_env
                .source
                .as_ref()
                .and_then(|s| s.repo.as_ref())
                .is_some();
            if !has_repo {
                bail!(
                    "Service {service_name} is not connected to a repository in environment \
                    {environment_name}, so there is no commit to deploy. Use --from-source \
                    to pull the latest image instead."
                );
            }
            None
        }
    };

    let (prompt_msg, spinner_msg, finish_msg) = match &mode {
        Mode::FromSource => (
            format!(
                "Pull the latest source (commit or image) and deploy service {service_name} in environment {environment_name}?"
            ),
            format!("Pulling the latest source and deploying service {service_name}..."),
            format!(
                "Triggered a deploy from the latest source for service {}",
                service_name.green()
            ),
        ),
        Mode::Commit(sha) => {
            let short = short_sha(sha);
            (
                format!(
                    "Deploy commit {short} to service {service_name} in environment {environment_name}?"
                ),
                format!("Deploying commit {short} to service {service_name}..."),
                format!(
                    "Triggered a deploy of commit {} for service {}",
                    short.bold(),
                    service_name.green()
                ),
            )
        }
        Mode::Latest => (
            format!(
                "Redeploy the latest deployment from service {service_name} in environment {environment_name}?"
            ),
            format!("Redeploying the latest deployment from service {service_name}..."),
            format!(
                "The latest deployment from service {} has been redeployed",
                service_name.green()
            ),
        ),
    };

    let confirmed = if args.bypass {
        true
    } else if std::io::stdout().is_terminal() {
        prompt_confirm_with_default(&prompt_msg, false)?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };

    if !confirmed {
        return Ok(());
    }

    let spinner = create_spinner_if(!args.json, spinner_msg);

    let json_output = match (mode, latest_deployment_id) {
        (Mode::FromSource, _) => {
            post_graphql::<mutations::ServiceInstanceDeployLatestCommit, _>(
                &client,
                configs.get_backboard(),
                mutations::service_instance_deploy_latest_commit::Variables {
                    environment_id,
                    service_id: service_id.clone(),
                },
            )
            .await?;
            serde_json::json!({ "success": true })
        }
        (Mode::Commit(sha), _) => {
            let response = post_graphql::<mutations::ServiceInstanceDeployCommit, _>(
                &client,
                configs.get_backboard(),
                mutations::service_instance_deploy_commit::Variables {
                    environment_id,
                    service_id: service_id.clone(),
                    commit_sha: sha,
                },
            )
            .await?;
            serde_json::json!({ "id": response.service_instance_deploy_v2 })
        }
        (Mode::Latest, Some(id)) => {
            let response = post_graphql::<mutations::DeploymentRedeploy, _>(
                &client,
                configs.get_backboard(),
                mutations::deployment_redeploy::Variables { id },
            )
            .await?;
            serde_json::json!({ "id": response.deployment_redeploy.id })
        }
        (Mode::Latest, None) => unreachable!("latest mode always resolves a deployment id"),
    };

    if args.json {
        println!("{json_output}");
    } else if let Some(spinner) = spinner {
        spinner.finish_with_message(finish_msg);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_short_and_full_shas() {
        assert_eq!(normalize_commit_sha("3f2a9c1").unwrap(), "3f2a9c1");
        assert_eq!(
            normalize_commit_sha("  3F2A9C1D  ").unwrap(),
            "3f2a9c1d",
            "trims and lowercases"
        );
        let full = "a".repeat(40);
        assert_eq!(normalize_commit_sha(&full).unwrap(), full);
    }

    #[test]
    fn rejects_malformed_shas() {
        for bad in [
            "",
            "abc",
            "3f2a9c",
            "zzzzzzz",
            "3f2a9c1 extra",
            &"a".repeat(41),
        ] {
            let err = normalize_commit_sha(bad).unwrap_err().to_string();
            assert!(err.contains("Invalid commit SHA"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn short_sha_is_at_most_seven_chars() {
        assert_eq!(short_sha("3f2a9c1d4e"), "3f2a9c1");
        assert_eq!(short_sha("3f2a9c1"), "3f2a9c1");
    }
}
