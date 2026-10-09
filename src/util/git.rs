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
                    // The path is relative to the directory holding the
                    // pointer file when it is not absolute (submodules).
                    return Some(current.join(path.trim()));
                }
            }
        }
        if !current.pop() {
            return None;
        }
    }
}

/// The directory that holds `config`. A linked worktree has its own git
/// directory (with `HEAD`) but shares `config` with the main repository,
/// which the `commondir` file points at.
fn common_git_dir(git_dir: &Path) -> PathBuf {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(contents) if !contents.trim().is_empty() => git_dir.join(contents.trim()),
        _ => git_dir.to_path_buf(),
    }
}

/// Find the first GitHub remote in the repo, preferring `origin`.
/// Returns None if not in a git repo or no GitHub remote is set.
pub fn detect_github_remote(cwd: &Path) -> Option<GithubRemote> {
    let git_dir = find_git_dir(cwd)?;
    let config = std::fs::read_to_string(common_git_dir(&git_dir).join("config")).ok()?;

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

    fn write_config(git_dir: &Path, url: &str) {
        std::fs::create_dir_all(git_dir).unwrap();
        std::fs::write(
            git_dir.join("config"),
            format!("[remote \"origin\"]\n\turl = {url}\n"),
        )
        .unwrap();
    }

    #[test]
    fn detects_remote_in_regular_repo() {
        let dir = tempfile::tempdir().unwrap();
        write_config(&dir.path().join(".git"), "git@github.com:foo/bar.git");

        let remote = detect_github_remote(dir.path()).unwrap();
        assert_eq!(remote.full_repo_name(), "foo/bar");
    }

    #[test]
    fn detects_remote_in_linked_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        write_config(&main.join(".git"), "git@github.com:foo/bar.git");

        // A linked worktree keeps HEAD in `.git/worktrees/<name>` and points
        // back at the shared directory with `commondir`.
        let worktree_git = main.join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(&worktree_git).unwrap();
        std::fs::write(worktree_git.join("commondir"), "../..\n").unwrap();
        std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").unwrap();

        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .unwrap();

        let remote = detect_github_remote(&worktree).unwrap();
        assert_eq!(remote.full_repo_name(), "foo/bar");
        assert_eq!(detect_current_branch(&worktree).as_deref(), Some("feature"));
    }

    #[test]
    fn resolves_relative_gitdir_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let module_git = dir
            .path()
            .join("super")
            .join(".git")
            .join("modules")
            .join("sub");
        write_config(&module_git, "https://github.com/foo/sub.git");

        let sub = dir.path().join("super").join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();

        let remote = detect_github_remote(&sub).unwrap();
        assert_eq!(remote.full_repo_name(), "foo/sub");
    }
}
