//! OpenCode Desktop's Electron stores (verified against v1.18.29).
//! `server` is a JSON string inside opencode.global.dat, while the default
//! URL lives in opencode.settings. Keep unknown keys and existing connections.
//! https://github.com/anomalyco/opencode/blob/v1.18.29/packages/app/src/context/server.tsx
//! https://github.com/anomalyco/opencode/blob/v1.18.29/packages/desktop/src/main/store.ts

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use super::opencode::Connection;

const SETTINGS: &str = "opencode.settings";
const GLOBAL: &str = "opencode.global.dat";
// Ownership metadata is separate from OpenCode's stores and has no secrets.
const MANAGED: &str = "railway.servers.json";

pub(super) fn config_dir() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Unable to locate OpenCode Desktop's configuration directory")?
        .join("ai.opencode.desktop"))
}

fn read_object(path: &Path) -> Result<Map<String, Value>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("Invalid OpenCode Desktop settings in {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(error).with_context(|| format!("Reading {}", path.display())),
    }
}

fn object<'a>(parent: &'a mut Map<String, Value>, key: &str) -> Result<&'a mut Map<String, Value>> {
    parent
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .with_context(|| format!("Unsupported OpenCode Desktop format: {key} must be an object"))
}

fn server_url(server: &Value) -> Option<&str> {
    server
        .as_str()
        .or_else(|| server.get("http").unwrap_or(server).get("url")?.as_str())
}

struct Stores {
    settings: Map<String, Value>,
    global: Map<String, Value>,
    server: Map<String, Value>,
    managed: Map<String, Value>,
}

impl Stores {
    fn read(root: &Path) -> Result<Self> {
        let global = read_object(&root.join(GLOBAL))?;
        let server = match global.get("server") {
            None => Map::new(),
            Some(Value::String(value)) => {
                serde_json::from_str(value).context("Invalid OpenCode Desktop server settings")?
            }
            _ => bail!("Unsupported OpenCode Desktop format: server must be a JSON string"),
        };
        Ok(Self {
            global,
            server,
            settings: read_object(&root.join(SETTINGS))?,
            managed: read_object(&root.join(MANAGED))?,
        })
    }

    fn list(&mut self) -> Result<&mut Vec<Value>> {
        self.server
            .entry("list")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("Unsupported OpenCode Desktop format: server list must be an array")
    }

    fn upsert(&mut self, connection: &Connection, agent_id: &str, agent_name: &str) -> Result<()> {
        let url = &connection.url;
        let entry = json!({
            "type": "http",
            "displayName": format!("Railway: {agent_name}"),
            "http": { "url": url, "username": connection.username, "password": connection.password }
        });
        let list = self.list()?;
        if let Some(existing) = list.iter_mut().find(|item| server_url(item) == Some(url)) {
            *existing = entry;
        } else {
            list.push(entry);
        }
        let projects = object(&mut self.server, "projects")?
            .entry(url.clone())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("Unsupported OpenCode Desktop project list")?;
        if let Some(project) = projects
            .iter_mut()
            .find(|project| project["worktree"] == connection.directory)
        {
            project
                .as_object_mut()
                .context("Unsupported OpenCode Desktop project")?
                .insert("expanded".into(), json!(true));
        } else {
            projects.insert(
                0,
                json!({ "worktree": connection.directory, "expanded": true }),
            );
        }
        object(&mut self.server, "lastProject")?.insert(url.clone(), json!(connection.directory));
        self.settings.insert("defaultServerUrl".into(), json!(url));
        self.managed.insert(agent_id.into(), json!(url));
        Ok(())
    }

    fn remove(&mut self, agent_id: &str) -> Result<bool> {
        let Some(value) = self.managed.remove(agent_id) else {
            return Ok(false);
        };
        let url = value
            .as_str()
            .context("Invalid Railway OpenCode server record")?;
        self.list()?
            .retain(|server| server_url(server) != Some(url));
        for key in ["projects", "lastProject", "recentlyClosed"] {
            if self.server.contains_key(key) {
                object(&mut self.server, key)?.remove(url);
            }
        }
        if self
            .settings
            .get("defaultServerUrl")
            .and_then(Value::as_str)
            == Some(url)
        {
            self.settings.remove("defaultServerUrl");
        }
        Ok(true)
    }

    fn save(mut self, root: &Path) -> Result<()> {
        self.global.insert(
            "server".into(),
            Value::String(serde_json::to_string(&self.server)?),
        );
        // Validate/serialize everything before touching any store. Write the
        // connection before making it the default so a partial write is usable.
        let files = [
            (GLOBAL, serde_json::to_vec_pretty(&self.global)?),
            (SETTINGS, serde_json::to_vec_pretty(&self.settings)?),
            (MANAGED, serde_json::to_vec_pretty(&self.managed)?),
        ];
        fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Electron rewrites its stores with 0644 after launch. Keep the
            // containing directory private so saved passwords stay protected.
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
        for (name, contents) in files {
            let path = root.join(name);
            if let Ok(previous) = fs::read(&path) {
                if previous == contents {
                    continue;
                }
                write_private(&root.join(format!("{name}.railway-backup")), &previous)?;
            }
            write_private(&path, &contents)?;
        }
        Ok(())
    }
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    // NamedTempFile is created with 0600 on Unix, including while being written.
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().context("Missing settings directory")?)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("Writing {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn is_running(root: &Path) -> Result<bool> {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    // Chromium's singleton lock is a symlink named <hostname>-<pid>.
    let lock = match fs::read_link(root.join("SingletonLock")) {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("Reading OpenCode Desktop's application lock"),
    };
    let pid = lock
        .to_str()
        .and_then(|value| value.rsplit_once('-'))
        .and_then(|(_, pid)| pid.parse::<i32>().ok())
        .filter(|pid| *pid > 1)
        .context("Unrecognized OpenCode Desktop application lock; quit the app before setup")?;
    match kill(Pid::from_raw(pid), None) {
        Ok(()) | Err(Errno::EPERM) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(error) => Err(error).context("Checking whether OpenCode Desktop is running"),
    }
}

#[cfg(windows)]
fn is_running(_root: &Path) -> Result<bool> {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq OpenCode.exe", "/NH", "/FO", "CSV"])
        .output()?;
    if !output.status.success() {
        bail!("Could not check OpenCode Desktop; quit the app before setup");
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .to_ascii_lowercase()
        .contains("\"opencode.exe\""))
}

/// Catch unsupported stores (or an open app on Windows/Linux) before provisioning.
pub(super) fn preflight() -> Result<()> {
    let root = config_dir()?;
    Stores::read(&root)?;
    #[cfg(not(target_os = "macos"))]
    if is_running(&root)? {
        bail!("Quit OpenCode Desktop, then rerun this command to save its server configuration.");
    }
    Ok(())
}

async fn pause(root: &Path) -> Result<bool> {
    if !is_running(root)? {
        return Ok(false);
    }
    #[cfg(target_os = "macos")]
    {
        println!("Restarting OpenCode Desktop to apply its server configuration...");
        // NSRunningApplication requests a graceful quit without force-killing
        // helpers or requiring Apple Events access to control another app.
        let output = tokio::process::Command::new("/usr/bin/osascript")
            .args(["-l", "JavaScript", "-e", "ObjC.import('AppKit'); var apps = $.NSRunningApplication.runningApplicationsWithBundleIdentifier('ai.opencode.desktop'); for (var i = 0; i < apps.count; i++) { apps.objectAtIndex(i).terminate; }"])
            .output().await.context("Quit OpenCode Desktop and rerun setup")?;
        if !output.status.success() {
            bail!("Unable to quit OpenCode Desktop. Quit the app and rerun setup.");
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        while is_running(root)? {
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "OpenCode Desktop is still running. Quit the app and rerun setup; its configuration was not changed."
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Ok(true)
    }
    #[cfg(not(target_os = "macos"))]
    bail!("Quit OpenCode Desktop, then rerun this command to save its server configuration.")
}

async fn resume(was_running: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    if was_running {
        let status = tokio::process::Command::new("/usr/bin/open")
            .args(["-b", "ai.opencode.desktop"])
            .status()
            .await?;
        if !status.success() {
            bail!("Configuration saved. Open OpenCode Desktop manually to use the server.");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = was_running;
    Ok(())
}

pub(super) async fn configure(
    connection: &Connection,
    agent_id: &str,
    agent_name: &str,
) -> Result<()> {
    let root = config_dir()?;
    let was_running = pause(&root).await?;
    // Read AFTER quitting: the renderer flushes its latest state on exit.
    let result = (|| {
        let mut stores = Stores::read(&root)?;
        stores.upsert(connection, agent_id, agent_name)?;
        stores.save(&root)
    })();
    let restarted = resume(was_running).await;
    result?;
    restarted
}

pub(super) async fn remove(agent_id: &str) -> Result<bool> {
    let root = config_dir()?;
    if !Stores::read(&root)?.managed.contains_key(agent_id) {
        return Ok(false);
    }
    let was_running = pause(&root).await?;
    let result: Result<bool> = (|| {
        let mut stores = Stores::read(&root)?;
        let removed = stores.remove(agent_id)?;
        stores.save(&root)?;
        Ok(removed)
    })();
    let restarted = resume(was_running).await;
    let removed = result?;
    restarted?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        Connection {
            url: "https://app-test.up.railway.app".into(),
            username: "opencode".into(),
            password: "secret".into(),
            directory: "/app".into(),
            reused: false,
        }
    }

    #[test]
    fn merges_credentials_default_and_project_without_losing_other_state() {
        let root = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let original = json!({"server": json!({"list": [{"type":"http", "http":{"url":"https://other.example", "password":"other-secret"}}], "projects":{"local":[{"worktree":"/local"}]}, "custom":42}).to_string(), "model":"keep"});
        fs::write(root.path().join(GLOBAL), original.to_string()).unwrap();
        fs::write(
            root.path().join(SETTINGS),
            r#"{"windowIds":["existing"],"theme":"dark"}"#,
        )
        .unwrap();
        let mut stores = Stores::read(root.path()).unwrap();
        stores.upsert(&connection(), "agent-1", "test").unwrap();
        stores.save(root.path()).unwrap();
        let mut again = Stores::read(root.path()).unwrap();
        let mut changed = connection();
        changed.password = "new-secret".into();
        again.upsert(&changed, "agent-1", "test").unwrap();
        again.save(root.path()).unwrap();
        let saved = Stores::read(root.path()).unwrap();
        assert_eq!(saved.server["list"].as_array().unwrap().len(), 2);
        assert_eq!(saved.server["list"][1]["http"]["password"], "new-secret");
        assert_eq!(saved.server["list"][0]["http"]["password"], "other-secret");
        assert_eq!(
            saved.server["projects"][&changed.url],
            json!([{"worktree":"/app","expanded":true}])
        );
        assert_eq!(saved.server["projects"]["local"][0]["worktree"], "/local");
        assert_eq!(saved.server["lastProject"][&changed.url], "/app");
        assert_eq!(saved.settings["defaultServerUrl"], changed.url);
        assert_eq!(saved.settings["windowIds"], json!(["existing"]));
        assert_eq!(saved.global["model"], "keep");
        assert_eq!(saved.server["custom"], 42);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            for name in [GLOBAL, SETTINGS, "opencode.global.dat.railway-backup"] {
                assert_eq!(
                    fs::metadata(root.path().join(name))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
    }

    #[test]
    fn removal_only_removes_the_managed_agent_and_its_default() {
        let root = tempfile::tempdir().unwrap();
        let mut stores = Stores::read(root.path()).unwrap();
        let first = connection();
        let mut second = connection();
        second.url = "https://other.up.railway.app".into();
        stores.upsert(&first, "one", "one").unwrap();
        stores.upsert(&second, "two", "two").unwrap();
        assert!(!stores.remove("unknown").unwrap());
        assert!(stores.remove("one").unwrap());
        assert_eq!(stores.settings["defaultServerUrl"], second.url);
        assert_eq!(stores.list().unwrap().len(), 1);
        assert!(stores.remove("two").unwrap());
        assert!(!stores.settings.contains_key("defaultServerUrl"));
        assert!(stores.server["projects"].as_object().unwrap().is_empty());
    }

    #[test]
    fn invalid_store_is_rejected_without_overwriting_it() {
        let root = tempfile::tempdir().unwrap();
        for contents in ["not json", r#"{"server":{}}"#, r#"{"server":"[]"}"#] {
            fs::write(root.path().join(GLOBAL), contents).unwrap();
            assert!(Stores::read(root.path()).is_err());
            assert_eq!(
                fs::read_to_string(root.path().join(GLOBAL)).unwrap(),
                contents
            );
        }
    }
}
