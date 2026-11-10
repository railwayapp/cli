//! Public-gate conversation discovery and the transient local-client connection.
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use reqwest_websocket::Message;
use serde_json::{Value, json};
use std::time::Duration;

use super::{Connection, Target};
use crate::commands::cloud_agent::client_sessions::{self, Thread};
use crate::{client::GQLClient, config::Configs};

#[derive(Clone)]
pub(crate) struct ClientConnection {
    pub connection: Connection,
    pub agent_id: String,
    pub environment_id: String,
}

impl ClientConnection {
    pub(crate) fn new(connection: Connection, agent_id: &str, environment_id: &str) -> Self {
        Self {
            connection,
            agent_id: agent_id.into(),
            environment_id: environment_id.into(),
        }
    }

    pub(super) fn target(&self) -> Result<Target> {
        let configs = Configs::new()?;
        Ok(Target {
            client: GQLClient::new_authorized(&configs)?,
            backboard: configs.get_backboard(),
            agent_id: self.agent_id.clone(),
            environment_id: self.environment_id.clone(),
            connection: self.connection.clone(),
        })
    }

    pub(crate) fn bridge(
        &self,
        selected: impl Fn(Thread) + Send + Sync + 'static,
    ) -> Result<super::bridge::Bridge> {
        super::bridge::Bridge::start(self.target()?, selected)
    }

    pub(crate) fn args(&self, thread: Option<&str>) -> Vec<String> {
        let mut url = self.connection.url.clone();
        if let Some(thread) = thread {
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("session_id", thread)
                .finish();
            url.push(if url.contains('?') { '&' } else { '?' });
            url.push_str(&query);
        }
        vec!["--attach".into(), url]
    }

    pub(crate) async fn list(&self) -> Result<Vec<Thread>> {
        let data = self.request("list_daemon_sessions").await?;
        data["sessions"]
            .as_array()
            .context("Invalid Railway session list")?
            .iter()
            .map(parse_thread)
            .collect()
    }

    pub(crate) async fn thread(&self, id: &str) -> Result<Thread> {
        client_sessions::validate_id(id)?;
        self.list()
            .await?
            .into_iter()
            .find(|t| t.id == id)
            .context("Railway conversation no longer exists")
    }

    pub(crate) async fn delete_thread(&self, id: &str) -> Result<()> {
        // Reuse CA's deletion path: it stops the daemon's session task before
        // removing history, including a conversation that is currently live.
        let info =
            crate::commands::code::connect_info(&self.environment_id, &self.agent_id).await?;
        crate::commands::cloud_agent::remote_threads::delete(&info, "railway", id, &[]).await
    }

    async fn request(&self, command: &str) -> Result<Value> {
        tokio::time::timeout(Duration::from_secs(20), async {
            let mut socket = self.target()?.open(None).await?;
            socket
                .send(Message::Text(
                    json!({"type": command, "id": "railway-cli-history"}).to_string(),
                ))
                .await?;
            while let Some(frame) = socket.next().await {
                match frame? {
                    Message::Text(text) => {
                        let value: Value = serde_json::from_str(&text)?;
                        if value["id"] != "railway-cli-history" {
                            continue;
                        }
                        if value["success"] != true {
                            bail!("Railway conversation request failed ({command})");
                        }
                        return Ok(value["data"].clone());
                    }
                    Message::Ping(data) => socket.send(Message::Pong(data)).await?,
                    _ => {}
                }
            }
            bail!("Railway daemon disconnected during conversation discovery")
        })
        .await
        .context("Railway conversation discovery timed out")?
    }
}

pub(super) fn parse_thread(row: &Value) -> Result<Thread> {
    let id = row["id"]
        .as_str()
        .or(row["session_id"].as_str())
        .context("Railway session has no ID")?;
    client_sessions::validate_id(id)?;
    Ok(Thread {
        id: id.into(),
        title: client_sessions::title(
            row["title"]
                .as_str()
                .filter(|t| !t.trim().is_empty())
                .or(row["preview"].as_str()),
            client_sessions::NEW_THREAD,
        ),
        directory: row["cwd"]
            .as_str()
            .filter(|d| !d.is_empty())
            .context("Railway session has no directory")?
            .into(),
        created_at: row["created_at"]
            .as_i64()
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0)),
        updated_at: row["updated_at"]
            .as_i64()
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .map(|t| t.to_rfc3339())
            .unwrap_or_default(),
        state: if row["is_streaming"] == true {
            "working"
        } else {
            "idle"
        }
        .into(),
    })
}

pub(super) fn selected_thread(message: &Message) -> Option<Thread> {
    let Message::Text(text) = message else {
        return None;
    };
    let value: Value = serde_json::from_str(text).ok()?;
    if value["type"] != "response" || value["command"] != "get_state" || value["success"] != true {
        return None;
    }
    parse_thread(&value["data"]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_preserves_bridge_auth_and_selects_the_native_session() {
        let c = ClientConnection::new(
            Connection {
                url: "ws://127.0.0.1:8123/_railway/agent?token=capability".into(),
                directory: "/app/project".into(),
                client_version: "0.1.16".into(),
            },
            "agent-a",
            "env-a",
        );
        let args = c.args(Some("thread-a"));
        assert_eq!(args[0], "--attach");
        let url = url::Url::parse(&args[1]).unwrap();
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            [
                ("token".into(), "capability".into()),
                ("session_id".into(), "thread-a".into())
            ]
        );
        assert_eq!(c.args(None), ["--attach", &c.connection.url]);
    }

    #[test]
    fn remote_state_identifies_the_sidebar_thread_without_credentials() {
        let message = Message::Text(json!({"type":"response", "command":"get_state", "success":true,
            "data":{"session_id":"abc123", "cwd":"/app/project", "title":"Fix startup", "is_streaming":true}}).to_string());
        let thread = selected_thread(&message).unwrap();
        assert_eq!(thread.id, "abc123");
        assert_eq!(thread.title, "Fix startup");
        assert_eq!(thread.directory, "/app/project");
        assert_eq!(thread.state, "working");
        assert!(
            selected_thread(&Message::Text(
                json!({"type":"response", "command":"get_state", "success":false,
            "data":{"session_id":"abc123", "cwd":"/app"}})
                .to_string()
            ))
            .is_none()
        );
        assert!(parse_thread(&json!({"id":"../bad", "cwd":"/app"})).is_err());
    }
}
