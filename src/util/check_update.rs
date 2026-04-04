use std::cmp::Ordering;

use anyhow::{Context, bail};
use dirs::home_dir;

use super::compare_semver::compare_semver;

/// Best-effort write — logs a warning on failure but does not propagate.
/// Used by cache mutation methods where a write failure is non-fatal.
fn try_write(update: &UpdateCheck) {
    if let Err(e) = update.write() {
        eprintln!("warning: failed to write update cache: {e}");
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct UpdateCheck {
    pub last_update_check: Option<chrono::DateTime<chrono::Utc>>,
    pub latest_version: Option<String>,
    /// Number of consecutive download failures for the cached version.
    /// After 3 failures the version is cleared to force a fresh API check.
    #[serde(default)]
    pub download_failures: u32,
    /// Version the user rolled back from.  Auto-update skips this version
    /// and resumes normally once a newer release is published.
    #[serde(default)]
    pub skipped_version: Option<String>,
    /// Timestamp of the last package-manager spawn.  We only re-spawn if
    /// this is older than 1 hour, preventing rapid-fire retries when
    /// multiple CLI invocations happen before the update finishes.
    #[serde(default)]
    pub last_package_manager_spawn: Option<chrono::DateTime<chrono::Utc>>,
}
impl UpdateCheck {
    fn has_stale_latest_version(&self) -> bool {
        self.latest_version
            .as_deref()
            .map(|latest| {
                !matches!(
                    compare_semver(env!("CARGO_PKG_VERSION"), latest),
                    Ordering::Less
                )
            })
            .unwrap_or(false)
    }

    fn clear_latest_fields(&mut self) {
        self.latest_version = None;
        self.download_failures = 0;
        self.last_package_manager_spawn = None;
        self.last_update_check = None;
    }

    pub fn write(&self) -> anyhow::Result<()> {
        let home = home_dir().context("Failed to get home directory")?;
        let path = home.join(".railway/version.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let pid = std::process::id();
        let tmp_path = path.with_extension(format!("tmp.{pid}-{nanos}.json"));
        let contents = serde_json::to_string_pretty(&self)?;
        std::fs::write(&tmp_path, contents)?;
        super::rename_replacing(&tmp_path, &path)?;
        Ok(())
    }

    /// Update the check timestamp, optionally preserving (or clearing) the
    /// cached pending version.  Resets the failure counter.
    /// Preserves `skipped_version` so a rollback skip survives cache updates.
    pub fn persist_latest(version: Option<&str>) {
        let mut update = Self::read().unwrap_or_default();
        update.last_update_check = Some(chrono::Utc::now());
        // Reset package-manager spawn gate when the target version changes
        // so the new version gets an immediate attempt.
        if update.latest_version.as_deref() != version {
            update.last_package_manager_spawn = None;
        }
        update.latest_version = version.map(String::from);
        update.download_failures = 0;
        try_write(&update);
    }

    /// Read the cached update state and clear any pending version that is no
    /// longer ahead of the currently running binary. This keeps commands like
    /// `autoupdate status` and `autoupdate skip` aligned with actual pending
    /// state even when they bypass the normal startup cleanup path.
    pub fn read_normalized() -> Self {
        let mut update = Self::read().unwrap_or_default();
        if update.has_stale_latest_version() {
            update.clear_latest_fields();
            try_write(&update);
        }
        update
    }

    /// Record a version to skip during auto-update (set after rollback).
    /// Also clears `last_update_check` so the next invocation performs a
    /// fresh API check and can discover a newer release immediately.
    pub fn skip_version(version: &str) {
        let mut update = Self::read().unwrap_or_default();
        update.skipped_version = Some(version.to_string());
        update.last_package_manager_spawn = None;
        update.last_update_check = None;
        try_write(&update);
    }

    /// Reset cached update state after a successful upgrade or auto-apply.
    /// Clears both the pending version notification and any rollback skip
    /// in a single read-write cycle.
    pub fn clear_after_update() {
        let mut update = Self::read().unwrap_or_default();
        update.last_update_check = Some(chrono::Utc::now());
        update.latest_version = None;
        update.download_failures = 0;
        update.last_package_manager_spawn = None;
        update.skipped_version = None;
        try_write(&update);
    }

    /// Max consecutive download failures before clearing the cached version.
    const MAX_DOWNLOAD_FAILURES: u32 = 3;

    /// Record a failed download attempt.  After [`Self::MAX_DOWNLOAD_FAILURES`]
    /// consecutive failures the cached pending version is cleared so the next
    /// invocation re-checks the GitHub API instead of retrying a potentially
    /// stale version.
    pub fn record_download_failure() {
        let mut update = Self::read().unwrap_or_default();
        update.download_failures += 1;
        if update.download_failures >= Self::MAX_DOWNLOAD_FAILURES {
            update.latest_version = None;
            update.last_update_check = None;
            update.download_failures = 0;
        }
        try_write(&update);
    }

    /// Record that a package-manager update was just spawned.
    pub fn record_package_manager_spawn() {
        let mut update = Self::read().unwrap_or_default();
        update.last_package_manager_spawn = Some(chrono::Utc::now());
        try_write(&update);
    }

    /// Returns `true` if enough time has passed since the last package-manager
    /// spawn to allow another attempt (or if no spawn has been recorded).
    pub fn should_spawn_package_manager() -> bool {
        Self::read()
            .map(|u| match u.last_package_manager_spawn {
                Some(t) => (chrono::Utc::now() - t) >= chrono::Duration::hours(1),
                None => true,
            })
            .unwrap_or(true)
    }

    pub fn read() -> anyhow::Result<Self> {
        let home = home_dir().context("Failed to get home directory")?;
        let path = home.join(".railway/version.json");
        let contents =
            std::fs::read_to_string(&path).context("Failed to read update check file")?;
        serde_json::from_str::<Self>(&contents).context("Failed to parse update check file")
    }
}
#[derive(serde::Deserialize)]
struct GithubApiRelease {
    tag_name: String,
}

const GITHUB_API_RELEASE_URL: &str = "https://api.github.com/repos/railwayapp/cli/releases/latest";
pub async fn check_update(force: bool) -> anyhow::Result<Option<String>> {
    let update = UpdateCheck::read().unwrap_or_default();

    if let Some(last_update_check) = update.last_update_check {
        // Dates are compared in UTC; a check near midnight local time may
        // occasionally fire twice, but that is harmless.
        if (chrono::Utc::now() - last_update_check) < chrono::Duration::hours(12) && !force {
            return Ok(None);
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let response = client
        .get(GITHUB_API_RELEASE_URL)
        .header("User-Agent", "railwayapp")
        .send()
        .await?;
    let response = response.json::<GithubApiRelease>().await?;
    let latest_version = response.tag_name.trim_start_matches('v');

    match compare_semver(env!("CARGO_PKG_VERSION"), latest_version) {
        Ordering::Less => {
            // Re-read state from disk so we don't overwrite fields that
            // were changed while the network request was in flight (e.g.
            // `skipped_version` set by a concurrent rollback).
            let mut fresh = UpdateCheck::read().unwrap_or_default();
            // Don't arm the daily gate when the latest release is the version
            // the user rolled back from — keep checking so a fix release
            // published shortly after is discovered promptly.
            if fresh.skipped_version.as_deref() != Some(latest_version) {
                fresh.last_update_check = Some(chrono::Utc::now());
            }
            // Reset package-manager spawn gate when a genuinely new version
            // appears so it gets an immediate attempt.
            if fresh.latest_version.as_deref() != Some(latest_version) {
                fresh.last_package_manager_spawn = None;
            }
            fresh.latest_version = Some(latest_version.to_owned());
            fresh.download_failures = 0;
            fresh.write()?;
            Ok(Some(latest_version.to_string()))
        }
        _ => {
            // Record the check time so we don't re-check on every invocation.
            UpdateCheck::persist_latest(None);
            Ok(None)
        }
    }
}

/// Spawns a fully detached package manager process to update the CLI.
/// Used for npm, Bun, and Scoop installs where the package manager is fast.
/// The child process runs independently — if the update succeeds, the next
/// CLI invocation will be the new version and the "new version available"
/// notification will stop appearing.
pub fn spawn_package_manager_update(
    method: super::install_method::InstallMethod,
) -> anyhow::Result<()> {
    let (program, args) = method
        .package_manager_command()
        .context("No package manager command for this install method")?;

    if which::which(program).is_err() {
        bail!("Package manager '{program}' not found in PATH");
    }

    // Acquire a file lock to serialize the PID-check-spawn-write sequence,
    // preventing two concurrent invocations from both launching an updater.
    use fs2::FileExt;

    let lock_path = super::self_update::package_update_lock_path()?;
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_file =
        std::fs::File::create(&lock_path).context("Failed to create package-update lock file")?;
    lock_file
        .try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("Another update process is starting. Please try again."))?;

    // Re-check after acquiring the lock: the user may have run
    // `railway autoupdate disable` while we were waiting.
    if crate::telemetry::is_auto_update_disabled() {
        bail!("Auto-updates were disabled while waiting for lock");
    }

    // Only spawn once per hour to avoid rapid-fire retries when multiple
    // CLI invocations happen before the update finishes.
    if !UpdateCheck::should_spawn_package_manager() {
        bail!("Package-manager update was spawned recently; waiting before retrying");
    }

    // Guard against an already-running updater.
    let pid_path = super::self_update::package_update_pid_path()?;
    if let Some(pid) = is_background_update_running(&pid_path) {
        bail!("Another update process (pid {pid}) is already running");
    }

    let log_path = super::self_update::auto_update_log_path()?;

    let mut cmd = std::process::Command::new(program);
    cmd.args(&args);

    let child = super::spawn_detached(&mut cmd, &log_path)?;
    let child_pid = child.id();
    // Intentionally leak the Child handle — we never wait on the detached
    // process.  On Unix this is harmless; on Windows it leaks a HANDLE,
    // which is acceptable for a single short-lived spawn per invocation.
    std::mem::forget(child);

    // Record the child PID + timestamp so future invocations can detect an
    // in-flight update and expire stale entries.
    let now = chrono::Utc::now().timestamp();
    let _ = std::fs::write(&pid_path, format!("{child_pid} {now}"));

    // Record spawn time so we don't re-spawn within the next hour.
    UpdateCheck::record_package_manager_spawn();

    // Lock is released on drop after the PID file is written.

    Ok(())
}

/// Maximum age in seconds for a PID file entry before it's considered stale.
const PID_STALENESS_TTL_SECS: i64 = 600;

/// Parse a PID file containing `"{pid} {timestamp}"`.
pub fn parse_pid_file(contents: &str) -> Option<(u32, i64)> {
    let mut parts = contents.split_whitespace();
    let pid = parts.next()?.parse().ok()?;
    let ts = parts.next()?.parse().ok()?;
    Some((pid, ts))
}

/// Returns `true` if a background package-manager update is currently running,
/// based on the PID file at the given path.
pub fn is_background_update_running(pid_path: &std::path::Path) -> Option<u32> {
    let contents = std::fs::read_to_string(pid_path).ok()?;
    let (pid, ts) = parse_pid_file(&contents)?;
    let age_secs = chrono::Utc::now().timestamp().saturating_sub(ts);
    if age_secs < PID_STALENESS_TTL_SECS && is_pid_alive(pid) {
        Some(pid)
    } else {
        None
    }
}

/// Check whether a process with the given PID is still running.
pub fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        // Signal 0 checks existence without delivering a signal.
        // EPERM means the process exists but we lack permission to signal it.
        matches!(
            kill(Pid::from_raw(pid as i32), None),
            Ok(()) | Err(nix::errno::Errno::EPERM)
        )
    }
    #[cfg(windows)]
    {
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::processthreadsapi::{GetExitCodeProcess, OpenProcess};
        use winapi::um::winnt::PROCESS_QUERY_INFORMATION;
        // GetExitCodeProcess returns STILL_ACTIVE (259) while the process runs.
        const STILL_ACTIVE: u32 = 259;
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid);
            if handle.is_null() {
                // Process doesn't exist or we have no permission to query it.
                return false;
            }
            let mut exit_code: u32 = 0;
            let ok = GetExitCodeProcess(handle, &mut exit_code as *mut u32 as *mut _) != 0;
            CloseHandle(handle);
            ok && exit_code == STILL_ACTIVE
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Conservative fallback for other platforms (e.g. FreeBSD): assume
        // alive and let the 10-minute staleness TTL expire the entry.
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next_version(version: &str) -> String {
        let mut parts = version
            .split('-')
            .next()
            .unwrap_or(version)
            .split('.')
            .map(|part| part.parse::<u8>().unwrap_or(0))
            .collect::<Vec<_>>();
        parts.resize(3, 0);

        for idx in (0..parts.len()).rev() {
            if parts[idx] < u8::MAX {
                parts[idx] += 1;
                for part in parts.iter_mut().skip(idx + 1) {
                    *part = 0;
                }
                return format!("{}.{}.{}", parts[0], parts[1], parts[2]);
            }
        }

        "255.255.255-rc.1".to_string()
    }

    #[test]
    fn stale_latest_version_is_detected_and_cleared() {
        let mut update = UpdateCheck {
            last_update_check: Some(chrono::Utc::now()),
            latest_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            download_failures: 2,
            skipped_version: Some("0.1.0".to_string()),
            last_package_manager_spawn: Some(chrono::Utc::now()),
        };

        assert!(update.has_stale_latest_version());

        update.clear_latest_fields();

        assert!(update.latest_version.is_none());
        assert_eq!(update.download_failures, 0);
        assert!(update.last_package_manager_spawn.is_none());
        assert!(update.last_update_check.is_none());
        assert_eq!(update.skipped_version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn newer_latest_version_is_not_stale() {
        let update = UpdateCheck {
            latest_version: Some(next_version(env!("CARGO_PKG_VERSION"))),
            ..Default::default()
        };

        assert!(!update.has_stale_latest_version());
    }
}
