use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::Configs;
use crate::telemetry::{self, CliTrackEvent};

/// Stage timings collected during one flow, flushed by [`flush_stages`].
///
/// Buffered rather than sent as they happen because [`telemetry::send`] awaits
/// a real HTTP POST — nine of those inline would add roughly a second to the
/// launch this is meant to measure. Recording is a mutex push; the network cost
/// is paid once, detached, after the flow has finished.
static STAGES: Mutex<Vec<StageTiming>> = Mutex::new(Vec::new());

struct StageTiming {
    stage: String,
    duration_ms: u64,
    success: bool,
}

/// `RAILWAY_STAGE_TIMING=1` prints the breakdown to stderr as well as reporting
/// it. A debugging aid for latency work — note it writes plainly, so under the
/// `railway ca` TUI it lands on top of the frame.
fn timing_to_stderr() -> bool {
    std::env::var("RAILWAY_STAGE_TIMING").is_ok_and(|v| v == "1" || v == "true")
}

/// Record a stage measured by the caller itself, for spans that don't wrap
/// cleanly in [`timed_for`] — the sub-legs inside a wake or create wait, where
/// the timing brackets a loop rather than one future.
pub fn record_stage(stage: &str, duration: Duration, success: bool) {
    record(stage, duration, success);
}

/// Detached sends in flight, so a process about to exit can give them a
/// bounded window to finish. Without this, launches that end quickly (the
/// `railway code -- <cmd>` exec path) dropped their spawned events and the
/// funnel silently under-counted scripted usage.
static DETACHED: Mutex<Vec<tokio::task::JoinHandle<()>>> = Mutex::new(Vec::new());

/// Spawn a telemetry send off the caller's path, remembering the handle for
/// [`drain_detached`]. The spawn itself never blocks anything.
pub fn spawn_detached(fut: impl std::future::Future<Output = ()> + Send + 'static) {
    let handle = tokio::spawn(fut);
    if let Ok(mut detached) = DETACHED.lock() {
        detached.push(handle);
    }
}

/// Give in-flight detached sends up to `limit` to finish — called AFTER the
/// user's work is done (session ended, command finished), never on the
/// launch path. Sends that outlast the budget are abandoned, same as before.
pub async fn drain_detached(limit: Duration) {
    let handles = match DETACHED.lock() {
        Ok(mut guard) => std::mem::take(&mut *guard),
        Err(_) => return,
    };
    let deadline = Instant::now() + limit;
    for handle in handles {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        let _ = tokio::time::timeout(remaining, handle).await;
    }
}

/// Whether `RAILWAY_STAGE_TIMING` diagnostics are on, for callers that want to
/// print per-round detail (probe cadence, poll counts) beyond the stage sums.
pub fn timing_diagnostics() -> bool {
    timing_to_stderr()
}

fn record(stage: &str, duration: Duration, success: bool) {
    let timing = StageTiming {
        stage: stage.to_string(),
        duration_ms: duration.as_millis() as u64,
        success,
    };
    if let Ok(mut stages) = STAGES.lock() {
        stages.push(timing);
    }
}

/// Time `fut`, record it, and report a failure exactly as [`track_for`] does.
///
/// The timing half is why this exists: `track_for` takes an already-awaited
/// `Result`, so by the time it is called the stage has run and its duration is
/// gone. Successful launches were invisible — every stage wrapper in the flow
/// fired only on failure, which left no way to see where a slow-but-working
/// launch spent its time except by reading a terminal capture by hand.
pub async fn timed_for<T>(
    command: &str,
    stage: &str,
    fut: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let started = Instant::now();
    let result = fut.await;
    let elapsed = started.elapsed();
    record(stage, elapsed, result.is_ok());
    if let Err(ref e) = result {
        report_failure_timed(command, stage, &format!("{e}"), elapsed.as_millis() as u64).await;
    }
    result
}

/// Send everything [`timed_for`] collected, then clear it.
///
/// Detached (`tokio::spawn`, same as the auth and session events) so the
/// reporting never lands on the caller's critical path. Telemetry that is lost
/// because the process exited first is the intended trade: this measures a flow
/// whose whole point is how quickly it finishes.
pub fn flush_stages(command: &'static str) {
    let stages = match STAGES.lock() {
        Ok(mut guard) => std::mem::take(&mut *guard),
        Err(_) => return,
    };
    if stages.is_empty() {
        return;
    }
    if timing_to_stderr() {
        let total: u64 = stages.iter().map(|s| s.duration_ms).sum();
        let mut line = format!("[{command} timing] total_tracked={total}ms");
        for s in &stages {
            let mark = if s.success { "" } else { "!" };
            line.push_str(&format!(" {}{}={}ms", s.stage, mark, s.duration_ms));
        }
        eprintln!("{line}");
    }
    for s in stages {
        // Failures already reported themselves, with the same duration.
        if !s.success {
            continue;
        }
        spawn_detached(telemetry::send(CliTrackEvent {
            command: command.to_string(),
            sub_command: Some(format!("stage_{}", s.stage)),
            success: true,
            error_message: None,
            duration_ms: s.duration_ms,
            cli_version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            is_ci: Configs::env_is_ci(),
        }));
    }
}

/// Report an SSH operation failure at the given stage.
///
/// Fires in addition to the generic failure event emitted by the `commands!`
/// macro: the macro tells us *the `ssh` command* failed; this event tells us
/// *which stage* failed. Use lowercase_snake_case stage names; they appear
/// in telemetry as `sub_command = "stage_<name>_failed"`.
pub async fn report_failure(stage: &str, message: &str) {
    report_failure_for("ssh", stage, message).await;
}

/// Like [`report_failure`] but under an arbitrary command namespace, so other
/// SSH-backed commands (e.g. `sandbox ssh`) land in the same stage-failure
/// dashboards with their own command tag.
pub async fn report_failure_for(command: &str, stage: &str, message: &str) {
    report_failure_timed(command, stage, message, 0).await;
}

/// [`report_failure_for`] with the stage's measured duration. Same event, so
/// existing stage-failure dashboards are unaffected — `duration_ms` simply
/// carries a real number now instead of always zero, which is what tells a
/// stage that failed fast apart from one that hung until something gave up.
pub async fn report_failure_timed(command: &str, stage: &str, message: &str, duration_ms: u64) {
    let truncated = telemetry::truncate_message(message);

    telemetry::send(CliTrackEvent {
        command: command.to_string(),
        sub_command: Some(format!("stage_{stage}_failed")),
        success: false,
        error_message: Some(truncated),
        duration_ms,
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
    track_for("ssh", stage, result).await
}

/// [`track`] under an arbitrary command namespace.
pub async fn track_for<T>(
    command: &str,
    stage: &str,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    if let Err(ref e) = result {
        report_failure_for(command, stage, &format!("{e}")).await;
    }
    result
}
