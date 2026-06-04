use anyhow::{Result, bail};
use reqwest::Client;

use crate::{
    client::post_graphql,
    config::Configs,
    gql::queries::{self, git_hub_repos::GitHubReposGithubRepos},
};

pub async fn resolve_repo_branch(
    client: &Client,
    configs: &Configs,
    repo: &str,
    branch: Option<String>,
) -> Result<String> {
    if let Some(branch) = branch {
        return Ok(branch);
    }

    let repos = post_graphql::<queries::GitHubRepos, _>(
        client,
        configs.get_backboard(),
        queries::git_hub_repos::Variables {},
    )
    .await?
    .github_repos;

    default_branch_for_repo(&repos, repo).map(str::to_string).ok_or_else(|| {
        anyhow::anyhow!(
            "Branch is required because repo `{repo}` was not found in your connected GitHub repos. Pass --branch or connect the repo to the Railway GitHub App."
        )
    })
}

pub fn default_branch_for_repo<'a>(
    repos: &'a [GitHubReposGithubRepos],
    repo: &str,
) -> Option<&'a str> {
    repos
        .iter()
        .find(|candidate| candidate.full_name.eq_ignore_ascii_case(repo))
        .map(|repo| repo.default_branch.as_str())
}

pub fn validate_repo_name(repo: &str) -> Result<()> {
    let parts: Vec<_> = repo.split('/').collect();
    if parts.len() != 2 || parts.iter().any(|part| part.trim().is_empty()) {
        bail!("Repo must be in owner/repo format");
    }

    Ok(())
}
