use colored::Colorize;

use crate::client::{GQLClient, post_graphql};
use crate::config::Configs;
use crate::gql::mutations::{self, cli_event_track};

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Notices {
    telemetry_notice_shown: bool,
}

impl Notices {
    fn path() -> Option<std::path::PathBuf> {
        dirs::home_dir().map(|h| h.join(".railway/notices.json"))
    }

    fn read() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn write(&self) {
        if let Some(path) = Self::path() {
            let _ = serde_json::to_string(self)
                .ok()
                .map(|contents| std::fs::write(path, contents));
        }
    }
}

pub fn show_notice_if_needed() {
    if is_telemetry_disabled() {
        return;
    }

    let notices = Notices::read();
    if notices.telemetry_notice_shown {
        return;
    }

    eprintln!(
        "{}\nYou can opt out by running `railway telemetry disable` or by setting RAILWAY_NO_TELEMETRY=1 in your environment.\n{}",
        "Railway now collects CLI usage data to improve the developer experience.".bold(),
        format!("Learn more: {}", "https://docs.railway.com/cli/telemetry").dimmed(),
    );

    Notices {
        telemetry_notice_shown: true,
    }
    .write();
}

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

fn env_var_is_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    #[serde(default)]
    pub telemetry_disabled: bool,
}

impl Preferences {
    fn path() -> Option<std::path::PathBuf> {
        dirs::home_dir().map(|h| h.join(".railway/preferences.json"))
    }

    pub fn read() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn write(&self) {
        if let Some(path) = Self::path() {
            let _ = serde_json::to_string(self)
                .ok()
                .map(|contents| std::fs::write(path, contents));
        }
    }
}

pub fn is_telemetry_disabled_by_env() -> bool {
    env_var_is_truthy("DO_NOT_TRACK") || env_var_is_truthy("RAILWAY_NO_TELEMETRY")
}

fn is_telemetry_disabled() -> bool {
    is_telemetry_disabled_by_env() || Preferences::read().telemetry_disabled
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
