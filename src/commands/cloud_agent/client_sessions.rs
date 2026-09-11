//! Conversation discovery for local clients of remote coding-agent servers.
//! Row identities include the provider and VM; a thread ID is never an SSH name.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

use super::{codex, opencode};

#[derive(Clone)]
pub(crate) enum Connection {
    Codex(codex::Connection),
    OpenCode(opencode::Connection, bool),
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct Thread {
    pub id: String,
    pub title: String,
    pub directory: String,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: String,
    pub state: String,
}

pub(crate) fn name(harness: &str, agent: &str, thread: Option<&str>) -> String {
    format!(
        "client-thread:{harness}:{agent}:{}",
        thread.unwrap_or_default()
    )
}

pub(crate) const NEW_THREAD: &str = "New Thread";

pub(crate) fn draft_name(harness: &str, agent: &str, pane: &str) -> String {
    name(harness, agent, Some(&format!("~{pane}")))
}

pub(crate) fn parse_name(name: &str) -> Option<(&str, &str, Option<&str>)> {
    let mut fields = name.strip_prefix("client-thread:")?.splitn(3, ':');
    let harness = fields.next()?;
    if !matches!(
        harness,
        "codex" | "opencode" | "opencode2" | "claude" | "grok" | "railway"
    ) {
        return None;
    }
    let agent = fields.next()?;
    if agent.is_empty() {
        return None;
    }
    let thread = fields.next()?;
    Some((
        harness,
        agent,
        (!thread.is_empty() && !thread.starts_with('~')).then_some(thread),
    ))
}

pub(crate) fn is_client(name: &str) -> bool {
    parse_name(name).is_some()
}

impl Connection {
    pub(crate) fn harness(&self) -> &'static str {
        match self {
            Self::Codex(_) => "codex",
            Self::OpenCode(_, false) => "opencode",
            Self::OpenCode(_, true) => "opencode2",
        }
    }

    pub(crate) fn directory(&self) -> &str {
        match self {
            Self::Codex(c) => &c.directory,
            Self::OpenCode(c, _) => &c.directory,
        }
    }

    pub(crate) fn set_directory(&mut self, directory: &str) {
        match self {
            Self::Codex(c) => c.directory = directory.into(),
            Self::OpenCode(c, _) => c.directory = directory.into(),
        }
    }

    pub(crate) fn args(&self, thread: Option<&str>) -> Vec<String> {
        let mut args = match self {
            Self::Codex(c) => codex::attach_args(c),
            Self::OpenCode(c, beta) => opencode::attach_args(c, *beta),
        };
        if let Some(thread) = thread {
            match self {
                Self::Codex(_) => args.extend(["resume".into(), thread.into()]),
                Self::OpenCode(_, _) => args.extend(["--session".into(), thread.into()]),
            }
        }
        args
    }

    pub(crate) async fn list(&self) -> Result<Vec<Thread>> {
        // Bound an entire paginated refresh, not just each individual request.
        tokio::time::timeout(Duration::from_secs(20), self.list_inner())
            .await
            .context("Conversation discovery timed out")?
    }

    async fn list_inner(&self) -> Result<Vec<Thread>> {
        let mut rows = Vec::new();
        match self {
            Self::Codex(c) => {
                let mut rpc = codex::Rpc::connect(c).await?;
                let mut cursor = Value::Null;
                loop {
                    let page = rpc.call("thread/list", json!({
                        "limit": 100, "cursor": cursor, "sortKey": "updated_at",
                        "archived": false, "sourceKinds": ["cli", "vscode", "appServer", "exec"]
                    })).await?;
                    for row in page["data"]
                        .as_array()
                        .context("Invalid Codex thread list")?
                    {
                        rows.push(parse_codex(row)?);
                    }
                    let next = page["nextCursor"].clone();
                    if next.is_null() {
                        break;
                    }
                    if next == cursor {
                        bail!("Codex repeated its conversation cursor");
                    }
                    cursor = next;
                }
            }
            Self::OpenCode(c, beta) => {
                let mut cursor = None::<String>;
                loop {
                    let mut query = Vec::new();
                    if *beta {
                        query.extend([("limit", "100".into()), ("order", "desc".into())]);
                    }
                    if let Some(cursor) = &cursor {
                        query.push(("cursor", cursor.clone()));
                    }
                    let page =
                        opencode_request(c, *beta, reqwest::Method::GET, "session", &query, None)
                            .await?;
                    let data = if *beta { &page["data"] } else { &page };
                    for row in data.as_array().context("Invalid OpenCode session list")? {
                        if row["parentID"].as_str().is_some_and(|id| !id.is_empty())
                            || row["time"]["archived"]
                                .as_i64()
                                .is_some_and(|time| time > 0)
                        {
                            continue;
                        }
                        rows.push(parse_opencode(row, &c.directory)?);
                    }
                    let next = page["cursor"]["next"].as_str().map(str::to_owned);
                    if !*beta || next.is_none() {
                        break;
                    }
                    if next == cursor {
                        bail!("OpenCode repeated its conversation cursor");
                    }
                    cursor = next;
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        rows.retain(|row| seen.insert(row.id.clone()));
        Ok(rows)
    }

    pub(crate) async fn thread(&self, id: &str) -> Result<Thread> {
        validate_id(id)?;
        match self {
            Self::Codex(c) => {
                let mut rpc = codex::Rpc::connect(c).await?;
                let result = rpc
                    .call(
                        "thread/read",
                        json!({"threadId": id, "includeTurns": false}),
                    )
                    .await?;
                parse_codex(&result["thread"])
            }
            Self::OpenCode(c, beta) => {
                let result = opencode_request(
                    c,
                    *beta,
                    reqwest::Method::GET,
                    &format!("session/{id}"),
                    &[],
                    None,
                )
                .await?;
                parse_opencode(if *beta { &result["data"] } else { &result }, &c.directory)
            }
        }
    }

    /// Delete through the harness so its history indexes and transcripts agree.
    pub(crate) async fn delete_thread(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        tokio::time::timeout(Duration::from_secs(30), async {
            match self {
                Self::Codex(c) => {
                    let mut rpc = codex::Rpc::connect(c).await?;
                    if let Err(error) = rpc.call("thread/delete", json!({"threadId": id})).await
                        && !error
                            .to_string()
                            .ends_with(&format!("no rollout found for thread id {id}"))
                    {
                        return Err(error);
                    }
                }
                Self::OpenCode(c, beta) => {
                    opencode_request(
                        c,
                        *beta,
                        reqwest::Method::DELETE,
                        &format!("session/{id}"),
                        &[],
                        None,
                    )
                    .await?;
                }
            }
            Ok(())
        })
        .await
        .context("Conversation deletion timed out")?
    }

    /// Only pre-create a conversation when seeding a prompt. A bare launch
    /// belongs on the native home screen, without a forced --session argument.
    pub(crate) async fn new_thread(&self, prompt: Option<&str>) -> Result<Option<Thread>> {
        if prompt.is_none_or(|prompt| prompt.trim().is_empty()) {
            return Ok(None);
        }
        match self {
            Self::Codex(_) => Ok(None),
            Self::OpenCode(c, beta) => {
                let body = if *beta {
                    json!({"location": {"directory": c.directory}})
                } else {
                    json!({})
                };
                let result =
                    opencode_request(c, *beta, reqwest::Method::POST, "session", &[], Some(body))
                        .await?;
                Ok(Some(parse_opencode(
                    if *beta { &result["data"] } else { &result },
                    &c.directory,
                )?))
            }
        }
    }

    pub(crate) async fn initial_prompt(
        &self,
        thread: Option<&Thread>,
        prompt: Option<String>,
    ) -> Result<Option<String>> {
        if let (Self::OpenCode(c, false), Some(thread), Some(prompt)) = (self, thread, &prompt) {
            opencode_request(
                c,
                false,
                reqwest::Method::POST,
                &format!("session/{}/prompt_async", thread.id),
                &[],
                Some(json!({"parts": [{"type": "text", "text": prompt}]})),
            )
            .await?;
            return Ok(None);
        }
        Ok(prompt)
    }
}

pub(super) fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("Invalid conversation ID");
    }
    Ok(())
}

pub(super) fn title(value: Option<&str>, fallback: &str) -> String {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub(super) fn parse_codex(row: &Value) -> Result<Thread> {
    let id = row["id"].as_str().context("Codex thread has no ID")?;
    validate_id(id)?;
    Ok(Thread {
        id: id.into(),
        title: title(
            row["name"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .or(row["preview"].as_str()),
            NEW_THREAD,
        ),
        directory: row["cwd"]
            .as_str()
            .context("Codex thread has no directory")?
            .into(),
        created_at: row["createdAt"]
            .as_i64()
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0)),
        updated_at: row["updatedAt"]
            .as_i64()
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .map(|t| t.to_rfc3339())
            .unwrap_or_default(),
        state: match row["status"]["type"].as_str() {
            Some("active") => "working",
            Some("systemError") => "failed",
            _ => "idle",
        }
        .into(),
    })
}

pub(super) fn parse_opencode(row: &Value, directory: &str) -> Result<Thread> {
    let id = row["id"].as_str().context("OpenCode session has no ID")?;
    validate_id(id)?;
    Ok(Thread {
        id: id.into(),
        title: title(
            row["title"]
                .as_str()
                .filter(|title| !title.starts_with("New session - ")),
            NEW_THREAD,
        ),
        directory: row["location"]["directory"]
            .as_str()
            .or(row["directory"].as_str())
            .unwrap_or(directory)
            .into(),
        created_at: row["time"]["created"]
            .as_i64()
            .and_then(chrono::DateTime::from_timestamp_millis),
        updated_at: row["time"]["updated"]
            .as_i64()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|t| t.to_rfc3339())
            .unwrap_or_default(),
        state: "idle".into(),
    })
}

async fn opencode_request(
    c: &opencode::Connection,
    beta: bool,
    method: reqwest::Method,
    path: &str,
    query: &[(&str, String)],
    body: Option<Value>,
) -> Result<Value> {
    let deleting = method == reqwest::Method::DELETE;
    let url = opencode::validate_url(&c.url)?
        .join(&format!("{}{path}", if beta { "api/" } else { "" }))?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut request = client
        .request(method, url)
        .basic_auth(&c.username, Some(&c.password))
        .query(query);
    if !beta {
        request = request.query(&[("directory", &c.directory)]);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    if deleting && response.status() == reqwest::StatusCode::NOT_FOUND {
        let error: Value = response.json().await.unwrap_or(Value::Null);
        if error["_tag"] == "SessionNotFoundError" || error["name"] == "NotFoundError" {
            return Ok(Value::Null);
        }
        bail!("OpenCode conversation deletion failed (404 Not Found)");
    }
    if !response.status().is_success() {
        bail!(
            "OpenCode conversation request failed ({})",
            response.status()
        );
    }
    if response.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(Value::Null);
    }
    Ok(response.json().await?)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn deletion_rejects_non_native_ids_before_connecting() {
        for beta in [false, true] {
            for id in ["", "../neighbor", "id?all=true", "~draft", "id/child"] {
                let connection = Connection::OpenCode(
                    opencode::Connection {
                        url: "https://backend.invalid".into(),
                        username: "opencode".into(),
                        password: "secret".into(),
                        directory: "/app/project".into(),
                        reused: true,
                    },
                    beta,
                );
                assert_eq!(
                    connection.delete_thread(id).await.unwrap_err().to_string(),
                    "Invalid conversation ID"
                );
            }
        }
    }

    #[tokio::test]
    async fn bare_opencode_launch_opens_home_without_creating_a_session() {
        for beta in [false, true] {
            let connection = super::Connection::OpenCode(
                super::opencode::Connection {
                    url: "http://127.0.0.1:1".into(),
                    username: "opencode".into(),
                    password: "secret".into(),
                    directory: "/app".into(),
                    reused: false,
                },
                beta,
            );
            for prompt in [None, Some(" \n ")] {
                assert!(connection.new_thread(prompt).await.unwrap().is_none());
            }
            assert!(!connection.args(None).iter().any(|arg| arg == "--session"));
            assert!(
                connection
                    .args(Some("chosen-thread"))
                    .windows(2)
                    .any(|args| args == ["--session", "chosen-thread"])
            );
        }
    }
    use super::*;

    #[test]
    fn native_titles_and_directories_win_over_previews_and_server_defaults() {
        let codex = parse_codex(&json!({"id":"thread-1", "cwd":"/app/worktree", "name":"Fix startup", "preview":"Original prompt", "createdAt":1700000000,"updatedAt":1700000001})).unwrap();
        assert_eq!(codex.title, "Fix startup");
        assert_eq!(codex.directory, "/app/worktree");
        let legacy = parse_opencode(&json!({"id":"ses_1", "title":"Renamed session", "directory":"/app/old", "time":{"created":1700000000000_i64,"updated":1700000001000_i64}}), "/default").unwrap();
        let beta = parse_opencode(&json!({"id":"ses_2", "title":"Beta session", "location":{"directory":"/app/new"},"time":{"created":1700000000000_i64,"updated":1700000001000_i64}}), "/default").unwrap();
        assert_eq!(legacy.title, "Renamed session");
        assert_eq!(legacy.directory, "/app/old");
        assert_eq!(beta.directory, "/app/new");
        assert_eq!(codex.created_at, beta.created_at);
        assert_eq!(codex.updated_at, legacy.updated_at);
        assert!(parse_codex(&json!({"id":"../not-a-thread", "cwd":"/app"})).is_err());
        assert_eq!(title(Some(" a\n\x1bb "), "empty"), "a  b");
    }

    #[test]
    fn thread_identity_is_scoped_to_the_provider_and_vm() {
        let mut names = std::collections::HashSet::new();
        for harness in ["codex", "opencode", "opencode2"] {
            for agent in ["vm-a", "vm-b"] {
                let row = name(harness, agent, Some("same-thread-id"));
                assert!(names.insert(row.clone()));
                assert_eq!(
                    parse_name(&row),
                    Some((harness, agent, Some("same-thread-id")))
                );
            }
        }
        assert!(!is_client("ssh-session-123"));
        assert!(!is_client("client-thread:unknown:vm:thread"));
    }
}
