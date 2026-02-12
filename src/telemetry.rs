use serde::Serialize;
use std::time::Duration;

use crate::client::GQLClient;
use crate::config::Configs;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliTrackEvent {
    pub command: String,
    pub sub_command: Option<String>,
    pub duration_ms: u64,
    pub success: bool,
    pub error_message: Option<String>,
    pub os: &'static str,
    pub arch: &'static str,
    pub cli_version: &'static str,
    pub is_ci: bool,
}

fn is_telemetry_disabled() -> bool {
    std::env::var("DO_NOT_TRACK")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub async fn send(event: CliTrackEvent) {
    if is_telemetry_disabled() {
        return;
    }

    let configs = match Configs::new() {
        Ok(c) => c,
        Err(_) => return,
    };

    let client = match GQLClient::new_authorized(&configs) {
        Ok(c) => c,
        Err(_) => return,
    };

    let url = configs.get_backboard();
    let body = serde_json::json!({
        "query": "mutation CliEventTrack($input: CliEventTrackInput!) { cliEventTrack(input: $input) }",
        "variables": { "input": event }
    });

    let _ = client
        .post(&url)
        .json(&body)
        .timeout(Duration::from_secs(3))
        .send()
        .await;
}
