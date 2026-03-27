use std::cmp::Ordering;

use anyhow::{Context, bail};
use dirs::home_dir;

use super::compare_semver::compare_semver;

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct UpdateCheck {
    pub last_update_check: Option<chrono::DateTime<chrono::Utc>>,
    pub latest_version: Option<String>,
}
impl UpdateCheck {
    pub fn write(&self) -> anyhow::Result<()> {
        let home = home_dir().context("Failed to get home directory")?;
        let path = home.join(".railway/version.json");
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap();
        let pid = std::process::id();
        // almost guaranteed no collision- can be upgraded to uuid if necessary.
        let tmp_path = path.with_extension(format!("tmp.{pid}-{nanos}.json"));
        let contents = serde_json::to_string_pretty(&self)?;
        std::fs::write(&tmp_path, contents)?;
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    }

    /// Clear the cached "new version available" notification.
    pub fn clear_latest() {
        let update = Self {
            last_update_check: Some(chrono::Utc::now()),
            latest_version: None,
        };
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
    let update = UpdateCheck::read().unwrap_or_default();

    if let Some(last_update_check) = update.last_update_check {
        if chrono::Utc::now().date_naive() == last_update_check.date_naive() && !force {
            bail!("Update check already ran today");
        }
    }

    let client = reqwest::Client::new();
    let response = client
        .get(GITHUB_API_RELEASE_URL)
        .header("User-Agent", "railwayapp")
        .send()
        .await?;
    let response = response.json::<GithubApiRelease>().await?;
    let latest_version = response.tag_name.trim_start_matches('v');

    match compare_semver(env!("CARGO_PKG_VERSION"), latest_version) {
        Ordering::Less => {
            let update = UpdateCheck {
                last_update_check: Some(chrono::Utc::now()),
                latest_version: Some(latest_version.to_owned()),
            };
            update.write()?;
            Ok(Some(latest_version.to_string()))
        }
        _ => Ok(None),
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

    // Verify the package manager binary exists
    if which::which(program).is_err() {
        bail!("Package manager '{program}' not found in PATH");
    }

    // Guard against overlapping package-manager updates using a PID file.
    // Format: "PID UNIX_TIMESTAMP".  We check whether the recorded process is
    // still alive AND the entry is recent (< 10 min).  The staleness check
    // ensures we recover on all platforms even if PID liveness detection fails.
    let pid_path = super::self_update::update_lock_path()?;
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
    let log_file = std::fs::File::create(&log_path)?;
    let log_stderr = log_file.try_clone()?;

    // Spawn in its own process group so SIGINT from the terminal doesn't
    // propagate to the child when the user hits Ctrl+C.
    let mut cmd = std::process::Command::new(program);
    cmd.args(&args).stdout(log_file).stderr(log_stderr);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let child = cmd.spawn().context(format!("Failed to spawn {program}"))?;

    // Record the child PID + timestamp so future invocations can detect an
    // in-flight update and expire stale entries.
    let now = chrono::Utc::now().timestamp();
    let _ = std::fs::write(&pid_path, format!("{} {now}", child.id()));

    Ok(())
}

/// Check whether a process with the given PID is still running.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use std::process::Command;
        // `kill -0 <pid>` checks existence without sending a signal.
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(not(unix))]
    {
        // On Windows, conservatively assume it's alive to avoid duplicates.
        // A stale PID file will be overwritten on the next successful spawn.
        let _ = pid;
        true
    }
}
