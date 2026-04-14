use std::time::Duration;

use anyhow::{Result, bail};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};

use crate::{
    client::auth_failure_error, commands::Environment, config::Configs, consts,
    errors::RailwayError,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub project_id: String,
    pub environment_id: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    Metadata {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "streamId")]
        stream_id: String,
    },
    Chunk {
        text: String,
    },
    ToolCallReady {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        args: serde_json::Value,
    },
    ToolExecutionComplete {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        result: serde_json::Value,
        #[serde(rename = "isError")]
        is_error: bool,
    },
    Error {
        message: String,
    },
    Aborted {
        #[serde(default)]
        reason: Option<String>,
    },
    WorkflowCompleted {
        #[serde(rename = "completedAt")]
        completed_at: String,
    },
}

pub fn get_chat_url(configs: &Configs) -> String {
    format!("https://backboard.{}/api/v1/chat", configs.get_host())
}

/// Build an HTTP client for the chat API.
///
/// The chat endpoint requires user OAuth tokens — project access tokens
/// (`RAILWAY_TOKEN`) are not supported. We skip project tokens and only
/// use the user's OAuth bearer token.
pub fn build_chat_client(configs: &Configs) -> Result<Client, RailwayError> {
    let mut headers = HeaderMap::new();
    if let Some(token) = configs.get_railway_auth_token() {
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
    } else {
        return Err(RailwayError::Unauthorized);
    }
    headers.insert(
        "x-source",
        HeaderValue::from_static(consts::get_user_agent()),
    );
    let client = Client::builder()
        .danger_accept_invalid_certs(matches!(Configs::get_environment_id(), Environment::Dev))
        .user_agent(consts::get_user_agent())
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(30))
        // No overall timeout — SSE streams are long-lived
        .build()
        .unwrap();
    Ok(client)
}

pub async fn stream_chat(
    client: &Client,
    url: &str,
    request: &ChatRequest,
    mut on_event: impl FnMut(ChatEvent),
) -> Result<()> {
    let mut response = client
        .post(url)
        .header("Accept", "text/event-stream")
        .header("Content-Type", "application/json")
        .json(request)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        match status.as_u16() {
            401 | 403 => return Err(auth_failure_error().into()),
            429 => return Err(RailwayError::Ratelimited.into()),
            _ => {
                let body = response.text().await.unwrap_or_default();
                bail!("Chat request failed ({}): {}", status, body);
            }
        }
    }

    let mut buffer = String::new();
    let mut current_event_type = String::new();
    let mut current_data = String::new();

    while let Some(chunk) = response.chunk().await? {
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].trim_end_matches('\r').to_string();
            buffer = buffer[line_end + 1..].to_string();

            if line.is_empty() {
                // Empty line signals end of SSE event
                if !current_data.is_empty() {
                    if let Some(event) = parse_sse_event(&current_event_type, &current_data) {
                        on_event(event);
                    }
                    current_event_type.clear();
                    current_data.clear();
                }
            } else if let Some(value) = line.strip_prefix("event: ") {
                current_event_type = value.to_string();
            } else if let Some(value) = line.strip_prefix("data: ") {
                if !current_data.is_empty() {
                    current_data.push('\n');
                }
                current_data.push_str(value);
            }
            // Ignore comments (lines starting with :) and unknown fields
        }
    }

    Ok(())
}

fn parse_sse_event(event_type: &str, data: &str) -> Option<ChatEvent> {
    match event_type {
        "metadata" => {
            serde_json::from_str(data)
                .ok()
                .map(|v: serde_json::Value| ChatEvent::Metadata {
                    thread_id: v["threadId"].as_str().unwrap_or_default().to_string(),
                    stream_id: v["streamId"].as_str().unwrap_or_default().to_string(),
                })
        }
        "chunk" => serde_json::from_str(data)
            .ok()
            .map(|v: serde_json::Value| ChatEvent::Chunk {
                text: v["text"].as_str().unwrap_or_default().to_string(),
            }),
        "tool_call_ready" => serde_json::from_str(data).ok(),
        "tool_execution_complete" => serde_json::from_str(data).ok(),
        "error" => serde_json::from_str(data)
            .ok()
            .map(|v: serde_json::Value| ChatEvent::Error {
                message: v["error"]
                    .as_str()
                    .or_else(|| v["message"].as_str())
                    .unwrap_or("Unknown error")
                    .to_string(),
            }),
        "aborted" => {
            serde_json::from_str(data)
                .ok()
                .map(|v: serde_json::Value| ChatEvent::Aborted {
                    reason: v["reason"].as_str().map(|s| s.to_string()),
                })
        }
        "workflow_completed" => serde_json::from_str(data).ok(),
        // Ignore events we don't need to surface: started, tool_call_streaming_start,
        // tool_call_delta, tool_execution_start, tool_output_delta, step_finish,
        // completed, subagent_start, subagent_complete
        _ => None,
    }
}
