//! Minimal git config introspection. Parses `.git/config` and `.git/HEAD`
//! directly so we don't shell out or take a `git2` dependency.
//!
//! Used by `railway up --new` to detect whether the current
//! directory has a GitHub remote — when it does, we can deploy from
//! the repo instead of bundling and uploading a tarball.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct GithubRemote {
    /// Remote alias name (e.g. "origin").
    pub name: String,
    pub owner: String,
    pub repo: String,
}

impl GithubRemote {
    pub fn full_repo_name(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// Walk up from `start` looking for a `.git` directory or pointer
/// file. Returns the resolved git directory (the place that holds
/// `config`, `HEAD`, etc.) or None if not in a git repo.
fn find_git_dir(start: &Path) -> Option<PathBuf> {
    let mut current = start.canonicalize().ok()?;
    loop {
        let candidate = current.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        // Git worktrees use a `.git` *file* pointing at the real
        // gitdir (e.g. `gitdir: /path/to/main/.git/worktrees/foo`).
        if candidate.is_file() {
            if let Ok(contents) = std::fs::read_to_string(&candidate) {
                if let Some(path) = contents.strip_prefix("gitdir: ") {
                    return Some(PathBuf::from(path.trim()));
                }
            }
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Find the first GitHub remote in the repo, preferring `origin`.
/// Returns None if not in a git repo or no GitHub remote is set.
pub fn detect_github_remote(cwd: &Path) -> Option<GithubRemote> {
    let git_dir = find_git_dir(cwd)?;
    let config = std::fs::read_to_string(git_dir.join("config")).ok()?;

    let mut current_remote: Option<String> = None;
    let mut found: Vec<GithubRemote> = Vec::new();

    for line in config.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("[remote \"") {
            current_remote = rest.strip_suffix("\"]").map(str::to_owned);
            continue;
        }
        if trimmed.starts_with('[') {
            current_remote = None;
            continue;
        }
        let Some(name) = current_remote.clone() else {
            continue;
        };
        let url_value = trimmed
            .strip_prefix("url = ")
            .or_else(|| trimmed.strip_prefix("url="));
        if let Some(url) = url_value {
            if let Some((owner, repo)) = parse_github_url(url) {
                found.push(GithubRemote { name, owner, repo });
            }
        }
    }

    // Prefer origin; otherwise first match wins.
    found.sort_by_key(|r| if r.name == "origin" { 0 } else { 1 });
    found.into_iter().next()
}

/// Parse a github.com remote URL into (owner, repo). Accepts both
/// HTTPS and SSH forms with or without the trailing `.git`.
fn parse_github_url(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim().trim_end_matches('/');
    let cleaned = trimmed.strip_suffix(".git").unwrap_or(trimmed);

    let path = cleaned
        .strip_prefix("https://github.com/")
        .or_else(|| cleaned.strip_prefix("http://github.com/"))
        .or_else(|| cleaned.strip_prefix("git@github.com:"))
        .or_else(|| cleaned.strip_prefix("ssh://git@github.com/"))?;

    let mut parts = path.splitn(2, '/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_owned(), repo.to_owned()))
}

/// Read the current branch from `.git/HEAD`. Returns None for
/// detached HEAD (caller should fall back to the default branch).
pub fn detect_current_branch(cwd: &Path) -> Option<String> {
    let git_dir = find_git_dir(cwd)?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let trimmed = head.trim();
    trimmed.strip_prefix("ref: refs/heads/").map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_url() {
        assert_eq!(
            parse_github_url("https://github.com/foo/bar.git"),
            Some(("foo".to_owned(), "bar".to_owned()))
        );
    }

    #[test]
    fn parses_ssh_url() {
        assert_eq!(
            parse_github_url("git@github.com:foo/bar.git"),
            Some(("foo".to_owned(), "bar".to_owned()))
        );
    }

    #[test]
    fn parses_url_without_dot_git() {
        assert_eq!(
            parse_github_url("https://github.com/foo/bar"),
            Some(("foo".to_owned(), "bar".to_owned()))
        );
    }

    #[test]
    fn rejects_non_github() {
        assert_eq!(parse_github_url("https://gitlab.com/foo/bar.git"), None);
    }
}
