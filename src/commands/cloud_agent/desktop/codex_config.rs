//! Codex Desktop's declarative SSH/project import, verified in 26.901.51231 (8109).
//! The app reads $CODEX_HOME/codex-app/config.json at startup and owns the
//! resulting global-state writes. Setup only saves configuration, never launches
//! or activates Desktop.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

pub(super) const APPLY_URL: &str = "codex://codex-app/apply-config";

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AppConfig {
    #[serde(default = "version")]
    version: u64,
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    remote_connection_max_retry_attempts: Option<u64>,
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    ssh_connect_timeout_seconds: Option<u64>,
    #[serde(default)]
    remote_connections: Vec<RemoteConnection>,
}

fn version() -> u64 {
    1
}

// The app's optional fields permit omission, but not JSON null.
fn optional<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteConnection {
    ssh_alias: String,
    #[serde(default)]
    projects: Vec<Project>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Project {
    remote_path: String,
    #[serde(
        default,
        deserialize_with = "optional",
        skip_serializing_if = "Option::is_none"
    )]
    label: Option<String>,
}

pub(super) fn config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Unable to locate Codex Desktop's home directory")?;
    path_at(&home, std::env::var_os("CODEX_HOME").as_deref())
}

fn path_at(home: &Path, override_home: Option<&std::ffi::OsStr>) -> Result<PathBuf> {
    let root = match override_home.filter(|value| !value.is_empty()) {
        Some(value) => crate::commands::ssh::config::expand_tilde(Path::new(value))?,
        None => home.join(".codex"),
    };
    Ok(std::path::absolute(root)?.join("codex-app/config.json"))
}

fn read(path: &Path) -> Result<AppConfig> {
    let config: AppConfig = match fs::read(path) {
        Ok(bytes) => (|| -> Result<AppConfig> {
            let object: serde_json::Map<String, serde_json::Value> =
                serde_json::from_slice(&bytes)?;
            Ok(serde_json::from_value(serde_json::Value::Object(object))?)
        })()
        .with_context(|| {
            format!(
                "Unsupported or invalid Codex Desktop config in {}",
                path.display()
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppConfig {
            version: 1,
            ..Default::default()
        },
        Err(error) => return Err(error).with_context(|| format!("Reading {}", path.display())),
    };
    if config.version != 1 {
        bail!(
            "Unsupported Codex Desktop config version {} in {}",
            config.version,
            path.display()
        );
    }
    for connection in &config.remote_connections {
        if connection.ssh_alias.trim().is_empty()
            || connection
                .projects
                .iter()
                .any(|project| project.remote_path.trim().is_empty())
        {
            bail!(
                "Codex Desktop config contains an empty SSH alias or project path in {}",
                path.display()
            );
        }
    }
    Ok(config)
}

impl AppConfig {
    fn upsert(&mut self, alias: &str, name: &str, directory: &str) -> Result<()> {
        if alias.trim().is_empty() || directory.trim().is_empty() {
            bail!("Codex Desktop requires an SSH alias and remote project directory");
        }
        // Codex trims paths and normalizes trailing separators when matching projects.
        let directory = normalized_path(directory);
        for connection in &mut self.remote_connections {
            if connection.ssh_alias.trim() != alias {
                continue;
            }
            if let Some(project) = connection
                .projects
                .iter_mut()
                .find(|p| normalized_path(&p.remote_path) == directory)
            {
                // Respect an existing user label for this host/path.
                project
                    .label
                    .get_or_insert_with(|| format!("Railway: {name}"));
                return Ok(());
            }
        }
        let project = Project {
            remote_path: directory,
            label: Some(format!("Railway: {name}")),
        };
        if let Some(connection) = self
            .remote_connections
            .iter_mut()
            .find(|c| c.ssh_alias.trim() == alias)
        {
            connection.projects.push(project);
        } else {
            self.remote_connections.push(RemoteConnection {
                ssh_alias: alias.into(),
                projects: vec![project],
            });
        }
        Ok(())
    }
}

fn normalized_path(path: &str) -> String {
    let path = path.trim().replace('\\', "/");
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        trimmed.into()
    }
}

pub(super) fn preflight() -> Result<()> {
    read(&config_path()?)?;
    Ok(())
}

pub(super) fn preview(alias: &str, name: &str, directory: &str) -> Result<()> {
    let path = config_path()?;
    let mut config = read(&path)?;
    config.upsert(alias, name, directory)?;
    println!(
        "\n{}\n{}",
        path.display(),
        serde_json::to_string_pretty(&config)?
    );
    println!("Codex Desktop will import the connection and project on its next startup.");
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("Missing config directory")?)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("Writing {}", path.display()))?;
    Ok(())
}

fn update(path: &Path, edit: impl FnOnce(&mut AppConfig) -> Result<()>) -> Result<bool> {
    let parent = path.parent().context("Missing Codex config directory")?;
    fs::create_dir_all(parent)?;
    // Serialize concurrent Railway setup calls; Codex reads this file but does not write it.
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(parent.join(".railway-config.lock"))?;
    lock.try_lock_exclusive()
        .context("Another Railway process is updating Codex Desktop; rerun setup")?;
    let mut config = read(path)?;
    edit(&mut config)?;
    let mut bytes = serde_json::to_vec_pretty(&config)?;
    bytes.push(b'\n');
    match fs::read(path) {
        Ok(previous) if previous == bytes => return Ok(false),
        Ok(previous) => write_private(
            &path.with_file_name("config.json.railway-backup"),
            &previous,
        )?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Reading previous Codex Desktop config"),
    }
    write_private(path, &bytes)?;
    Ok(true)
}

pub(super) async fn configure(
    alias: &str,
    name: &str,
    directory: &str,
    ssh_config: &Path,
) -> Result<super::CodexDesktop> {
    let path = config_path()?;
    let mut label = format!("Railway: {name}");
    update(&path, |config| {
        config.upsert(alias, name, directory)?;
        label = config
            .remote_connections
            .iter()
            .filter(|c| c.ssh_alias.trim() == alias)
            .flat_map(|c| &c.projects)
            .find(|p| normalized_path(&p.remote_path) == normalized_path(directory))
            .and_then(|p| p.label.clone())
            .unwrap_or_else(|| normalized_path(directory));
        Ok(())
    })?;
    // Codex imports this declaration at startup. Keep setup entirely in the
    // background, including when Desktop is already running. JSON stdout stays clean.
    eprintln!(
        "Saved Codex Desktop connection {alias} and project {} in {}",
        normalized_path(directory),
        path.display()
    );
    Ok(super::CodexDesktop {
        ssh_alias: alias.into(),
        ssh_config_path: ssh_config.into(),
        config_path: path,
        project_label: label,
        remote_path: normalized_path(directory),
        apply_url: APPLY_URL.into(),
        apply_sent: false,
        apply_error: None,
    })
}

pub(super) fn remove(alias: &str) -> Result<bool> {
    let path = config_path()?;
    remove_at(&path, alias)
}

fn remove_at(path: &Path, alias: &str) -> Result<bool> {
    if !read(path)?
        .remote_connections
        .iter()
        .any(|c| c.ssh_alias.trim() == alias)
    {
        return Ok(false);
    }
    update(path, |config| {
        config
            .remote_connections
            .retain(|c| c.ssh_alias.trim() != alias);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_preserves_other_hosts_projects_preferences_and_custom_labels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let original = json!({"version":1,"sshConnectTimeoutSeconds":45,"remoteConnectionMaxRetryAttempts":0,
            "remoteConnections":[{"sshAlias":"personal","projects":[{"remotePath":"/work","label":"Personal"}]},
                {"sshAlias":"railway-box","projects":[{"remotePath":"/app/","label":"My label"},{"remotePath":"/other"}]}]});
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        update(&path, |c| c.upsert("railway-box", "box", "/app")).unwrap();
        update(&path, |c| {
            c.upsert("railway-box", "box", "/app/new project")
        })
        .unwrap();
        let before = fs::read(&path).unwrap();
        assert!(
            !update(&path, |c| c.upsert(
                "railway-box",
                "box",
                "/app/new project/"
            ))
            .unwrap()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        let current: serde_json::Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(current["sshConnectTimeoutSeconds"], 45);
        assert_eq!(current["remoteConnectionMaxRetryAttempts"], 0);
        assert_eq!(
            current["remoteConnections"][0],
            original["remoteConnections"][0]
        );
        assert_eq!(
            current["remoteConnections"][1]["projects"][0]["label"],
            "My label"
        );
        assert_eq!(
            current["remoteConnections"][1]["projects"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            current["remoteConnections"][1]["projects"][2],
            json!({"remotePath":"/app/new project","label":"Railway: box"})
        );
    }

    #[test]
    fn invalid_or_future_config_is_never_overwritten() {
        for text in [
            "{",
            "[]",
            "null",
            "",
            r#"{"version":2}"#,
            r#"{"version":1,"futureOption":true}"#,
            r#"{"remoteConnections":{}}"#,
            r#"{"sshConnectTimeoutSeconds":-1}"#,
            r#"{"sshConnectTimeoutSeconds":null}"#,
            r#"{"remoteConnections":[{"sshAlias":"","projects":[]}]}"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.json");
            fs::write(&path, text).unwrap();
            assert!(
                update(&path, |c| c.upsert("railway-box", "box", "/app")).is_err(),
                "{text}"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
        }
    }

    #[test]
    fn new_config_uses_v1_and_backs_up_before_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex-app/config.json");
        update(&path, |c| c.upsert("custom-alias", "box", "/app")).unwrap();
        let first = fs::read(&path).unwrap();
        assert_eq!(read(&path).unwrap().version, 1);
        update(&path, |c| c.upsert("second", "other", "/")).unwrap();
        assert_eq!(
            fs::read(path.with_file_name("config.json.railway-backup")).unwrap(),
            first
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn codex_home_override_and_default_resolve_separately() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            path_at(home.path(), None).unwrap(),
            home.path().join(".codex/codex-app/config.json")
        );
        let custom = home.path().join("custom home");
        assert_eq!(
            path_at(home.path(), Some(custom.as_os_str())).unwrap(),
            custom.join("codex-app/config.json")
        );
        assert_eq!(
            path_at(home.path(), Some(std::ffi::OsStr::new(""))).unwrap(),
            path_at(home.path(), None).unwrap()
        );
    }

    #[test]
    fn removal_is_scoped_to_the_registered_alias_and_absence_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex-app/config.json");
        assert!(!remove_at(&path, "custom-alias").unwrap());
        assert!(!path.parent().unwrap().exists());
        update(&path, |c| c.upsert("personal", "personal", "/work")).unwrap();
        update(&path, |c| c.upsert("custom-alias", "box", "/app")).unwrap();
        assert!(remove_at(&path, "custom-alias").unwrap());
        let config = read(&path).unwrap();
        assert_eq!(config.remote_connections.len(), 1);
        assert_eq!(config.remote_connections[0].ssh_alias, "personal");
        assert!(!remove_at(&path, "custom-alias").unwrap());
    }

    #[test]
    fn concurrent_writer_is_reported_before_any_config_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        update(&path, |c| c.upsert("personal", "personal", "/work")).unwrap();
        let before = fs::read(&path).unwrap();
        let lock = fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join(".railway-config.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        assert!(update(&path, |c| c.upsert("railway-box", "box", "/app")).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}
