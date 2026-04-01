use std::cmp::Ordering;

use anyhow::{Context, bail};
use dirs::home_dir;

use super::compare_semver::compare_semver;

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
}
impl UpdateCheck {
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
        update.latest_version = version.map(String::from);
        update.download_failures = 0;
        let _ = update.write();
    }

    /// Clear the cached "new version available" notification.
    pub fn clear_latest() {
        Self::persist_latest(None);
    }

    /// Record a version to skip during auto-update (set after rollback).
    pub fn skip_version(version: &str) {
        let mut update = Self::read().unwrap_or_default();
        update.skipped_version = Some(version.to_string());
        let _ = update.write();
    }

    /// Clear the rollback skip so all future versions are eligible again.
    pub fn clear_skipped_version() {
        let mut update = Self::read().unwrap_or_default();
        update.skipped_version = None;
        let _ = update.write();
    }

    /// Reset cached update state after a successful upgrade or auto-apply.
    /// Clears both the pending version notification and any rollback skip
    /// in a single read-write cycle.
    pub fn clear_after_update() {
        let mut update = Self::read().unwrap_or_default();
        update.last_update_check = Some(chrono::Utc::now());
        update.latest_version = None;
        update.download_failures = 0;
        update.skipped_version = None;
        let _ = update.write();
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
        let _ = update.write();
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
    let mut update = UpdateCheck::read().unwrap_or_default();

    if let Some(last_update_check) = update.last_update_check {
        // Dates are compared in UTC; a check near midnight local time may
        // occasionally fire twice, but that is harmless.
        if chrono::Utc::now().date_naive() == last_update_check.date_naive() && !force {
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
            update.last_update_check = Some(chrono::Utc::now());
            update.latest_version = Some(latest_version.to_owned());
            update.download_failures = 0;
            update.write()?;
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

    // Guard against an already-running updater: PID file with a 10-minute staleness TTL.
    let pid_path = super::self_update::package_update_pid_path()?;
    if let Ok(contents) = std::fs::read_to_string(&pid_path) {
        let parts: Vec<&str> = contents.split_whitespace().collect();
        if let (Some(pid_str), Some(ts_str)) = (parts.first(), parts.get(1)) {
            if let (Ok(pid), Ok(ts)) = (pid_str.parse::<u32>(), ts_str.parse::<i64>()) {
                let now = chrono::Utc::now().timestamp();
                let age_secs = now.saturating_sub(ts);
                if age_secs < 600 && is_pid_alive(pid) {
                    bail!("Another update process (pid {pid}) is already running");
                }
            }
        }
    }

    let log_path = super::self_update::auto_update_log_path()?;

    let mut cmd = std::process::Command::new(program);
    cmd.args(&args);

    let child = super::spawn_detached(&mut cmd, &log_path)?;

    // Record the child PID + timestamp so future invocations can detect an
    // in-flight update and expire stale entries.
    let now = chrono::Utc::now().timestamp();
    let _ = std::fs::write(&pid_path, format!("{} {now}", child.id()));

    // Lock is released on drop after the PID file is written.

    Ok(())
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
