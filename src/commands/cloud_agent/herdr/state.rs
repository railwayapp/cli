//! `state.json` in the plugin dir: which herdr profile belongs to which agent.
//! A cache only; `sync` rebuilds it from `railway ca list` and `herdr machine list`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
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
}

impl State {
    /// One file per herdr session: machine profiles and toggles belong to the
    /// client that owns them, and a named test session must not overwrite
    /// the default session's memory.
    pub fn path() -> Result<PathBuf> {
        Ok(super::plugin_dir()?.join(file_name(std::env::var_os("HERDR_SOCKET_PATH").as_deref())))
    }

    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("Unreadable {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("Reading {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path()?)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("Writing {}", path.display()))
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
}
