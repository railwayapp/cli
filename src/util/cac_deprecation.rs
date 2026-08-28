//! Warn when legacy Config as Code files (`railway.json` / `railway.toml`)
//! are present in the working tree. Signal-only: no behavior change.

use std::path::{Path, PathBuf};

use colored::Colorize;

use crate::util::reporter;

const DOCS_URL: &str =
    "https://docs.railway.com/infrastructure-as-code#migrating-from-config-as-code";
const DISABLE_ENV: &str = "RAILWAY_CAC_DEPRECATION_WARNING";

fn disabled_by_env() -> bool {
    matches!(
        std::env::var(DISABLE_ENV).as_deref(),
        Ok("0" | "false" | "off")
    )
}

fn command_is_exempt(command: &str) -> bool {
    matches!(
        command,
        "autoupdate"
            | "check_updates"
            | "check-updates"
            | "completion"
            | "docs"
            | "help"
            | "login"
            | "logout"
            | "mcp"
            | "setup"
            | "skills"
            | "telemetry"
            | "telemetry_cmd"
            | "upgrade"
            | "whoami"
    )
}

fn should_skip_for_args(raw_args: &[String]) -> bool {
    raw_args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--json" | "--help" | "-h" | "--version" | "-V"
        )
    })
}

/// Find a Config as Code file in `dir` or its ancestors (up to the filesystem root).
/// Prefers `railway.toml` over `railway.json` when both exist in the same directory,
/// matching builderv3 selection.
pub fn find_cac_file(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let toml = current.join("railway.toml");
        if toml.is_file() {
            return Some(toml);
        }
        let json = current.join("railway.json");
        if json.is_file() {
            return Some(json);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Find every `railway.toml` / `railway.json` under `start`, preferring toml
/// when both exist in the same directory. Respects `.gitignore`.
pub fn find_all_cac_files(start: &Path) -> Vec<PathBuf> {
    use std::collections::BTreeMap;

    let mut by_dir: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    let walker = ignore::WalkBuilder::new(start)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str());
        let Some(name) = name else {
            continue;
        };
        if name != "railway.toml" && name != "railway.json" {
            continue;
        }
        if path.components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some(
                    ".railway"
                        | "node_modules"
                        | "target"
                        | "vendor"
                        | "dist"
                        | ".next"
                        | "__pycache__"
                        | ".venv"
                        | "venv"
                )
            )
        }) {
            continue;
        }
        let Some(dir) = path.parent() else {
            continue;
        };
        match by_dir.get(dir) {
            Some(existing) if existing.extension().and_then(|e| e.to_str()) == Some("toml") => {}
            _ => {
                by_dir.insert(dir.to_path_buf(), path.to_path_buf());
            }
        }
    }
    by_dir.into_values().collect()
}

fn display_path(path: &Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| {
            path.strip_prefix(&cwd)
                .ok()
                .map(|p| p.display().to_string())
        })
        .unwrap_or_else(|| path.display().to_string())
}

/// Emit a deprecation warning when a CaC file is found near the cwd.
pub fn maybe_warn(raw_args: &[String], command: Option<&str>) {
    if disabled_by_env() || should_skip_for_args(raw_args) {
        return;
    }
    let Some(command) = command else {
        return;
    };
    if command_is_exempt(command) {
        return;
    }

    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Some(path) = find_cac_file(&cwd) else {
        return;
    };

    let shown = display_path(&path);
    let message = format!(
        "Config as Code (railway.json / railway.toml) is deprecated. Prefer Infrastructure as Code (.railway/railway.ts). Run `railway config migrate` or see https://docs.railway.com/infrastructure-as-code#migrating-from-config-as-code"
    );
    let hint = format!("Migrate: `railway config migrate` — {DOCS_URL}");

    reporter::warn("CAC_DEPRECATED", message, Some(&hint));

    // Human mode: extra dimmed guidance line for discoverability.
    if reporter::mode() == reporter::OutputMode::Human {
        eprintln!(
            "  {}",
            "Existing files keep working until 2026-12-01.".dimmed()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_toml_in_cwd() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("railway.toml"), "[build]\n").unwrap();
        let found = find_cac_file(dir.path()).unwrap();
        assert!(found.ends_with("railway.toml"));
    }

    #[test]
    fn prefers_toml_over_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("railway.toml"), "[build]\n").unwrap();
        fs::write(dir.path().join("railway.json"), "{}\n").unwrap();
        let found = find_cac_file(dir.path()).unwrap();
        assert!(found.ends_with("railway.toml"));
    }

    #[test]
    fn finds_json_when_no_toml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("railway.json"), "{}\n").unwrap();
        let found = find_cac_file(dir.path()).unwrap();
        assert!(found.ends_with("railway.json"));
    }

    #[test]
    fn walks_parents() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("railway.toml"), "[build]\n").unwrap();
        let nested = dir.path().join("apps").join("web");
        fs::create_dir_all(&nested).unwrap();
        let found = find_cac_file(&nested).unwrap();
        assert!(found.ends_with("railway.toml"));
    }

    #[test]
    fn returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_cac_file(dir.path()).is_none());
    }

    #[test]
    fn finds_all_cac_files_in_monorepo() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("packages/web")).unwrap();
        fs::create_dir_all(dir.path().join("packages/api")).unwrap();
        fs::create_dir_all(dir.path().join("packages/web/node_modules/other")).unwrap();
        fs::write(dir.path().join("packages/web/railway.json"), "{}\n").unwrap();
        fs::write(dir.path().join("packages/api/railway.toml"), "[build]\n").unwrap();
        fs::write(
            dir.path()
                .join("packages/web/node_modules/other/railway.json"),
            "{}\n",
        )
        .unwrap();
        let found = find_all_cac_files(dir.path());
        assert_eq!(found.len(), 2);
        assert!(
            found
                .iter()
                .any(|p| p.ends_with("packages/web/railway.json"))
        );
        assert!(
            found
                .iter()
                .any(|p| p.ends_with("packages/api/railway.toml"))
        );
    }

    #[test]
    fn find_all_prefers_toml_in_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("railway.toml"), "[build]\n").unwrap();
        fs::write(dir.path().join("railway.json"), "{}\n").unwrap();
        let found = find_all_cac_files(dir.path());
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("railway.toml"));
    }
}
