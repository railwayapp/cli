use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize)]
pub struct ClientMessage {
    pub r#type: String,
    pub payload: ClientPayload,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ClientPayload {
    Data {
        data: String,
    },
    WindowSize {
        cols: u16,
        rows: u16,
    },
    Signal {
        signal: u8,
    },
    Command {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    InitShell {
        shell: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
pub struct ServerMessage {
    pub r#type: String,
    pub payload: ServerPayload,
}

#[derive(Debug, Deserialize)]
pub struct ServerPayload {
    #[serde(default)]
    pub data: DataPayload,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub code: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum DataPayload {
    String(String),
    Buffer { data: Vec<u8> },
    Empty {},
}

impl Default for DataPayload {
    fn default() -> Self {
        DataPayload::Empty {}
    }
}
