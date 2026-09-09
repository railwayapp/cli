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
}

pub(super) struct Target {
    channel: Channel,
    pub root: PathBuf,
}

impl Target {
    pub fn name(&self) -> &'static str {
        self.channel.name()
    }

    fn is_installed(&self) -> bool {
        [SETTINGS, GLOBAL, DATABASE]
            .iter()
            .any(|name| self.root.join(name).is_file())
    }
}

fn target_at(base: &Path, beta: bool) -> Target {
    let channel = if beta {
        Channel::Beta
    } else {
        Channel::Standard
    };
    Target {
        channel,
        root: base.join(channel.id()),
    }
}

pub(super) fn targets(beta: bool) -> Result<Vec<Target>> {
    let base = dirs::config_dir()
        .context("Unable to locate OpenCode Desktop's configuration directory")?;
    Ok(vec![target_at(&base, beta)])
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
        // Stable Desktop also has drafts.sqlite, containing only document/blob.
        // Select SQLite only when the renderer state table actually exists.
        let sqlite = if root.join(DATABASE).exists() {
            let db =
                Database::open_with_flags(root.join(DATABASE), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'state')", [], |row| row.get::<_, bool>(0))?
        } else {
            false
        };
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

/// Catch unsupported stores before provisioning.
pub(super) fn preflight(beta: bool) -> Result<()> {
    for target in targets(beta)? {
        Stores::read(&target.root)
            .with_context(|| format!("Reading {} settings", target.name()))?;
    }
    Ok(())
}

/// Opportunistic setup for `railway code`: only write to an existing Desktop
/// installation of the selected edition. Callers keep failures non-fatal.
pub(crate) async fn configure_installed(
    beta: bool,
    connection: &Connection,
    agent_id: &str,
    agent_name: &str,
) -> Result<bool> {
    let mut configured = false;
    for target in targets(beta)? {
        configured |= configure_installed_target(&target, connection, agent_id, agent_name).await?;
    }
    Ok(configured)
}

async fn configure_installed_target(
    target: &Target,
    connection: &Connection,
    agent_id: &str,
    agent_name: &str,
) -> Result<bool> {
    if !target.is_installed() {
        return Ok(false);
    }
    configure_target(target, connection, agent_id, agent_name)
        .await
        .with_context(|| format!("Configuring {} Desktop", target.name()))?;
    Ok(true)
}

pub(super) async fn configure(
    beta: bool,
    connection: &Connection,
    agent_id: &str,
    agent_name: &str,
) -> Result<()> {
    for target in targets(beta)? {
        configure_target(&target, connection, agent_id, agent_name)
            .await
            .with_context(|| format!("Configuring {}", target.name()))?;
    }
    Ok(())
}

async fn configure_target(
    target: &Target,
    connection: &Connection,
    agent_id: &str,
    agent_name: &str,
) -> Result<()> {
    let root = &target.root;
    let mut stores = Stores::read(root)?;
    stores.upsert(connection, agent_id, agent_name)?;
    stores.save(root)
}

pub(super) async fn remove(agent_id: &str, beta: bool) -> Result<bool> {
    let mut removed = false;
    for target in targets(beta)? {
        removed |= remove_target(&target, agent_id).await?;
    }
    Ok(removed)
}

async fn remove_target(target: &Target, agent_id: &str) -> Result<bool> {
    let root = &target.root;
    let mut stores = Stores::read(root)?;
    if !stores.remove(agent_id)? {
        return Ok(false);
    }
    stores.save(root)?;
    Ok(true)
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

    #[tokio::test]
    async fn automatic_setup_skips_missing_desktop_even_if_other_edition_exists() {
        for beta in [false, true] {
            let base = tempfile::tempdir().unwrap();
            let target = target_at(base.path(), beta);
            let other = target_at(base.path(), !beta);
            fs::create_dir_all(&other.root).unwrap();
            fs::write(other.root.join(SETTINGS), "{}").unwrap();
            assert!(other.is_installed());
            assert!(
                !configure_installed_target(&target, &connection(), "agent", "box")
                    .await
                    .unwrap()
            );
            assert!(!target.root.exists());

            // An empty directory or Railway's own metadata is not evidence of
            // an installed Desktop app. Neither are directories named as stores.
            fs::create_dir_all(&target.root).unwrap();
            fs::write(target.root.join(MANAGED), "{}").unwrap();
            for name in [SETTINGS, GLOBAL, DATABASE] {
                fs::create_dir(target.root.join(name)).unwrap();
            }
            assert!(
                !configure_installed_target(&target, &connection(), "agent", "box")
                    .await
                    .unwrap()
            );
            assert_eq!(fs::read_dir(&target.root).unwrap().count(), 4);
            assert_eq!(fs::read_dir(&other.root).unwrap().count(), 1);
        }
    }

    #[tokio::test]
    async fn automatic_setup_detects_stores_and_refreshes_the_saved_connection() {
        for beta in [false, true] {
            for name in [SETTINGS, GLOBAL, DATABASE] {
                let base = tempfile::tempdir().unwrap();
                let target = target_at(base.path(), beta);
                fs::create_dir_all(&target.root).unwrap();
                if name == DATABASE {
                    let db = Database::open(target.root.join(DATABASE)).unwrap();
                    db.execute_batch(
                        "CREATE TABLE state (name TEXT NOT NULL, key TEXT NOT NULL,
                            value TEXT NOT NULL, updated_at INTEGER NOT NULL,
                            PRIMARY KEY (name, key));",
                    )
                    .unwrap();
                } else {
                    fs::write(target.root.join(name), "{}").unwrap();
                }
                let mut connection = connection();
                for password in ["initial-password", "refreshed-password"] {
                    connection.password = password.into();
                    connection.directory = "/app/custom-project".into();
                    assert!(
                        configure_installed_target(&target, &connection, "agent", "box")
                            .await
                            .unwrap()
                    );
                    let saved = Stores::read(&target.root).unwrap();
                    assert_eq!(saved.sqlite, name == DATABASE);
                    assert_eq!(saved.server["list"].as_array().unwrap().len(), 1);
                    assert_eq!(saved.server["list"][0]["http"]["password"], password);
                    assert_eq!(saved.server["list"][0]["displayName"], "Railway: box");
                    assert_eq!(saved.settings["defaultServerUrl"], connection.url);
                    assert_eq!(
                        saved.server["projects"][&connection.url][0]["worktree"],
                        connection.directory
                    );
                    assert_eq!(saved.managed["agent"], connection.url);
                }
                assert!(!target_at(base.path(), !beta).root.exists());
            }
        }
    }

    #[tokio::test]
    async fn automatic_setup_reports_invalid_stores_without_overwriting_them() {
        for name in [GLOBAL, DATABASE] {
            let base = tempfile::tempdir().unwrap();
            let target = target_at(base.path(), true);
            fs::create_dir_all(&target.root).unwrap();
            fs::write(target.root.join(name), "invalid settings").unwrap();
            assert!(
                configure_installed_target(&target, &connection(), "agent", "box")
                    .await
                    .is_err()
            );
            assert_eq!(
                fs::read_to_string(target.root.join(name)).unwrap(),
                "invalid settings"
            );
            assert!(!target.root.join(SETTINGS).exists());
            assert!(!target.root.join(MANAGED).exists());
        }
    }

    #[tokio::test]
    async fn configuration_and_removal_preserve_existing_application_lock() {
        for beta in [false, true] {
            let base = tempfile::tempdir().unwrap();
            let target = target_at(base.path(), beta);
            fs::create_dir_all(&target.root).unwrap();
            fs::write(target.root.join(SETTINGS), "{}").unwrap();
            // Setup must not interpret or manipulate Desktop's process lock.
            let lock = target.root.join("SingletonLock");
            fs::write(&lock, "desktop owns this lock").unwrap();
            let connection = connection();
            assert!(
                configure_installed_target(&target, &connection, "agent", "box")
                    .await
                    .unwrap()
            );
            let saved = Stores::read(&target.root).unwrap();
            assert_eq!(saved.settings["defaultServerUrl"], connection.url);
            assert_eq!(
                saved.server["list"][0]["http"]["password"],
                connection.password
            );
            assert_eq!(fs::read_to_string(&lock).unwrap(), "desktop owns this lock");
            assert!(remove_target(&target, "agent").await.unwrap());
            assert!(Stores::read(&target.root).unwrap().managed.is_empty());
            assert_eq!(fs::read_to_string(&lock).unwrap(), "desktop owns this lock");
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
    fn standard_and_beta_target_only_the_requested_channel() {
        let base = tempfile::tempdir().unwrap();
        for beta in [false, true] {
            let target = target_at(base.path(), beta);
            assert_eq!(
                target.channel,
                if beta {
                    Channel::Beta
                } else {
                    Channel::Standard
                }
            );
            assert_eq!(target.root, base.path().join(target.channel.id()));
        }
    }

    #[test]
    fn standard_drafts_database_keeps_server_settings_in_json() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::open(root.path().join(DATABASE)).unwrap();
        db.execute_batch("CREATE TABLE document (key TEXT PRIMARY KEY, value TEXT NOT NULL); INSERT INTO document VALUES ('draft', 'keep'); CREATE TABLE blob (id TEXT PRIMARY KEY, data BLOB NOT NULL);").unwrap();
        fs::write(
            root.path().join(GLOBAL),
            r#"{"other":"preserved","server":"{\"list\":[]}"}"#,
        )
        .unwrap();
        let mut stores = Stores::read(root.path()).unwrap();
        assert!(!stores.sqlite);
        stores.upsert(&connection(), "agent", "box").unwrap();
        stores.save(root.path()).unwrap();
        let saved = Stores::read(root.path()).unwrap();
        assert_eq!(saved.global["other"], "preserved");
        assert_eq!(saved.server["list"].as_array().unwrap().len(), 1);
        assert_eq!(
            db.query_row("SELECT value FROM document WHERE key='draft'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "keep"
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
    fn malformed_state_table_is_rejected_instead_of_falling_back_to_json() {
        let root = tempfile::tempdir().unwrap();
        let db = Database::open(root.path().join(DATABASE)).unwrap();
        db.execute_batch(
            "CREATE TABLE state (wrong TEXT); CREATE TABLE document (value TEXT); INSERT INTO document VALUES ('keep');",
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
