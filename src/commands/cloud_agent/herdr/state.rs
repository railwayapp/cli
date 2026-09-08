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
}

impl State {
    pub fn path() -> Result<PathBuf> {
        Ok(super::plugin_dir()?.join("state.json"))
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

#[cfg(test)]
mod tests {
    use super::*;

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
