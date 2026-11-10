//! Persistent local audit trail for the managed-database commands.
//!
//! Every invocation appends one JSONL entry to `~/.railway/<engine>-ops.jsonl`
//! (override with `RAILWAY_<ENGINE>_OPS_LOG`, e.g. in tests): timestamp, CLI
//! version, the full argument vector, the project/environment/service
//! selectors in play, outcome and duration. PITR, HA and pooling compose --
//! and when a database ends up misconfigured after some sequence of feature
//! enables/converts/reverts, this trail is how that sequence gets
//! reconstructed (`railway <engine> history`, or just reading the file).
//!
//! The trail is per engine rather than one shared file, which keeps each
//! engine's history readable on its own and leaves the Postgres trail
//! (`postgres-ops.jsonl`, written since this command shipped) exactly where
//! it already is.
//!
//! Server-side command telemetry already exists but carries no resource
//! identifiers; this log is the local, resource-aware complement. Entries
//! older than [`RETENTION_DAYS`] are pruned on every append, so the file
//! stays bounded without a separate cleanup path. Everything here is
//! best-effort: an unwritable log must never fail the actual command.

use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::controllers::database_engines::DatabaseEngine;

pub const RETENTION_DAYS: i64 = 90;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsLogEntry {
    pub timestamp: DateTime<Utc>,
    pub cli_version: String,
    /// Full argv (minus the binary path). These subcommands carry no
    /// secrets -- ids, counts, names and timestamps only.
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_ms: u64,
}

pub fn log_path(engine: &DatabaseEngine) -> Option<PathBuf> {
    let override_var = format!("RAILWAY_{}_OPS_LOG", engine.key.to_ascii_uppercase());
    if let Ok(path) = std::env::var(&override_var)
        && !path.is_empty()
    {
        return Some(PathBuf::from(path));
    }
    dirs::home_dir().map(|home| {
        home.join(".railway")
            .join(format!("{}-ops.jsonl", engine.key))
    })
}

/// Appends `entry`, pruning anything older than the retention window.
/// Best-effort by contract: all errors are swallowed.
pub fn record(engine: &DatabaseEngine, entry: &OpsLogEntry) {
    let Some(path) = log_path(engine) else { return };
    let _ = record_at(&path, entry);
}

fn record_at(path: &PathBuf, entry: &OpsLogEntry) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let cutoff = Utc::now() - Duration::days(RETENTION_DAYS);
    let mut lines: Vec<String> = match std::fs::read_to_string(path) {
        Ok(existing) => existing
            .lines()
            .filter(|line| {
                serde_json::from_str::<OpsLogEntry>(line)
                    .map(|parsed| parsed.timestamp >= cutoff)
                    // Keep unparseable lines rather than silently dropping
                    // someone's data on a format change.
                    .unwrap_or(true)
            })
            .map(String::from)
            .collect(),
        Err(_) => Vec::new(),
    };
    lines.push(serde_json::to_string(entry)?);

    let mut file = std::fs::File::create(path)?;
    for line in &lines {
        writeln!(file, "{line}")?;
    }
    Ok(())
}

/// All retained entries, oldest first. Unparseable lines are skipped.
pub fn read_entries(engine: &DatabaseEngine) -> Vec<OpsLogEntry> {
    let Some(path) = log_path(engine) else {
        return Vec::new();
    };
    match std::fs::read_to_string(path) {
        Ok(contents) => contents
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ts: DateTime<Utc>, args: &[&str]) -> OpsLogEntry {
        OpsLogEntry {
            timestamp: ts,
            cli_version: "0.0.0-test".to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            project: Some("proj-1".to_string()),
            environment: Some("env-1".to_string()),
            service: Some("svc-1".to_string()),
            success: true,
            error: None,
            duration_ms: 42,
        }
    }

    #[test]
    fn each_engine_writes_its_own_trail() {
        use crate::controllers::database_engines::{MYSQL, POSTGRES, REDIS};

        let filename_for = |engine: &DatabaseEngine| {
            log_path(engine)
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        };
        // Postgres keeps the filename it has written since the command
        // shipped, so an existing trail stays where its owner left it.
        assert_eq!(filename_for(&POSTGRES), "postgres-ops.jsonl");
        assert_eq!(filename_for(&MYSQL), "mysql-ops.jsonl");
        assert_eq!(filename_for(&REDIS), "redis-ops.jsonl");
    }

    #[test]
    fn appends_and_reads_back_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ops.jsonl");

        record_at(&path, &entry(Utc::now(), &["postgres", "pitr", "enable"])).unwrap();
        record_at(&path, &entry(Utc::now(), &["postgres", "ha", "convert"])).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<OpsLogEntry> = contents
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].args[1], "pitr");
        assert_eq!(parsed[1].args[1], "ha");
    }

    #[test]
    fn prunes_entries_past_retention_on_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ops.jsonl");

        let stale = entry(
            Utc::now() - Duration::days(RETENTION_DAYS + 1),
            &["postgres", "pitr", "enable"],
        );
        let fresh = entry(Utc::now(), &["postgres", "pitr", "disable"]);
        record_at(&path, &stale).unwrap();
        record_at(&path, &fresh).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<OpsLogEntry> = contents
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(parsed.len(), 1, "stale entry pruned");
        assert_eq!(parsed[0].args[2], "disable");
    }

    #[test]
    fn unparseable_lines_survive_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ops.jsonl");
        std::fs::write(&path, "not json at all\n").unwrap();

        record_at(&path, &entry(Utc::now(), &["postgres", "pitr", "status"])).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.lines().count() == 2);
        assert!(contents.starts_with("not json at all"));
    }
}
