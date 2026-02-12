use crate::client::{GQLClient, post_graphql};
use crate::config::Configs;
use crate::gql::mutations::{self, cli_event_track};

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

    let vars = cli_event_track::Variables {
        input: cli_event_track::CliEventTrackInput {
            command: event.command,
            sub_command: event.sub_command,
            duration_ms: event.duration_ms as i64,
            success: event.success,
            error_message: event.error_message,
            os: event.os.to_string(),
            arch: event.arch.to_string(),
            cli_version: event.cli_version.to_string(),
            is_ci: event.is_ci,
        },
    };

    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        post_graphql::<mutations::CliEventTrack, _>(&client, configs.get_backboard(), vars),
    )
    .await;
}
