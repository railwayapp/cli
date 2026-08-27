//! Cloud agent launch preferences — `~/.railway/agent-prefs.json`.
//!
//! Deliberately its own file rather than a field on `RailwayConfig`:
//! `~/.railway/config.json` forks per Railway environment
//! (`config-staging.json`, `config-dev.json`), and "which coding agent do I
//! use" is a property of the person, not of the environment they happen to be
//! linked to. A single file also survives `railway logout`, which is the right
//! call — logging out drops credentials, not preferences.
//!
//! Nothing in here is secret, but the file lives next to the token store, so
//! it is written 0600 for consistency with its neighbours.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Bumped only when a change needs a migration. A file from a newer CLI is
/// still parsed on a best-effort basis (every field has a serde default), so
/// downgrading degrades to "what this version understands" instead of wedging
/// a launch on a config error.
pub const CURRENT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentPrefs {
    pub version: u32,

    /// Harness slug — `claude`, `codex`, or `grok`, matching the remote binary
    /// names the launcher uses. `None` means "not configured": the launcher
    /// asks, rather than guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,

    #[serde(default)]
    pub skills: SkillsPrefs,

    #[serde(default)]
    pub mcp: McpPrefs,

    /// Where cloud agents go unless told otherwise. Chosen in setup, and the
    /// target a new agent is created in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_project: Option<DefaultProject>,

    /// TUI colour theme slug. `None` means the default; an unknown value is
    /// ignored rather than treated as an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,

    /// Hide the maximized layout's header tabs — ⌥⇧[ / ⌥⇧] stay the way
    /// between sessions. A settings-card choice; the wizard never asks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hide_tabs: bool,
}

impl Default for AgentPrefs {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            agent: None,
            skills: SkillsPrefs::default(),
            mcp: McpPrefs::default(),
            default_project: None,
            theme: None,
            hide_tabs: false,
        }
    }
}

/// Project MCP import — the launch directory's `.mcp.json`, carried onto the
/// agent. See [`super::mcp_sync`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpPrefs {
    /// On by default, unlike skills: the source is a file the project chose
    /// to commit, so bringing it is what launching from that directory means.
    /// `"mcp": {"enabled": false}` opts out.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Server names never shipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

impl Default for McpPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            exclude: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// The project and environment new agents are created in by default.
///
/// Names are stored alongside the ids so the TUI can label the target before
/// any network call — an id alone would show as a uuid on the first frame.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DefaultProject {
    pub project_id: String,
    pub project_name: String,
    pub environment_id: String,
    pub environment_name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SkillsPrefs {
    /// Sync this machine's personal skills to the agent on launch.
    #[serde(default)]
    pub enabled: bool,

    /// Which local directory the skills are read from — a
    /// [`super::skills_sync::SkillSource`] slug. Recorded at setup time rather
    /// than re-guessed per launch: a machine can have several skills
    /// directories with different contents, and silently switching between
    /// them would change what the agent can do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,

    /// Skill directory names that are never synced. Seeded with the skills the
    /// CLI installed itself — Railway's own ship inside the agent image, and
    /// pushing a local copy over them would let an older checkout win.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

impl AgentPrefs {
    pub fn path_in(home: &Path) -> PathBuf {
        home.join(".railway").join("agent-prefs.json")
    }

    /// Reads the prefs, treating missing OR unparseable as "not configured".
    /// A hand-mangled prefs file must not be able to break `railway ca` — the
    /// worst it can do is send the user back through setup.
    pub fn load_in(home: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(Self::path_in(home)).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn save_in(&self, home: &Path) -> Result<()> {
        let path = Self::path_in(home);
        let contents = serde_json::to_string_pretty(self)?;
        crate::util::write_atomic(&path, &contents)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        restrict_permissions(&path);
        Ok(())
    }
}

/// 0600 on unix; a no-op elsewhere. Best-effort — a preferences file that
/// could not be tightened is still a usable preferences file.
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let home = tempfile::tempdir().unwrap();
        let prefs = AgentPrefs {
            version: CURRENT_VERSION,
            agent: Some("claude".into()),
            skills: SkillsPrefs {
                enabled: true,
                source: Some("claude".into()),
                exclude: vec!["use-railway".into()],
            },
            mcp: McpPrefs {
                enabled: false,
                exclude: vec!["notion".into()],
            },
            default_project: None,
            theme: Some("ember".into()),
            hide_tabs: true,
        };
        prefs.save_in(home.path()).unwrap();
        assert_eq!(AgentPrefs::load_in(home.path()).unwrap(), prefs);
    }

    #[test]
    fn missing_file_is_not_configured() {
        let home = tempfile::tempdir().unwrap();
        assert!(AgentPrefs::load_in(home.path()).is_none());
    }

    #[test]
    fn corrupt_file_degrades_to_not_configured() {
        let home = tempfile::tempdir().unwrap();
        let path = AgentPrefs::path_in(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(AgentPrefs::load_in(home.path()).is_none());
    }

    /// A file written by a newer CLI must still yield the fields this version
    /// knows about — never an error that blocks a launch.
    #[test]
    fn unknown_fields_and_newer_version_still_parse() {
        let home = tempfile::tempdir().unwrap();
        let path = AgentPrefs::path_in(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"version": 99, "agent": "codex", "somethingNew": {"a": 1}}"#,
        )
        .unwrap();
        let prefs = AgentPrefs::load_in(home.path()).unwrap();
        assert_eq!(prefs.agent.as_deref(), Some("codex"));
        assert!(!prefs.skills.enabled);
    }

    /// A prefs file written before the MCP field existed imports by default —
    /// the source is the project's own committed config, and requiring
    /// everyone to re-run setup to get it would make the feature invisible.
    #[test]
    fn mcp_import_defaults_on_for_older_prefs_files() {
        let home = tempfile::tempdir().unwrap();
        let path = AgentPrefs::path_in(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"version": 1, "agent": "claude"}"#).unwrap();
        let prefs = AgentPrefs::load_in(home.path()).unwrap();
        assert!(prefs.mcp.enabled);

        // And the opt-out parses.
        std::fs::write(
            &path,
            r#"{"version": 1, "mcp": {"enabled": false, "exclude": ["notion"]}}"#,
        )
        .unwrap();
        let prefs = AgentPrefs::load_in(home.path()).unwrap();
        assert!(!prefs.mcp.enabled);
        assert_eq!(prefs.mcp.exclude, vec!["notion".to_string()]);
    }

    /// The launcher reads `agent` as a harness slug; setup must never write a
    /// display name into it.
    #[test]
    fn agent_field_is_serialized_as_a_slug() {
        let prefs = AgentPrefs {
            agent: Some("grok".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&prefs).unwrap();
        assert!(json.contains(r#""agent":"grok""#), "{json}");
    }
}
