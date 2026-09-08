//! OpenCode Desktop stores (stable v1.18.29 and beta 0.0.0-beta-19289).
//! `server` is a JSON string inside opencode.global.dat, while the default
//! URL lives in opencode.settings. Newer Desktop builds move renderer state
//! into drafts.sqlite. Keep unknown keys and existing connections in either.
//! https://github.com/anomalyco/opencode/blob/v1.18.29/packages/app/src/context/server.tsx
//! https://github.com/anomalyco/opencode/blob/v1.18.29/packages/desktop/src/main/store.ts
//! Beta's state schema and channel IDs were verified in the packaged release:
//! https://github.com/anomalyco/opencode-beta/releases/tag/v0.0.0-beta-19289

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection as Database, OpenFlags, OptionalExtension, params};
use serde_json::{Map, Value, json};

use super::opencode::Connection;

const SETTINGS: &str = "opencode.settings";
const GLOBAL: &str = "opencode.global.dat";
// Ownership metadata is separate from OpenCode's stores and has no secrets.
const MANAGED: &str = "railway.servers.json";
const DATABASE: &str = "drafts.sqlite";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Channel {
    Standard,
    Beta,
}

impl Channel {
    fn id(self) -> &'static str {
        match self {
            Self::Standard => "ai.opencode.desktop",
            Self::Beta => "ai.opencode.desktop.beta",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Standard => "OpenCode",
            Self::Beta => "OpenCode Beta",
        }
    }

    fn installed(self) -> bool {
        #[cfg(target_os = "macos")]
        {
            let app = format!("{}.app", self.name());
            Path::new("/Applications").join(&app).is_dir()
                || dirs::home_dir().is_some_and(|home| home.join("Applications").join(app).is_dir())
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }
}

pub(super) struct Target {
    channel: Channel,
    pub root: PathBuf,
}

impl Target {
    pub fn name(&self) -> &'static str {
        self.channel.name()
    }
}

fn detected_targets(base: &Path, installed: impl Fn(Channel) -> bool) -> Vec<Target> {
    let mut targets = [Channel::Standard, Channel::Beta]
        .into_iter()
        .filter(|channel| base.join(channel.id()).is_dir() || installed(*channel))
        .map(|channel| Target {
            channel,
            root: base.join(channel.id()),
        })
        .collect::<Vec<_>>();
    // Preserve setup before the first standard Desktop launch. A beta-only
    // installation/configuration is targeted without creating standard stores.
    if targets.is_empty() {
        targets.push(Target {
            channel: Channel::Standard,
            root: base.join(Channel::Standard.id()),
        });
    }
    targets
}

pub(super) fn targets() -> Result<Vec<Target>> {
    let base = dirs::config_dir()
        .context("Unable to locate OpenCode Desktop's configuration directory")?;
    Ok(detected_targets(&base, Channel::installed))
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
    sqlite: bool,
}

impl Stores {
    fn read(root: &Path) -> Result<Self> {
        let sqlite = root.join(DATABASE).exists();
        let global = if sqlite {
            let db =
                Database::open_with_flags(root.join(DATABASE), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let mut global = Map::new();
            if let Some(value) = read_server(&db)? {
                global.insert("server".into(), Value::String(value));
            }
            global
        } else {
            read_object(&root.join(GLOBAL))?
        };
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
            sqlite,
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
        // Beta filters hidden servers out of its picker. Its schema also
        // requires these maps when seeding a previously empty store.
        object(&mut self.server, "hidden")?.insert(url.clone(), json!(false));
        object(&mut self.server, "recentlyClosed")?;
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
        for key in ["projects", "lastProject", "recentlyClosed", "hidden"] {
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
            if name == GLOBAL && self.sqlite {
                self.save_database(root)?;
                continue;
            }
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

    fn save_database(&self, root: &Path) -> Result<()> {
        let mut db =
            Database::open_with_flags(root.join(DATABASE), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        let transaction = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let value = serde_json::to_string(&self.server)?;
        let previous = read_server(&transaction)?;
        if previous.as_deref() != Some(&value) {
            // Back up just the connection row; draft documents and other
            // renderer state remain in place and are never rewritten.
            if let Some(previous) = previous {
                write_private(
                    &root.join(format!("{GLOBAL}.railway-backup")),
                    &serde_json::to_vec_pretty(&json!({"server": previous}))?,
                )?;
            }
            transaction.execute(
                "INSERT INTO state (name, key, value, updated_at) VALUES (?1, 'server', ?2, ?3) \
                 ON CONFLICT(name, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![GLOBAL, value, chrono::Utc::now().timestamp_millis()],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

fn read_server(db: &Database) -> Result<Option<String>> {
    db.query_row(
        "SELECT value FROM state WHERE name = ?1 AND key = 'server'",
        [GLOBAL],
        |row| row.get(0),
    )
    .optional()
    .context("Unsupported OpenCode Desktop database; expected the renderer state table")
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
fn is_running(target: &Target) -> Result<bool> {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    // Chromium's singleton lock is a symlink named <hostname>-<pid>.
    let lock = match fs::read_link(target.root.join("SingletonLock")) {
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
fn is_running(target: &Target) -> Result<bool> {
    let executable = format!("{}.exe", target.name());
    let output = std::process::Command::new("tasklist")
        .args([
            "/FI",
            &format!("IMAGENAME eq {executable}"),
            "/NH",
            "/FO",
            "CSV",
        ])
        .output()?;
    if !output.status.success() {
        bail!("Could not check OpenCode Desktop; quit the app before setup");
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .to_ascii_lowercase()
        .contains(&format!("\"{}\"", executable.to_ascii_lowercase())))
}

/// Catch unsupported stores (or an open app on Windows/Linux) before provisioning.
pub(super) fn preflight() -> Result<()> {
    for target in targets()? {
        Stores::read(&target.root)
            .with_context(|| format!("Reading {} settings", target.name()))?;
        #[cfg(not(target_os = "macos"))]
        if is_running(&target)? {
            bail!(
                "Quit {}, then rerun this command to save its server configuration.",
                target.name()
            );
        }
    }
    Ok(())
}

async fn pause(target: &Target) -> Result<bool> {
    if !is_running(target)? {
        return Ok(false);
    }
    #[cfg(target_os = "macos")]
    {
        println!(
            "Restarting {} to apply its server configuration...",
            target.name()
        );
        // NSRunningApplication requests a graceful quit without force-killing
        // helpers or requiring Apple Events access to control another app.
        let script = format!(
            "ObjC.import('AppKit'); var apps = $.NSRunningApplication.runningApplicationsWithBundleIdentifier('{}'); for (var i = 0; i < apps.count; i++) {{ apps.objectAtIndex(i).terminate; }}",
            target.channel.id()
        );
        let output = tokio::process::Command::new("/usr/bin/osascript")
            .args(["-l", "JavaScript", "-e", &script])
            .output()
            .await
            .context("Quit OpenCode Desktop and rerun setup")?;
        if !output.status.success() {
            bail!("Unable to quit OpenCode Desktop. Quit the app and rerun setup.");
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        while is_running(target)? {
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

async fn resume(target: &Target, was_running: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    if was_running {
        let status = tokio::process::Command::new("/usr/bin/open")
            .args(["-b", target.channel.id()])
            .status()
            .await?;
        if !status.success() {
            bail!("Configuration saved. Open OpenCode Desktop manually to use the server.");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (target, was_running);
    Ok(())
}

pub(super) async fn configure(
    connection: &Connection,
    agent_id: &str,
    agent_name: &str,
) -> Result<()> {
    for target in targets()? {
        configure_target(&target, connection, agent_id, agent_name)
            .await
            .with_context(|| format!("Configuring {}", target.name()))?;
        println!(
            "Saved connection in {} ({})",
            target.name(),
            target.root.display()
        );
        if target.channel == Channel::Beta && !beta_compatible(connection).await {
            eprintln!(
                "OpenCode Beta settings were saved, but this server does not provide the versioned API required by Beta. Run a compatible OpenCode 2 server on the agent before using this connection in Beta."
            );
        }
    }
    Ok(())
}

async fn beta_compatible(connection: &Connection) -> bool {
    // Beta 19289 explicitly marks unversioned /api/health responses (including
    // stable 1.18.29) incompatible, even when the server reports healthy.
    let Ok(client) = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()
    else {
        return false;
    };
    let Ok(response) = client
        .get(format!("{}/api/health", connection.url))
        .basic_auth("opencode", Some(&connection.password))
        .send()
        .await
    else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    response
        .json::<Value>()
        .await
        .ok()
        .is_some_and(|body| body["healthy"] == true && body["version"].is_string())
}

async fn configure_target(
    target: &Target,
    connection: &Connection,
    agent_id: &str,
    agent_name: &str,
) -> Result<()> {
    let root = &target.root;
    let was_running = pause(target).await?;
    // Read AFTER quitting: the renderer flushes its latest state on exit.
    let result = (|| {
        let mut stores = Stores::read(root)?;
        stores.upsert(connection, agent_id, agent_name)?;
        stores.save(root)
    })();
    let restarted = resume(target, was_running).await;
    result?;
    restarted
}

pub(super) async fn remove(agent_id: &str) -> Result<bool> {
    let mut removed = false;
    for target in targets()? {
        removed |= remove_target(&target, agent_id).await?;
    }
    Ok(removed)
}

async fn remove_target(target: &Target, agent_id: &str) -> Result<bool> {
    let root = &target.root;
    if !Stores::read(root)?.managed.contains_key(agent_id) {
        return Ok(false);
    }
    let was_running = pause(target).await?;
    let result: Result<bool> = (|| {
        let mut stores = Stores::read(root)?;
        let removed = stores.remove(agent_id)?;
        stores.save(root)?;
        Ok(removed)
    })();
    let restarted = resume(target, was_running).await;
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

    #[test]
    fn detects_standard_beta_and_both_without_creating_other_channels() {
        let base = tempfile::tempdir().unwrap();
        let channels = |targets: Vec<Target>| {
            targets
                .into_iter()
                .map(|target| target.channel)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            channels(detected_targets(base.path(), |_| false)),
            [Channel::Standard]
        );
        assert_eq!(
            channels(detected_targets(base.path(), |channel| channel == Channel::Beta)),
            [Channel::Beta]
        );
        fs::create_dir(base.path().join(Channel::Beta.id())).unwrap();
        assert_eq!(
            channels(detected_targets(base.path(), |_| false)),
            [Channel::Beta]
        );
        fs::create_dir(base.path().join(Channel::Standard.id())).unwrap();
        assert_eq!(
            channels(detected_targets(base.path(), |_| false)),
            [Channel::Standard, Channel::Beta]
        );
    }

    #[test]
    fn beta_sqlite_merges_server_without_touching_drafts_or_other_state() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::open(root.path().join(DATABASE)).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE state (name TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
                updated_at INTEGER NOT NULL, PRIMARY KEY (name, key));
            CREATE TABLE document (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO document VALUES ('draft-1', 'keep draft');
            INSERT INTO state VALUES ('opencode.window.one.dat', 'tabs', '[1]', 123);
            INSERT INTO state VALUES ('opencode.global.dat', 'model', 'keep model', 456);",
        )
        .unwrap();
        let connection = connection();
        let original = json!({"list":[], "hidden":{&connection.url:true},
            "projects":{"local":[{"worktree":"/local","expanded":true}]},
            "lastProject":{"local":"/local"}, "recentlyClosed":{}})
        .to_string();
        db.execute(
            "INSERT INTO state VALUES (?1, 'server', ?2, 123)",
            params![GLOBAL, original],
        )
        .unwrap();
        // A stale pre-migration file must not override the live SQLite state.
        fs::write(
            root.path().join(GLOBAL),
            r#"{"server":"invalid legacy state"}"#,
        )
        .unwrap();
        for _ in 0..2 {
            let mut stores = Stores::read(root.path()).unwrap();
            stores.upsert(&connection, "beta-agent", "box").unwrap();
            stores.save(root.path()).unwrap();
        }
        let mut stores = Stores::read(root.path()).unwrap();
        assert!(stores.sqlite);
        assert_eq!(stores.server["list"].as_array().unwrap().len(), 1);
        assert_eq!(
            stores.server["list"][0]["http"]["password"],
            connection.password
        );
        assert_eq!(stores.server["hidden"][&connection.url], false);
        assert_eq!(
            stores.server["projects"][&connection.url][0]["worktree"],
            "/app"
        );
        assert_eq!(stores.server["lastProject"][&connection.url], "/app");
        assert_eq!(stores.settings["defaultServerUrl"], connection.url);
        let backup = read_object(&root.path().join(format!("{GLOBAL}.railway-backup"))).unwrap();
        assert_eq!(backup["server"], original);
        assert!(stores.remove("beta-agent").unwrap());
        stores.save(root.path()).unwrap();
        let removed = Stores::read(root.path()).unwrap();
        assert!(removed.server["list"].as_array().unwrap().is_empty());
        assert_eq!(removed.server["projects"]["local"][0]["worktree"], "/local");
        assert!(removed.server["hidden"].as_object().unwrap().is_empty());
        assert!(!removed.settings.contains_key("defaultServerUrl"));
        assert_eq!(
            db.query_row(
                "SELECT value FROM document WHERE key = 'draft-1'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "keep draft"
        );
        assert_eq!(
            db.query_row(
                "SELECT value, updated_at FROM state WHERE key = 'tabs'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
            ("[1]".into(), 123)
        );
        assert_eq!(
            db.query_row(
                "SELECT value, updated_at FROM state WHERE key = 'model'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
            ("keep model".into(), 456)
        );
    }

    #[test]
    fn unknown_sqlite_schema_is_rejected_instead_of_falling_back_to_json() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::open(root.path().join(DATABASE)).unwrap();
        db.execute_batch(
            "CREATE TABLE document (value TEXT); INSERT INTO document VALUES ('keep');",
        )
        .unwrap();
        assert!(Stores::read(root.path()).is_err());
        assert!(!root.path().join(GLOBAL).exists());
        assert!(!root.path().join(SETTINGS).exists());
        assert_eq!(
            db.query_row("SELECT value FROM document", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "keep"
        );
    }
}
