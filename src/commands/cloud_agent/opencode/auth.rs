//! Export local OpenCode provider sign-ins for the VM, without copying sessions.
//!
//! OpenCode V2 keeps credentials in the `credential` table of
//! `$XDG_DATA_HOME/opencode/opencode.db` (default `~/.local/share/opencode/`).
//! A machine whose local OpenCode is still V1 has no store to export; the
//! agent asks for a provider sign-in instead.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

pub(crate) const SEED: &str = r#"mkdir -p ~/.railway/runtimes/opencode || exit 1
opencode_credentials=$(mktemp ~/.railway/runtimes/opencode/credentials.XXXXXX) || exit 1
cat > "$opencode_credentials" || { rm -f "$opencode_credentials"; exit 1; }
chmod 600 "$opencode_credentials" || exit 1
mv "$opencode_credentials" ~/.railway/runtimes/opencode/credentials.json || exit 1"#;

pub(crate) fn seed_framed(len: usize) -> String {
    // Read only this frame: buffered readers can consume the following skills
    // archive on a pipe even when they output only the requested byte count.
    let command = crate::util::shell::shell_join(&[
        "python3".into(), "-c".into(),
        "import os, sys\nremaining = int(sys.argv[1])\nwhile remaining:\n chunk = os.read(0, min(remaining, 65536))\n if not chunk: sys.exit(1)\n sys.stdout.buffer.write(chunk)\n remaining -= len(chunk)".into(),
        len.to_string(),
    ]);
    SEED.replace("cat >", &format!("{command} >"))
}

fn valid_value(value: &Value) -> bool {
    let string = |key| value.get(key).is_some_and(Value::is_string);
    let metadata = value.get("metadata").is_none_or(|value| {
        value
            .as_object()
            .is_some_and(|map| map.values().all(Value::is_string))
    });
    metadata
        && match value["type"].as_str() {
            Some("key") => string("key"),
            Some("oauth") => {
                string("methodID")
                    && string("refresh")
                    && string("access")
                    && value["expires"].as_u64().is_some()
            }
            _ => false,
        }
}

fn payload(credentials: Vec<Value>, source: PathBuf) -> Result<Option<(Vec<u8>, PathBuf)>> {
    if credentials.is_empty() {
        return Ok(None);
    }
    Ok(Some((
        serde_json::to_vec(&json!({"version":1,"credentials":credentials}))?,
        source,
    )))
}

pub(crate) fn read(
    home: &Path,
    xdg_data_home: Option<&Path>,
    database: Option<&str>,
) -> Result<Option<(Vec<u8>, PathBuf)>> {
    let data = xdg_data_home
        .filter(|p| p.is_absolute())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".local/share"))
        .join("opencode");
    if database == Some(":memory:") {
        return Ok(None);
    }
    let path = data.join(database.unwrap_or("opencode.db"));
    if path.try_exists()? {
        let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("Reading OpenCode credentials from {}", path.display()))?;
        db.busy_timeout(Duration::from_secs(5))?;
        let transaction = db.unchecked_transaction()?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='credential')",
            [],
            |row| row.get(0),
        )?;
        if exists {
            let columns = transaction
                .prepare("PRAGMA table_info(credential)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<HashSet<_>>>()?;
            if !["id", "integration_id", "label", "value", "time_created"]
                .iter()
                .all(|key| columns.contains(*key))
            {
                bail!(
                    "Unsupported OpenCode credential database; provider credentials were not copied"
                );
            }
            // Match OpenCode's account preference: active first, then newest account.
            // NULL active is valid for credentials imported by older releases.
            let order = if columns.contains("active") {
                "COALESCE(active,0) DESC,"
            } else {
                ""
            };
            let mut query = transaction.prepare(&format!(
                "SELECT id,integration_id,label,value FROM credential WHERE integration_id IS NOT NULL ORDER BY {order} time_created DESC,id DESC"
            ))?;
            let rows = query.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            let mut providers = HashSet::new();
            let mut credentials = Vec::new();
            for row in rows {
                let (id, provider, label, value) = row?;
                if provider.starts_with("mcp_") || !providers.insert(provider.clone()) {
                    continue;
                }
                let value: Value = serde_json::from_str(&value)
                    .map_err(|_| anyhow::anyhow!("Invalid OpenCode provider credential JSON"))?;
                if provider.is_empty() || !id.starts_with("cred_") || !valid_value(&value) {
                    bail!("Unsupported OpenCode provider credential; credentials were not copied");
                }
                credentials
                    .push(json!({"id":id,"integrationID":provider,"label":label,"value":value}));
            }
            return payload(credentials, path);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn database(home: &Path) -> (PathBuf, Connection) {
        let path = home.join(".local/share/opencode/opencode.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE credential (id TEXT PRIMARY KEY, integration_id TEXT, label TEXT, value TEXT, active INTEGER, time_created INTEGER); CREATE TABLE session (secret TEXT); INSERT INTO session VALUES ('private chat');").unwrap();
        (path, db)
    }
    fn insert(
        db: &Connection,
        id: &str,
        provider: &str,
        value: &Value,
        active: Option<i32>,
        created: i32,
    ) {
        db.execute(
            "INSERT INTO credential VALUES (?1,?2,'account',?3,?4,?5)",
            params![id, provider, value.to_string(), active, created],
        )
        .unwrap();
    }
    fn key(secret: &str) -> Value {
        json!({"type":"key","key":secret})
    }

    #[test]
    fn selects_active_accounts_and_preserves_oauth_metadata_without_sessions_or_mcp() {
        let home = tempfile::tempdir().unwrap();
        let (path, db) = database(home.path());
        insert(&db, "cred_old", "openai", &key("old"), Some(0), 10);
        let oauth = json!({"type":"oauth","methodID":"chatgpt-browser","access":"current","refresh":"current-refresh","expires":123,"metadata":{"accountID":"account"}});
        insert(&db, "cred_active", "openai", &oauth, Some(1), 1);
        insert(&db, "cred_go", "opencode-go", &key("go-key"), None, 2);
        insert(
            &db,
            "cred_mcp",
            "mcp_example",
            &key("mcp-secret"),
            Some(1),
            3,
        );
        let (bytes, source) = read(home.path(), None, None).unwrap().unwrap();
        assert_eq!(source, path);
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload["credentials"].as_array().unwrap().len(), 2);
        assert_eq!(payload["credentials"][0]["value"], oauth);
        assert!(!String::from_utf8(bytes).unwrap().contains("private chat"));
        assert_eq!(
            db.query_row("SELECT count(*) FROM credential", [], |r| r
                .get::<_, i32>(0))
                .unwrap(),
            4
        );
    }

    #[test]
    fn empty_v2_store_and_v1_only_auth_json_export_nothing() {
        let home = tempfile::tempdir().unwrap();
        let (path, _db) = database(home.path());
        std::fs::write(
            path.with_file_name("auth.json"),
            r#"{"openai":{"type":"api","key":"stale"}}"#,
        )
        .unwrap();
        assert!(read(home.path(), None, None).unwrap().is_none());
        // Without a V2 store at all, a V1 auth.json is not converted either.
        let v1 = tempfile::tempdir().unwrap();
        let data = v1.path().join(".local/share/opencode");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            data.join("auth.json"),
            r#"{"openai":{"type":"api","key":"v1"}}"#,
        )
        .unwrap();
        assert!(read(v1.path(), None, None).unwrap().is_none());
    }

    #[test]
    fn explicit_database_path_and_relative_xdg_fallback_work() {
        let home = tempfile::tempdir().unwrap();
        let (path, db) = database(home.path());
        insert(&db, "cred_test", "example", &key("test"), None, 1);
        assert!(
            read(home.path(), Some(Path::new("relative")), None)
                .unwrap()
                .is_some()
        );
        assert!(
            read(
                home.path(),
                Some(&home.path().join("unused")),
                path.to_str()
            )
            .unwrap()
            .is_some()
        );
        assert!(read(home.path(), None, Some(":memory:")).unwrap().is_none());
    }

    #[test]
    fn malformed_native_credentials_fail_without_exposing_values() {
        let home = tempfile::tempdir().unwrap();
        let (path, db) = database(home.path());
        insert(
            &db,
            "cred_bad",
            "openai",
            &json!({"type":"oauth","refresh":"DO-NOT-PRINT"}),
            Some(1),
            1,
        );
        std::fs::write(
            path.with_file_name("auth.json"),
            r#"{"openai":{"type":"api","key":"stale"}}"#,
        )
        .unwrap();
        let error = read(home.path(), None, None).unwrap_err().to_string();
        assert!(error.contains("Unsupported"));
        assert!(!error.contains("DO-NOT-PRINT"));
    }

    #[cfg(unix)]
    #[test]
    fn credential_seed_preserves_framed_skills_input_and_private_permissions() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};
        for framed in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let auth = br#"{"version":1,"credentials":[]}"#;
            let seed = if framed {
                format!("{}\ncat", seed_framed(auth.len()))
            } else {
                SEED.into()
            };
            let seed = seed.replace("~/", &format!("{}/", root.path().display()));
            let mut child = Command::new("sh")
                .args(["-c", &seed])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(auth).unwrap();
            if framed {
                stdin.write_all(b"skills archive").unwrap();
            }
            drop(stdin);
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success());
            assert_eq!(
                output.stdout,
                if framed {
                    b"skills archive".as_slice()
                } else {
                    b""
                }
            );
            let path = root
                .path()
                .join(".railway/runtimes/opencode/credentials.json");
            assert_eq!(std::fs::read(&path).unwrap(), auth);
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
