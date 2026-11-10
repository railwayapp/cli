//! Shared, best-effort update outcomes. Keep this separate from the release
//! cache: a newer available release must not erase an installed update receipt.
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SkillsOutcome {
    Pending,
    NotInstalled,
    Synced { revision: String },
    Preserved { revision: String, count: u32 },
    Failed { message: String },
}

impl SkillsOutcome {
    pub fn summary(&self) -> String {
        match self {
            Self::Pending => "Agent skills synchronization pending".into(),
            Self::NotInstalled => "No CLI-managed agent skills installed".into(),
            Self::Synced { .. } => "Agent skills synchronized".into(),
            Self::Preserved { count, .. } => {
                let plural = if *count == 1 { "" } else { "s" };
                format!("{count} agent skill{plural} preserved — local edits detected")
            }
            Self::Failed { .. } => "Agent skills could not be synchronized".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsSync {
    pub cli_version: String,
    pub outcome: SkillsOutcome,
}

#[derive(Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateStatus {
    pub last_seen_version: Option<String>,
    pub installed_version: Option<String>,
    pub skills: Option<SkillsSync>,
    last_notified_version: Option<String>,
    last_available_notice: Option<String>,
}

impl UpdateStatus {
    pub fn read(home: &Path) -> Self {
        std::fs::read(home.join(".railway/update-status.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn observe(&mut self, version: &str, previous: Option<&str>) {
        let previous = self.last_seen_version.as_deref().or(previous);
        if previous.is_some_and(|previous| previous != version)
            && self.installed_version.as_deref().is_none_or(|installed| {
                super::compare_semver::compare_semver(installed, version)
                    != std::cmp::Ordering::Greater
            })
        {
            self.installed_version = Some(version.to_string());
        }
        self.last_seen_version = Some(version.to_string());
    }

    fn set_skills(&mut self, version: &str, outcome: SkillsOutcome) {
        // A detached process from an older binary can finish after the new one.
        if self.installed_version.as_deref().is_some_and(|installed| {
            super::compare_semver::compare_semver(installed, version) == std::cmp::Ordering::Greater
        }) || self.skills.as_ref().is_some_and(|sync| {
            super::compare_semver::compare_semver(&sync.cli_version, version)
                == std::cmp::Ordering::Greater
        }) {
            return;
        }
        self.skills = Some(SkillsSync {
            cli_version: version.into(),
            outcome,
        });
    }

    fn receipt(&mut self, running: &str) -> Option<String> {
        if self.installed_version.as_deref() != Some(running)
            || self.last_notified_version.as_deref() == Some(running)
        {
            return None;
        }
        let sync = self
            .skills
            .as_ref()
            .filter(|sync| sync.cli_version == running)?;
        let suffix = match &sync.outcome {
            SkillsOutcome::Pending => return None,
            SkillsOutcome::NotInstalled => String::new(),
            SkillsOutcome::Synced { .. } => " · agent skills synchronized".into(),
            SkillsOutcome::Preserved { count, .. } => {
                let plural = if *count == 1 { "" } else { "s" };
                format!(" · {count} agent skill{plural} preserved (local edits)")
            }
            SkillsOutcome::Failed { .. } => {
                " · agent skills sync incomplete (see `railway autoupdate status`)".into()
            }
        };
        self.last_notified_version = Some(running.into());
        Some(format!("✓ Railway updated to v{running}{suffix}"))
    }
}

/// Serialize short read-modify-write operations, never holding this lock over
/// network access or while acquiring a skills/binary install lock.
fn mutate<T>(home: &Path, change: impl FnOnce(&mut UpdateStatus) -> T) -> Result<T> {
    use fs2::FileExt;

    let dir = home.join(".railway");
    std::fs::create_dir_all(&dir)?;
    let lock = std::fs::File::create(dir.join("update-status.lock"))?;
    lock.lock_exclusive()?;
    let mut status = UpdateStatus::read(home);
    let before = serde_json::to_string(&status)?;
    let result = change(&mut status);
    let after = serde_json::to_string(&status)?;
    if before != after {
        super::write_atomic(&dir.join("update-status.json"), &after)?;
    }
    Ok(result)
}

pub fn observe_running(version: &str, previous: Option<&str>, managed_skills: bool) {
    if let Some(home) = dirs::home_dir() {
        let _ = mutate(&home, |status| {
            status.observe(version, previous);
            if !managed_skills {
                status.set_skills(version, SkillsOutcome::NotInstalled);
            }
        });
    }
}

pub fn record_installed(version: &str) {
    if let Some(home) = dirs::home_dir() {
        let _ = mutate(&home, |status| {
            status
                .last_seen_version
                .get_or_insert_with(|| env!("CARGO_PKG_VERSION").into());
            status.installed_version = Some(version.into());
            if status
                .skills
                .as_ref()
                .is_some_and(|sync| sync.cli_version != version)
            {
                status.skills = None;
            }
        });
    }
}

pub fn record_skills(version: &str, outcome: SkillsOutcome) {
    if let Some(home) = dirs::home_dir() {
        let _ = mutate(&home, |status| status.set_skills(version, outcome));
    }
}

pub fn mark_presented(version: &str) {
    if let Some(home) = dirs::home_dir() {
        let _ = mutate(&home, |status| {
            status.last_notified_version = Some(version.into())
        });
    }
}

/// Claim the notice under the same lock used to persist it: simultaneous CLI
/// invocations cannot both print it. Read-only/machine callers never claim it.
pub fn take_receipt() -> Option<String> {
    let home = dirs::home_dir()?;
    mutate(&home, |status| status.receipt(env!("CARGO_PKG_VERSION")))
        .ok()
        .flatten()
}

pub fn available_notice_due(version: &str) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    mutate(&home, |status| {
        if status.last_available_notice.as_deref() == Some(version) {
            return false;
        }
        status.last_available_notice = Some(version.into());
        true
    })
    .unwrap_or(false)
}

pub fn print_status() {
    println!("Running version: v{}", env!("CARGO_PKG_VERSION"));
    let Some(home) = dirs::home_dir() else { return };
    let status = UpdateStatus::read(&home);
    if let Some(version) = &status.installed_version {
        let activation =
            match super::compare_semver::compare_semver(version, env!("CARGO_PKG_VERSION")) {
                std::cmp::Ordering::Equal => "active",
                std::cmp::Ordering::Greater => "active on next command",
                std::cmp::Ordering::Less => "previously recorded; running version is newer",
            };
        println!("Installed version: v{version} ({activation})");
    }
    if let Some(sync) = status.skills {
        println!(
            "Skills for CLI v{}: {}",
            sync.cli_version,
            sync.outcome.summary()
        );
        let needs_details = matches!(
            sync.outcome,
            SkillsOutcome::Preserved { .. } | SkillsOutcome::Failed { .. }
        );
        match sync.outcome {
            SkillsOutcome::Synced { revision } | SkillsOutcome::Preserved { revision, .. } => {
                println!("Skills revision: {revision}");
            }
            SkillsOutcome::Failed { message } => println!("Skills sync error: {message}"),
            _ => {}
        }
        if needs_details {
            println!(
                "Skill details: `railway skills update` (use `--force` to overwrite local edits)."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_waits_for_activation_and_skills_and_is_consumed_once() {
        let home = tempfile::tempdir().unwrap();
        mutate(home.path(), |state| {
            state.observe("1.0.0", None);
            state.installed_version = Some("1.1.0".into());
            state.set_skills("1.1.0", SkillsOutcome::Pending);
        })
        .unwrap();
        assert!(
            mutate(home.path(), |state| state.receipt("1.0.0"))
                .unwrap()
                .is_none()
        );
        assert!(
            mutate(home.path(), |state| state.receipt("1.1.0"))
                .unwrap()
                .is_none()
        );
        mutate(home.path(), |state| {
            state.set_skills(
                "1.1.0",
                SkillsOutcome::Synced {
                    revision: "sha".into(),
                },
            )
        })
        .unwrap();
        let receipt = mutate(home.path(), |state| state.receipt("1.1.0"))
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt,
            "✓ Railway updated to v1.1.0 · agent skills synchronized"
        );
        assert!(
            mutate(home.path(), |state| state.receipt("1.1.0"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn external_updates_are_detected_without_announcing_fresh_installs() {
        let mut state = UpdateStatus::default();
        state.observe("1.0.0", None);
        state.set_skills("1.0.0", SkillsOutcome::NotInstalled);
        assert!(state.receipt("1.0.0").is_none());
        state.observe("1.1.0", None);
        state.set_skills("1.1.0", SkillsOutcome::NotInstalled);
        assert_eq!(
            state.receipt("1.1.0").as_deref(),
            Some("✓ Railway updated to v1.1.0")
        );
    }

    #[test]
    fn stale_skill_workers_cannot_replace_newer_results() {
        let mut state = UpdateStatus {
            installed_version: Some("1.1.0".into()),
            ..Default::default()
        };
        state.set_skills(
            "1.1.0",
            SkillsOutcome::Preserved {
                revision: "new".into(),
                count: 2,
            },
        );
        state.set_skills(
            "1.0.0",
            SkillsOutcome::Synced {
                revision: "old".into(),
            },
        );
        assert!(
            state
                .receipt("1.1.0")
                .unwrap()
                .contains("2 agent skills preserved")
        );
    }

    #[test]
    fn concurrent_commands_claim_a_single_completion_receipt() {
        let home = tempfile::tempdir().unwrap();
        mutate(home.path(), |state| {
            state.installed_version = Some("1.1.0".into());
            state.set_skills("1.1.0", SkillsOutcome::NotInstalled);
        })
        .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let home = home.path().to_path_buf();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    mutate(&home, |state| state.receipt("1.1.0")).unwrap()
                })
            })
            .collect();
        let receipts = tasks
            .into_iter()
            .filter_map(|task| task.join().unwrap())
            .count();
        assert_eq!(receipts, 1);
    }
}
