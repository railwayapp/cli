//! Readable defaults for newly created Codex and OpenCode agents.
use std::{collections::HashSet, path::Path};

use anyhow::{Result, bail};
use rand::Rng;

use super::{Agent, LaunchArgs};
use crate::{
    config::Configs,
    controllers::{cloud_agent, project::get_project},
};

pub(super) struct Target {
    pub project_id: String,
    pub environment_id: String,
    pub use_local_name: bool,
}

impl Target {
    pub fn new((project_id, environment_id): (String, String), use_local_name: bool) -> Self {
        Self {
            project_id,
            environment_id,
            use_local_name,
        }
    }

    pub fn local_name_project(&self) -> Option<String> {
        self.use_local_name.then(|| self.project_id.clone())
    }
}

fn prefix(agent: Agent) -> Option<&'static str> {
    match agent {
        Agent::Codex => Some("codex"),
        Agent::OpenCode => Some("oc"),
        Agent::OpenCode2 => Some("oc2"),
        _ => None,
    }
}

/// Called only after reuse/wake has been ruled out. An explicit name and other
/// harnesses keep their existing behavior without additional lookups.
pub(super) async fn for_launch(
    client: &reqwest::Client,
    configs: &Configs,
    args: &LaunchArgs,
    agent: Agent,
    target: &Target,
) -> Result<Option<String>> {
    if let Some(name) = &args.name {
        return Ok(Some(name.clone()));
    }
    let Some(prefix) = prefix(agent) else {
        return Ok(None);
    };
    let label = if target.use_local_name {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| local_project_name(&cwd))
    } else {
        None
    };
    let label = match label {
        Some(label) => label,
        None => {
            get_project(client, configs, target.project_id.clone())
                .await?
                .name
        }
    };
    // Names can be used account-wide by `connect <name>`. Check all of this
    // user's agents, including sleeping ones and those in other projects.
    // This is not an atomic reservation: concurrent creates can still collide,
    // so connections continue to resolve IDs and reject ambiguous names.
    let existing = cloud_agent::list_mine(client, &configs.get_backboard())
        .await?
        .into_iter()
        .map(|agent| agent.name.to_ascii_lowercase())
        .collect();
    let start = rand::thread_rng().gen_range(0..SUFFIX_COUNT);
    available_name(prefix, &label, &existing, start).map(Some)
}

fn local_project_name(cwd: &Path) -> Option<String> {
    // A .git file also marks a worktree/submodule root. No remote URL, Git
    // process, or local Git configuration needs to be read for this label.
    let root = cwd
        .ancestors()
        .find(|path| path.join(".git").exists())
        .unwrap_or(cwd);
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn fragment(label: &str) -> String {
    let value: String = label
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(5)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if value.is_empty() {
        "proj".into()
    } else {
        value
    }
}

const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const SUFFIX_COUNT: usize = 36 * 36 * 36;

fn available_name(
    prefix: &str,
    label: &str,
    existing: &HashSet<String>,
    start: usize,
) -> Result<String> {
    let fragment = fragment(label);
    // Start randomly, then walk the suffix space on a collision. This always
    // finds a free name when one exists and has a bounded exhaustion case.
    for offset in 0..SUFFIX_COUNT {
        let n = (start + offset) % SUFFIX_COUNT;
        let suffix: String = [
            ALPHABET[n / (36 * 36)],
            ALPHABET[n / 36 % 36],
            ALPHABET[n % 36],
        ]
        .into_iter()
        .map(char::from)
        .collect();
        let name = format!("{prefix}-{fragment}-{suffix}");
        if !existing.contains(&name) {
            return Ok(name);
        }
    }
    bail!("All generated names for {prefix}-{fragment} are in use. Choose a name with --name.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editions_and_fragments_follow_the_lowercase_format() {
        assert_eq!(prefix(Agent::OpenCode), Some("oc"));
        assert_eq!(prefix(Agent::OpenCode2), Some("oc2"));
        assert_eq!(prefix(Agent::Claude), None);
        assert_eq!(prefix(Agent::Codex), Some("codex"));
        for agent in [Agent::Codex, Agent::OpenCode, Agent::OpenCode2] {
            let prefix = prefix(agent).unwrap();
            assert_eq!(
                available_name(prefix, "Railgun", &HashSet::new(), 4405).unwrap(),
                format!("{prefix}-railg-3ed")
            );
        }
        for (label, expected) in [
            ("Rail Gun!", "railg"),
            ("My-API_2", "myapi"),
            ("API", "api"),
            ("🚂日本語", "proj"),
            ("a\nB\tC", "abc"),
        ] {
            assert_eq!(fragment(label), expected);
        }
    }

    #[test]
    fn collisions_wrap_without_changing_the_project_or_edition() {
        for prefix in ["codex", "oc", "oc2"] {
            let existing =
                HashSet::from([format!("{prefix}-railg-zzz"), format!("{prefix}-railg-000")]);
            assert_eq!(
                available_name(prefix, "Railgun", &existing, SUFFIX_COUNT - 1).unwrap(),
                format!("{prefix}-railg-001")
            );
            assert_eq!(
                available_name("other", "Railgun", &existing, SUFFIX_COUNT - 1).unwrap(),
                "other-railg-zzz"
            );
        }
    }

    #[test]
    fn directory_fallback_uses_the_repository_root_including_worktrees() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("Railgun");
        let nested = repo.join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(local_project_name(&nested).as_deref(), Some("deep"));
        std::fs::write(repo.join(".git"), "gitdir: unused").unwrap();
        assert_eq!(local_project_name(&nested).as_deref(), Some("Railgun"));
        std::fs::remove_file(repo.join(".git")).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        assert_eq!(local_project_name(&nested).as_deref(), Some("Railgun"));
    }

    #[tokio::test]
    async fn retargeting_preserves_directory_naming_only_for_the_original_project() {
        let temp = tempfile::tempdir().unwrap();
        let mut configs = Configs::for_test(temp.path().join("config.json"));
        let client = reqwest::Client::new();
        let original_project = "11111111-1111-1111-1111-111111111111";
        let environment = "33333333-3333-3333-3333-333333333333";
        let base = LaunchArgs {
            local_name_project: Some(original_project.into()),
            ..Default::default()
        };
        for project in [original_project, "22222222-2222-2222-2222-222222222222"] {
            let args = base.clone().retargeted(
                project.into(),
                environment.into(),
                "opencode2",
                true,
                None,
                None,
            );
            let target = super::super::resolve_target(
                &mut configs,
                &client,
                &args,
                &mut super::super::AgentPrefs::default(),
                temp.path(),
            )
            .await
            .unwrap();
            assert_eq!(target.project_id, project);
            assert_eq!(target.environment_id, environment);
            assert_eq!(target.use_local_name, project == original_project);
            assert_eq!(
                target.local_name_project().as_deref(),
                (project == original_project).then_some(project)
            );
        }
    }

    #[tokio::test]
    async fn explicit_names_and_other_harnesses_need_no_network() {
        let temp = tempfile::tempdir().unwrap();
        let configs = Configs::for_test(temp.path().join("config.json"));
        let client = reqwest::Client::new();
        let target = Target::new(("unused".into(), "unused".into()), false);
        let args = LaunchArgs {
            name: Some("My-Name".into()),
            ..Default::default()
        };
        for agent in [Agent::Codex, Agent::OpenCode, Agent::OpenCode2] {
            assert_eq!(
                for_launch(&client, &configs, &args, agent, &target)
                    .await
                    .unwrap()
                    .as_deref(),
                Some("My-Name")
            );
        }
        assert!(
            for_launch(
                &client,
                &configs,
                &LaunchArgs::default(),
                Agent::Claude,
                &target
            )
            .await
            .unwrap()
            .is_none()
        );
    }
}
