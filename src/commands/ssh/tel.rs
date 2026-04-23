use crate::config::Configs;
use crate::telemetry::{self, CliTrackEvent};

/// Report an SSH operation failure at the given stage.
///
/// Fires in addition to the generic failure event emitted by the `commands!`
/// macro: the macro tells us *the `ssh` command* failed; this event tells us
/// *which stage* failed. Use lowercase_snake_case stage names; they appear
/// in telemetry as `sub_command = "stage_<name>_failed"`.
pub async fn report_failure(stage: &str, message: &str) {
    let mut truncated = message.to_string();
    if truncated.len() > 256 {
        truncated.truncate(256);
    }

    telemetry::send(CliTrackEvent {
        command: "ssh".to_string(),
        sub_command: Some(format!("stage_{stage}_failed")),
        success: false,
        error_message: Some(truncated),
        duration_ms: 0,
        cli_version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        is_ci: Configs::env_is_ci(),
    })
    .await;
}

/// On `Err`, fire a stage-tagged failure event and pass the error through
/// unchanged. Intended to wrap each step of an SSH flow so failures are
/// categorized without replacing the existing `?`-propagation.
pub async fn track<T>(stage: &str, result: anyhow::Result<T>) -> anyhow::Result<T> {
    if let Err(ref e) = result {
        report_failure(stage, &format!("{e}")).await;
    }
    result
}
