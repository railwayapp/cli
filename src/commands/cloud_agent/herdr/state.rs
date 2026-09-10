//! Plugin bookkeeping: cached profile matches, acknowledged transitions and
//! incomplete bootstrap steps. Inventory comes from Railway and Herdr on sync.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Configs;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    revision: u64,
    /// agent id → herdr profile id
    #[serde(default)]
    pub machines: BTreeMap<String, String>,
    #[serde(default)]
    pub last_sync: Option<String>,
    /// project id → name, so the picker skips the workspace tree query
    /// (~2 s for a couple of hundred projects) when it already knows them all.
    #[serde(default)]
    pub project_names: BTreeMap<String, String>,
    /// agent id → status label at the last sync, so a wake done elsewhere is
    /// noticed and its machine reconnected.
    #[serde(default)]
    pub agent_status: BTreeMap<String, String>,
    /// An acknowledged sleep must not be undone by a lagging RUNNING read.
    /// Cleared on a sleeping observation, an explicit wake, or the bounded
    /// transition deadline (a wake elsewhere may have overtaken the sleep).
    #[serde(default)]
    pub sleep_until: BTreeMap<String, DateTime<Utc>>,
    /// A saved machine is not proof that its Railway bootstrap completed.
    #[serde(default)]
    pub bootstrap_pending: BTreeMap<String, String>,
}

impl State {
    /// One file per watcher session, so a named session's sync observations
    /// do not overwrite the default session's memory.
    pub fn path() -> Result<PathBuf> {
        Ok(super::plugin_dir()?.join(file_name(std::env::var_os("HERDR_SOCKET_PATH").as_deref())))
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("Unreadable {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("Reading {}", path.display())),
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::util::write_atomic(path, &serde_json::to_string_pretty(self)?)
            .with_context(|| format!("Writing {}", path.display()))
    }

    fn bind_scope(&mut self, scope: &str) {
        if self.scope.as_deref() != Some(scope) {
            *self = Self {
                scope: Some(scope.to_owned()),
                revision: self.revision,
                ..Default::default()
            };
        }
    }

    pub fn same_revision(&self, other: &Self) -> bool {
        self.scope == other.scope && self.revision == other.revision
    }

    pub fn sleep_pending(&self, id: &str, now: DateTime<Utc>) -> bool {
        self.sleep_until.get(id).is_some_and(|until| *until > now)
    }
}

/// Capture the same account/backend as the request client, including token
/// overrides. Raw credentials never go into the state file. A token rotation
/// without a stable user ID conservatively discards the old pruning evidence.
#[derive(Clone)]
pub(super) struct Store {
    pub path: PathBuf,
    scope: String,
}

impl Store {
    pub fn new(configs: &Configs) -> Result<Self> {
        let principal = if let Some(token) = Configs::get_railway_token() {
            format!("project:{token}")
        } else if let Some(token) = Configs::get_railway_api_token() {
            format!("api:{token}")
        } else if let Some(id) = &configs.root_config.user.id {
            format!("user:{id}")
        } else {
            format!(
                "session:{}",
                configs.get_railway_auth_token().unwrap_or_default()
            )
        };
        Ok(Self::at(
            State::path()?,
            &configs.get_backboard(),
            &principal,
        ))
    }

    pub(super) fn at(path: PathBuf, backboard: &str, principal: &str) -> Self {
        let encoded = serde_json::to_vec(&(backboard, principal)).expect("strings serialize");
        Self {
            path,
            scope: format!("{:x}", Sha256::digest(encoded)),
        }
    }

    pub fn load(&self) -> Result<State> {
        let mut state = State::load_from(&self.path)?;
        state.bind_scope(&self.scope);
        Ok(state)
    }

    pub async fn lock(&self) -> Result<LockedState> {
        let file = lock_file(&self.path.with_extension("lock")).await?;
        Ok(LockedState {
            state: self.load()?,
            path: self.path.clone(),
            _file: file,
        })
    }

    pub async fn update(&self, change: impl FnOnce(&mut State)) -> Result<()> {
        let mut locked = self.lock().await?;
        change(&mut locked.state);
        locked.save()
    }
}

pub(super) struct LockedState {
    pub state: State,
    path: PathBuf,
    _file: File,
}

impl LockedState {
    pub fn save(&mut self) -> Result<()> {
        self.state.revision = self.state.revision.wrapping_add(1);
        self.state.save_to(&self.path)
    }
}

/// A sibling lock survives atomic replacement of the state file. Waiting is
/// asynchronous, and closing the file releases the lock on every error path.
pub(super) async fn lock_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(e) if e.kind() == fs2::lock_contended_error().kind() => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(e) => return Err(e).context("Locking herdr state"),
        }
    }
}

fn file_name(socket: Option<&std::ffi::OsStr>) -> String {
    let session = socket
        .map(Path::new)
        .and_then(|p| p.parent())
        .filter(|dir| dir.parent().and_then(|d| d.file_name()) == Some("sessions".as_ref()))
        .and_then(|dir| dir.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    match session {
        Some(name) => format!("state-{name}.json"),
        None => "state.json".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_sessions_get_their_own_state_file() {
        assert_eq!(file_name(None), "state.json");
        assert_eq!(
            file_name(Some("/Users/me/.config/herdr/herdr.sock".as_ref())),
            "state.json"
        );
        assert_eq!(
            file_name(Some("/x/herdr/sessions/rca/herdr.sock".as_ref())),
            "state-rca.json"
        );
    }

    #[test]
    fn missing_file_is_empty_state_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("state.json");
        assert_eq!(State::load_from(&path).unwrap(), State::default());
        let mut s = State::default();
        s.machines.insert("agent-1".into(), "profile-1".into());
        s.save_to(&path).unwrap();
        assert_eq!(State::load_from(&path).unwrap(), s);
    }

    #[tokio::test]
    async fn account_and_backend_changes_discard_pruning_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let a = Store::at(path.clone(), "https://production", "account-a");
        a.update(|s| {
            s.machines.insert("agent-a".into(), "profile-a".into());
        })
        .await
        .unwrap();
        assert_eq!(a.load().unwrap().machines.len(), 1);
        for (host, account) in [
            ("https://production", "account-b"),
            ("https://staging", "account-a"),
        ] {
            let other = Store::at(path.clone(), host, account);
            assert!(other.load().unwrap().machines.is_empty());
        }
        assert!(!std::fs::read_to_string(path).unwrap().contains("account-a"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_updates_preserve_each_registration_and_invalidate_old_plans() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("state.json"), "backboard", "account");
        let before = store.load().unwrap();
        let start = std::sync::Arc::new(tokio::sync::Barrier::new(20));
        let writes = (0..20).map(|n| {
            let store = store.clone();
            let start = start.clone();
            tokio::spawn(async move {
                start.wait().await;
                store
                    .update(|s| {
                        s.machines
                            .insert(format!("agent-{n}"), format!("profile-{n}"));
                    })
                    .await
                    .unwrap();
            })
        });
        for result in futures::future::join_all(writes).await {
            result.unwrap();
        }
        let after = store.load().unwrap();
        assert_eq!(after.machines.len(), 20);
        assert!(!after.same_revision(&before));
    }
}
