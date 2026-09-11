//! Display-only conversation snapshots. Reading these never contacts a VM.
use super::app::ConsoleSession;
use crate::commands::cloud_agent::client_sessions;
use sha2::{Digest, Sha256};
use std::{io::Write, path::PathBuf};

pub(super) struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn open(backboard: &str) -> Option<Self> {
        Some(Self {
            root: dirs::cache_dir()?
                .join("railway/conversations-v1")
                .join(format!("{:x}", Sha256::digest(backboard.as_bytes()))),
        })
    }
    fn path(&self, environment: &str, agent: &str) -> PathBuf {
        self.root.join(format!(
            "{:x}.json",
            Sha256::digest(format!("{environment}:{agent}"))
        ))
    }
    pub fn read(&self, environment: &str, agent: &str) -> Option<Vec<ConsoleSession>> {
        let rows =
            serde_json::from_slice(&std::fs::read(self.path(environment, agent)).ok()?).ok()?;
        Some(snapshot(agent, rows))
    }
    pub fn save(
        &self,
        environment: &str,
        agent: &str,
        rows: &[ConsoleSession],
    ) -> anyhow::Result<()> {
        let data = serde_json::to_vec(&snapshot(agent, rows.to_vec()))?;
        let path = self.path(environment, agent);
        if std::fs::read(&path).ok().as_deref() == Some(&data) {
            return Ok(());
        }
        std::fs::create_dir_all(&self.root)?;
        let mut file = tempfile::NamedTempFile::new_in(&self.root)?;
        file.write_all(&data)?;
        file.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}
pub(super) fn snapshot(agent: &str, rows: Vec<ConsoleSession>) -> Vec<ConsoleSession> {
    rows.into_iter()
        .filter(|row| {
            row.kind == "THREAD"
                && client_sessions::parse_name(&row.name)
                    .is_some_and(|(_, id, thread)| id == agent && thread.is_some())
        })
        .map(|mut row| {
            row.attached = false;
            if let Some(snapshot) = &mut row.snapshot {
                snapshot.state = "idle".into();
                snapshot.last_reply = None;
            }
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_preserves_titles_but_not_shells_drafts_or_live_status() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache {
            root: dir.path().into(),
        };
        let thread = crate::commands::cloud_agent::remote_threads::tests::thread("claude", "saved");
        let mut saved = ConsoleSession::client_thread("vm", "claude", Some(&thread.thread));
        let title = saved.short_name();
        saved.snapshot.as_mut().unwrap().state = "working".into();
        let draft = ConsoleSession::client_thread("vm", "claude", None);
        let mut shell = draft.clone();
        shell.kind = "SHELL".into();
        shell.name = "real-shell".into();
        cache.save("env", "vm", &[saved, draft, shell]).unwrap();
        let cache = Cache {
            root: dir.path().into(),
        };
        let rows = cache.read("env", "vm").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].short_name(), title);
        assert_eq!(rows[0].snapshot.as_ref().unwrap().state, "idle");
        assert!(cache.read("other-env", "vm").is_none());
        assert!(cache.read("env", "other-vm").is_none());
    }
}
