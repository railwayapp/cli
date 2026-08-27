//! Pinned IaC plan artifacts for CI.
//!
//! `railway config plan --out` writes the evaluated change set, the
//! environment etag it was computed against, and the git tree of `.railway/`.
//! `railway config apply --plan` applies that change set as-is. It does not
//! re-evaluate the authoring file. A drifted environment or a different
//! `.railway/` tree fails instead of silently planning something new.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{client::GQLClient, config::Configs};

use super::engine::{apply_change_set, fetch_config_etag};
use super::json::stable_stringify;

pub const KIND: &str = "railway.config.plan";
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SavedPlan {
    pub kind: String,
    pub version: u32,
    pub cli_version: String,
    pub source_tree: String,
    pub environment_id: String,
    pub config_etag: String,
    pub change_set_hash: String,
    pub change_set: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    pub destructive: bool,
}

pub fn change_set_hash(change_set: &Value) -> String {
    let digest = Sha256::digest(stable_stringify(change_set).as_bytes());
    format!("sha256:{digest:x}")
}

pub fn is_destructive_change_set(change_set: &Value) -> bool {
    change_set
        .get("changes")
        .and_then(Value::as_array)
        .map(|changes| {
            changes
                .iter()
                .any(|change| change.get("severity").and_then(Value::as_str) == Some("destructive"))
        })
        .unwrap_or(false)
}

pub fn source_tree(cwd: &Path, override_tree: Option<&str>) -> Result<String> {
    if let Some(tree) = override_tree {
        if tree.trim().is_empty() {
            bail!("--source-tree must not be empty");
        }
        return Ok(tree.trim().to_string());
    }
    detect_source_tree(cwd)
}

pub fn detect_source_tree(cwd: &Path) -> Result<String> {
    if let Some(tree) = git_railway_tree(cwd) {
        return Ok(tree);
    }
    hash_railway_dir(&cwd.join(".railway"))
}

fn git_railway_tree(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD:.railway"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let tree = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if tree.is_empty() { None } else { Some(tree) }
}

fn hash_railway_dir(dir: &Path) -> Result<String> {
    if !dir.is_dir() {
        bail!(
            "Could not pin .railway/: {} is not a directory and git has no HEAD:.railway tree",
            dir.display()
        );
    }
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for (relative, path) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(fs::read(path)?);
        hasher.update([0]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("Failed to read {}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        if name == ".DS_Store" {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, out)?;
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        out.push((relative, path));
    }
    Ok(())
}

pub fn from_runner_json(raw: &Value, source_tree: String) -> Result<SavedPlan> {
    let environment = raw
        .get("currentEnvironment")
        .context("Plan JSON is missing currentEnvironment")?;
    let environment_id = environment
        .get("environmentId")
        .and_then(Value::as_str)
        .context("Plan JSON is missing currentEnvironment.environmentId")?
        .to_string();
    let config_etag = environment
        .get("configEtag")
        .and_then(Value::as_str)
        .context(
            "Plan JSON is missing currentEnvironment.configEtag; re-run `railway config plan --out`",
        )?
        .to_string();
    let change_set = raw
        .get("changeSet")
        .cloned()
        .context("Plan JSON is missing changeSet")?;
    Ok(SavedPlan {
        kind: KIND.to_string(),
        version: VERSION,
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
        source_tree,
        environment_id,
        config_etag,
        change_set_hash: change_set_hash(&change_set),
        change_set,
        diff: raw.get("diff").and_then(Value::as_str).map(str::to_string),
        destructive: is_destructive_change_set(raw.get("changeSet").unwrap_or(&Value::Null)),
    })
}

pub fn write_plan(path: &Path, plan: &SavedPlan) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
    }
    let body = serde_json::to_string_pretty(plan)?;
    fs::write(path, body).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

pub fn read_plan(path: &Path) -> Result<SavedPlan> {
    let body = fs::read_to_string(path)
        .with_context(|| format!("Failed to read saved plan {}", path.display()))?;
    let plan: SavedPlan = serde_json::from_str(&body).with_context(|| {
        format!(
            "Saved plan {} is not a railway.config.plan file",
            path.display()
        )
    })?;
    if plan.kind != KIND {
        bail!(
            "Saved plan {} has kind {:?}, expected {KIND}",
            path.display(),
            plan.kind
        );
    }
    if plan.version != VERSION {
        bail!(
            "Saved plan {} has version {}, this CLI reads version {VERSION}",
            path.display(),
            plan.version
        );
    }
    let actual = change_set_hash(&plan.change_set);
    if actual != plan.change_set_hash {
        bail!(
            "Saved plan {} is corrupt: changeSetHash is {} but the change set hashes to {actual}",
            path.display(),
            plan.change_set_hash
        );
    }
    Ok(plan)
}

pub fn assert_source_tree(plan: &SavedPlan, cwd: &Path) -> Result<()> {
    let current = detect_source_tree(cwd)?;
    if current != plan.source_tree {
        bail!(
            "Merged .railway/ tree {current} does not match the planned tree {}. Re-run `railway config plan` on this revision; do not apply an unreviewed combination.",
            plan.source_tree
        );
    }
    Ok(())
}

pub async fn apply_saved_plan(configs: &Configs, plan: &SavedPlan) -> Result<Value> {
    let live = fetch_config_etag(configs, &plan.environment_id).await?;
    match live.as_deref() {
        Some(etag) if etag == plan.config_etag => {}
        Some(etag) => bail!(
            "The environment changed since this plan was computed (etag {etag}, plan {}). Re-run `railway config plan` and review the new diff.",
            plan.config_etag
        ),
        None => {
            bail!("The environment no longer reports a config etag. Re-run `railway config plan`.")
        }
    }

    let empty = plan
        .change_set
        .get("changes")
        .and_then(Value::as_array)
        .map(Vec::is_empty)
        .unwrap_or(true);
    if empty {
        return Ok(serde_json::json!({
            "id": null,
            "status": "noop",
            "changes": [],
        }));
    }

    let client = GQLClient::new_authorized(configs)?;
    let endpoint = configs.get_backboard();
    apply_change_set(
        &client,
        &endpoint,
        &plan.environment_id,
        &plan.change_set,
        Some(&plan.config_etag),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hashes_are_stable_under_key_reorder() {
        let a = json!({"version": 1, "changes": [{"path": "service.api", "kind": "update"}]});
        let b = json!({"changes": [{"kind": "update", "path": "service.api"}], "version": 1});
        assert_eq!(change_set_hash(&a), change_set_hash(&b));
    }

    #[test]
    fn detects_destructive_severity() {
        let set = json!({
            "changes": [
                {"summary": "add", "severity": "info"},
                {"summary": "delete service", "severity": "destructive"}
            ]
        });
        assert!(is_destructive_change_set(&set));
        assert!(!is_destructive_change_set(&json!({"changes": []})));
    }

    #[test]
    fn from_runner_json_requires_etag() {
        let raw = json!({
            "currentEnvironment": { "environmentId": "env_1" },
            "changeSet": { "version": 1, "changes": [] }
        });
        assert!(from_runner_json(&raw, "tree".into()).is_err());
    }

    #[test]
    fn from_runner_json_round_trips() {
        let raw = json!({
            "currentEnvironment": {
                "environmentId": "env_1",
                "configEtag": "etag_1"
            },
            "changeSet": { "version": 1, "changes": [] },
            "diff": "already up to date"
        });
        let plan = from_runner_json(&raw, "abc".into()).unwrap();
        assert_eq!(plan.kind, KIND);
        assert_eq!(plan.source_tree, "abc");
        assert_eq!(plan.config_etag, "etag_1");
        assert!(!plan.destructive);
        let encoded = serde_json::to_string(&plan).unwrap();
        let parsed: SavedPlan = serde_json::from_str(&encoded).unwrap();
        assert_eq!(parsed.change_set_hash, plan.change_set_hash);
    }

    #[test]
    fn read_plan_rejects_tampered_hash() {
        let dir = tempfile_dir("saved-plan");
        let path = dir.join("plan.json");
        let mut plan = from_runner_json(
            &json!({
                "currentEnvironment": {
                    "environmentId": "env_1",
                    "configEtag": "etag_1"
                },
                "changeSet": { "version": 1, "changes": [] }
            }),
            "tree".into(),
        )
        .unwrap();
        plan.change_set_hash = "sha256:deadbeef".into();
        write_plan(&path, &plan).unwrap();
        assert!(
            read_plan(&path)
                .unwrap_err()
                .to_string()
                .contains("corrupt")
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn hashes_railway_dir_when_git_tree_is_missing() {
        let dir = tempfile_dir("saved-plan-tree");
        fs::create_dir_all(dir.join(".railway")).unwrap();
        fs::write(dir.join(".railway").join("railway.ts"), "export default {}").unwrap();
        let tree = detect_source_tree(&dir).unwrap();
        assert!(tree.starts_with("sha256:"));
        assert_eq!(detect_source_tree(&dir).unwrap(), tree);
        let _ = fs::remove_dir_all(dir);
    }

    fn tempfile_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
