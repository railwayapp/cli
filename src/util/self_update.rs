use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use colored::Colorize;

/// Maximum age for a staged update before it's considered stale and cleaned up.
const STAGED_UPDATE_MAX_AGE_DAYS: i64 = 7;

fn railway_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to get home directory")?;
    Ok(home.join(".railway"))
}

fn staged_update_dir() -> Result<PathBuf> {
    Ok(railway_dir()?.join("staged-update"))
}

fn backups_dir() -> Result<PathBuf> {
    Ok(railway_dir()?.join("backups"))
}

pub fn update_lock_path() -> Result<PathBuf> {
    Ok(railway_dir()?.join("update.lock"))
}

pub fn package_update_pid_path() -> Result<PathBuf> {
    Ok(railway_dir()?.join("package-update.pid"))
}

pub fn package_update_lock_path() -> Result<PathBuf> {
    Ok(railway_dir()?.join("package-update.lock"))
}

pub fn auto_update_log_path() -> Result<PathBuf> {
    Ok(railway_dir()?.join("auto-update.log"))
}

/// Returns the compile-time target triple of this binary, ensuring the
/// self-updater fetches the exact same ABI variant (e.g. gnu vs musl).
/// The value is set by `build.rs` via `BUILD_TARGET`.
fn detect_target_triple() -> Result<&'static str> {
    Ok(env!("BUILD_TARGET"))
}

const RELEASE_BASE_URL: &str = "https://github.com/railwayapp/cli/releases/download";

fn release_asset_name(version: &str, target: &str) -> String {
    // i686-pc-windows-gnu is cross-compiled on Linux and only ships as tar.gz.
    let ext = if target.contains("windows") && target != "i686-pc-windows-gnu" {
        "zip"
    } else {
        "tar.gz"
    };
    format!("railway-v{version}-{target}.{ext}")
}

fn release_url(version: &str, asset_name: &str) -> String {
    format!("{RELEASE_BASE_URL}/v{version}/{asset_name}")
}

fn binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "railway.exe"
    } else {
        "railway"
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StagedUpdate {
    version: String,
    target: String,
    staged_at: chrono::DateTime<chrono::Utc>,
}

impl StagedUpdate {
    fn read() -> Result<Option<Self>> {
        let path = staged_update_dir()?.join("update.json");
        match fs::read_to_string(&path) {
            Ok(contents) => {
                let update: Self = serde_json::from_str(&contents)
                    .context("Failed to parse staged update metadata")?;
                Ok(Some(update))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("Failed to read staged update metadata"),
        }
    }

    fn write(&self) -> Result<()> {
        let dir = staged_update_dir()?;
        fs::create_dir_all(&dir)?;
        let path = dir.join("update.json");
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let pid = std::process::id();
        let tmp_path = dir.join(format!("update.tmp.{pid}-{nanos}.json"));
        let contents = serde_json::to_string_pretty(self)?;
        fs::write(&tmp_path, contents)?;
        super::rename_replacing(&tmp_path, &path)?;
        Ok(())
    }

    fn clean() -> Result<()> {
        let dir = staged_update_dir()?;
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("Failed to clean staged update directory"),
        }
    }

    fn is_stale(&self) -> bool {
        let max_age = chrono::Duration::days(STAGED_UPDATE_MAX_AGE_DAYS);
        chrono::Utc::now() - self.staged_at > max_age
    }
}

/// Public entry point for cleaning staged updates (e.g., when auto-updates are disabled).
pub fn clean_staged() -> Result<()> {
    StagedUpdate::clean()
}

/// Returns the version string of a staged update waiting to be applied,
/// or `None` if nothing is staged (or the metadata is unreadable).
pub fn staged_version() -> Option<String> {
    StagedUpdate::read().ok().flatten().map(|s| s.version)
}

/// Downloads and stages the update, assuming the caller already holds the
/// update lock.  Shared by [`download_and_stage`] (background path) and
/// [`self_update_interactive`] (interactive path).
async fn download_and_stage_inner(version: &str, timeout_secs: u64) -> Result<()> {
    let target = detect_target_triple()?;

    if let Ok(Some(staged)) = StagedUpdate::read() {
        if staged.version == version && staged.target == target {
            if staged_update_dir()
                .map(|d| d.join(binary_name()).exists())
                .unwrap_or(false)
            {
                return Ok(());
            }
            // Metadata exists but binary is missing — clean and re-download.
            let _ = StagedUpdate::clean();
        }
    }

    let asset_name = release_asset_name(version, target);
    let url = release_url(version, &asset_name);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()?;

    let response = client
        .get(&url)
        .header("User-Agent", "railwayapp")
        .send()
        .await
        .context("Failed to download update")?;

    if !response.status().is_success() {
        bail!("Failed to download update: HTTP {}", response.status());
    }

    let bytes = response
        .bytes()
        .await
        .context("Failed to read update response")?;

    let dir = staged_update_dir()?;
    fs::create_dir_all(&dir)?;

    let bin_name = binary_name();
    let extract_and_write = || -> Result<()> {
        if asset_name.ends_with(".zip") {
            extract_from_zip(&bytes, bin_name, &dir)?;
        } else {
            extract_from_tar_gz(&bytes, bin_name, &dir)?;
        }

        StagedUpdate {
            version: version.to_string(),
            target: target.to_string(),
            staged_at: chrono::Utc::now(),
        }
        .write()?;

        Ok(())
    };

    if let Err(e) = extract_and_write() {
        let _ = StagedUpdate::clean();
        return Err(e);
    }

    Ok(())
}

/// Downloads the release tarball for the given version and extracts the binary
/// to the staged update directory. Cleans up on partial failure.
/// Uses file locking to prevent concurrent CLI processes from racing.
///
/// Returns `Ok(true)` when the update was staged (or was already staged for
/// this version/target).  Returns `Ok(false)` when another process holds the
/// update lock — the caller should **not** treat this as a completed update.
pub async fn download_and_stage(version: &str) -> Result<bool> {
    use fs2::FileExt;

    let target = detect_target_triple()?;

    if let Ok(Some(staged)) = StagedUpdate::read() {
        if staged.version == version && staged.target == target {
            if staged_update_dir()
                .map(|d| d.join(binary_name()).exists())
                .unwrap_or(false)
            {
                return Ok(true);
            }
            // Metadata exists but binary is missing — clean and re-download.
            let _ = StagedUpdate::clean();
        }
    }

    let lock_path = update_lock_path()?;
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_file =
        std::fs::File::create(&lock_path).context("Failed to create update lock file")?;
    if lock_file.try_lock_exclusive().is_err() {
        // Another process is already staging or applying an update.
        return Ok(false);
    }

    // Re-check after acquiring the lock: the user may have run
    // `railway autoupdate disable` while we were waiting.
    if crate::telemetry::is_auto_update_disabled() {
        return Ok(false);
    }

    download_and_stage_inner(version, 30).await?;

    Ok(true)
}

/// Spawns a detached child process that downloads and stages the update.
/// The child runs independently of the parent — it survives after the
/// parent exits, so slow downloads are not killed by the exit timeout.
pub fn spawn_background_download(version: &str) -> Result<()> {
    let exe = std::env::current_exe().context("Failed to get current exe path")?;
    let log_path = auto_update_log_path()?;

    let mut cmd = std::process::Command::new(exe);
    cmd.env(crate::consts::RAILWAY_STAGE_UPDATE_ENV, version);

    let child = super::spawn_detached(&mut cmd, &log_path)?;
    std::mem::forget(child);
    Ok(())
}

fn extract_from_tar_gz(bytes: &[u8], bin_name: &str, dest_dir: &Path) -> Result<()> {
    use flate2::read::GzDecoder;

    let decoder = GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries().context("Failed to read tar entries")? {
        let mut entry = entry.context("Failed to read tar entry")?;
        let path = entry.path().context("Failed to read entry path")?;

        if path.file_name().and_then(|n| n.to_str()) == Some(bin_name) {
            let dest_path = dest_dir.join(bin_name);
            let mut file =
                fs::File::create(&dest_path).context("Failed to create staged binary file")?;
            std::io::copy(&mut entry, &mut file).context("Failed to write staged binary")?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&dest_path, fs::Permissions::from_mode(0o755))?;
            }

            return Ok(());
        }
    }

    bail!("Binary '{bin_name}' not found in archive");
}

fn extract_from_zip(bytes: &[u8], bin_name: &str, dest_dir: &Path) -> Result<()> {
    use std::io::Cursor;

    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("Failed to read zip archive")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to read zip entry")?;
        let path = file.mangled_name();

        if path.file_name().and_then(|n| n.to_str()) == Some(bin_name) {
            let dest_path = dest_dir.join(bin_name);
            let mut out =
                fs::File::create(&dest_path).context("Failed to create staged binary file")?;
            std::io::copy(&mut file, &mut out).context("Failed to write staged binary")?;
            return Ok(());
        }
    }

    bail!("Binary '{bin_name}' not found in zip archive");
}

const BACKUP_PREFIX: &str = "railway-v";

/// Extract the version string from a backup filename.
/// Handles both `railway-v{ver}` and `railway-v{ver}_{target}[.exe]` formats.
fn backup_version(entry: &fs::DirEntry) -> String {
    let raw = entry.file_name().to_string_lossy().into_owned();
    let stem = raw
        .trim_start_matches(BACKUP_PREFIX)
        .trim_end_matches(".exe");
    match stem.split_once('_') {
        Some((ver, _)) => ver.to_string(),
        None => stem.to_string(),
    }
}

fn list_backups(dir: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(BACKUP_PREFIX))
        .collect();

    // Sort by version (oldest first) so prune_backups can drop the leading entries.
    entries.sort_by(|a, b| {
        crate::util::compare_semver::compare_semver(&backup_version(a), &backup_version(b))
    });

    Ok(entries)
}

fn backup_current_binary_no_prune() -> Result<()> {
    let current_exe = std::env::current_exe().context("Failed to get current exe path")?;
    let current_version = env!("CARGO_PKG_VERSION");
    let target = detect_target_triple()?;
    let dir = backups_dir()?;
    fs::create_dir_all(&dir)?;

    let backup_name = if cfg!(target_os = "windows") {
        format!("{BACKUP_PREFIX}{current_version}_{target}.exe")
    } else {
        format!("{BACKUP_PREFIX}{current_version}_{target}")
    };
    let backup_path = dir.join(&backup_name);

    if fs::hard_link(&current_exe, &backup_path).is_err() {
        // hard_link fails if backup already exists or across filesystems — fall back to copy.
        let _ = fs::copy(&current_exe, &backup_path);
    }

    Ok(())
}

fn backup_current_binary() -> Result<()> {
    backup_current_binary_no_prune()?;
    prune_backups(&backups_dir()?, 3)?;
    Ok(())
}

fn prune_backups(dir: &Path, keep: usize) -> Result<()> {
    let entries = list_backups(dir)?;

    if entries.len() <= keep {
        return Ok(());
    }

    let to_remove = entries.len() - keep;
    for entry in entries.into_iter().take(to_remove) {
        let _ = fs::remove_file(entry.path());
    }

    Ok(())
}

/// Cleans up leftover `.old.exe` from a previous Windows binary replacement.
#[cfg(windows)]
fn clean_old_binary() {
    if let Ok(exe) = std::env::current_exe() {
        let old_path = exe.with_extension("old.exe");
        let _ = fs::remove_file(&old_path);
    }
}

/// Atomically replaces the binary at `target` with the binary at `source`.
/// On Unix: copies to a temp file in the same directory, then renames (atomic).
/// On Windows: renames running binary to .old, copies new one in, cleans up .old on next run.
fn replace_binary(source: &Path, target: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let exe_dir = target.parent().context("Failed to get binary directory")?;
        let pid = std::process::id();
        let tmp_path = exe_dir.join(format!(".railway-tmp-{pid}"));

        fs::copy(source, &tmp_path).context("Failed to copy new binary")?;

        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o755))?;

        super::rename_replacing(&tmp_path, target).context(
            "Failed to replace binary. You may need to run with sudo or use `railway upgrade` manually.",
        )?;
    }

    #[cfg(windows)]
    {
        let old_path = target.with_extension("old.exe");
        let _ = fs::remove_file(&old_path);
        fs::rename(target, &old_path).context("Failed to rename current binary")?;
        if let Err(e) = fs::copy(source, target) {
            let _ = fs::rename(&old_path, target);
            bail!("Failed to install new binary: {e}");
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        bail!("Self-update is not supported on this platform");
    }

    Ok(())
}

/// Applies a staged update by atomically replacing the current binary.
/// Returns Ok(version) on success.
fn apply_staged_update() -> Result<String> {
    let staged = StagedUpdate::read()?.context("No staged update found")?;

    // Verify the staged binary matches the current platform.
    let current_target = detect_target_triple()?;
    if staged.target != current_target {
        StagedUpdate::clean()?;
        bail!(
            "Staged update is for {}, but this machine is {}",
            staged.target,
            current_target
        );
    }

    let staged_binary = staged_update_dir()?.join(binary_name());
    if !staged_binary.exists() {
        bail!("Staged binary not found");
    }

    backup_current_binary()?;

    let current_exe = std::env::current_exe().context("Failed to get current exe path")?;
    replace_binary(&staged_binary, &current_exe)?;

    let version = staged.version.clone();
    StagedUpdate::clean()?;

    Ok(version)
}

/// Reads and validates the staged update.  Returns `Ok(staged)` when the
/// staged binary is safe to apply, or an `Err` describing why not.
/// Cleans up the staged directory when the update is stale, wrong-platform,
/// not-newer, or skipped.
fn validate_staged() -> Result<StagedUpdate> {
    let staged = StagedUpdate::read()?.context("No staged update found")?;

    if staged.is_stale() {
        let _ = StagedUpdate::clean();
        bail!("Staged update is too old");
    }

    let current_target = detect_target_triple()?;
    if staged.target != current_target {
        let _ = StagedUpdate::clean();
        bail!(
            "Staged update is for {}, but this machine is {current_target}",
            staged.target
        );
    }

    if !matches!(
        crate::util::compare_semver::compare_semver(env!("CARGO_PKG_VERSION"), &staged.version),
        std::cmp::Ordering::Less
    ) {
        let _ = StagedUpdate::clean();
        bail!("You are already on the latest version");
    }

    if let Ok(check) = crate::util::check_update::UpdateCheck::read() {
        if check.skipped_version.as_deref() == Some(staged.version.as_str()) {
            let _ = StagedUpdate::clean();
            bail!("v{} was previously rolled back", staged.version);
        }
    }

    Ok(staged)
}

/// Try to apply a previously staged self-update.
/// Uses file locking to prevent concurrent CLI instances from racing.
/// Returns the applied version on success, `None` otherwise.
pub fn try_apply_staged() -> Option<String> {
    use fs2::FileExt;

    #[cfg(windows)]
    clean_old_binary();

    let lock_path = match update_lock_path() {
        Ok(p) => p,
        Err(_) => return None,
    };

    let lock_file = match std::fs::File::create(&lock_path) {
        Ok(f) => f,
        Err(_) => return None,
    };

    if lock_file.try_lock_exclusive().is_err() {
        return None;
    }

    // Validate after acquiring the lock so another process can't delete or
    // replace the staged binary between validation and apply.
    if validate_staged().is_err() {
        return None;
    }

    let result = match apply_staged_update() {
        Ok(version) => {
            crate::util::check_update::UpdateCheck::clear_after_update();

            eprintln!(
                "{} v{} (active on next run)",
                "Auto-updated Railway CLI to".green().bold(),
                version,
            );
            Some(version)
        }
        Err(e) => {
            if e.to_string().contains("Staged binary not found") {
                let _ = StagedUpdate::clean();
            }
            // Other errors kept for retry; STAGED_UPDATE_MAX_AGE_DAYS handles permanent failures.
            None
        }
    };

    drop(lock_file);
    result
}

pub async fn self_update_interactive() -> Result<()> {
    use fs2::FileExt;

    // Try the network check first.  If it fails and an update is already
    // staged on disk, apply that instead of surfacing a network error.
    let latest_version = match crate::util::check_update::check_update(true).await {
        Ok(Some(v)) => Some(v),
        Ok(None) => None,
        Err(_) => {
            // Network failure — fall through and try the staged update.
            None
        }
    };

    let lock_path = update_lock_path()?;
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_file =
        std::fs::File::create(&lock_path).context("Failed to create update lock file")?;
    lock_file
        .lock_exclusive()
        .context("Another update process is already running")?;

    if let Some(ref version) = latest_version {
        println!("{} v{}...", "Downloading".green().bold(), version);
        download_and_stage_inner(version, 120).await?;
    } else {
        // Falling back to a staged update — validate before applying.
        let staged = validate_staged().context(
            "You are already on the latest version (or the update check failed — check your network)",
        )?;
        println!("Applying previously downloaded v{}...", staged.version);
    }

    let version = apply_staged_update()?;

    crate::util::check_update::UpdateCheck::clear_after_update();

    drop(lock_file);

    println!("{} v{}", "Successfully updated to".green().bold(), version);

    Ok(())
}

pub fn rollback() -> Result<()> {
    use fs2::FileExt;

    // Acquire the update lock first so background auto-update processes cannot
    // stage or apply while we are building the candidate list or prompting.
    let lock_path = update_lock_path()?;
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_file =
        std::fs::File::create(&lock_path).context("Failed to create update lock file")?;
    lock_file
        .lock_exclusive()
        .context("Another update process is running. Please try again.")?;

    let dir = backups_dir()?;
    let current_target = detect_target_triple()?;

    // Back up the current binary so the rollback itself can be undone.
    // Use the no-prune variant so candidates aren't removed before the user
    // sees the picker.
    backup_current_binary_no_prune()?;

    let entries = list_backups(&dir)?;
    let current_version = env!("CARGO_PKG_VERSION");

    // Collect (version_string, path) pairs, newest-first, excluding current.
    // Backup filenames are either the old format "railway-v{ver}" or the new
    // format "railway-v{ver}_{target}".  Old-format backups (no target) are
    // assumed to match the current target since they were created locally
    // before target tracking was added.
    let candidates: Vec<(String, std::path::PathBuf)> = entries
        .iter()
        .rev()
        .filter_map(|e| {
            let raw = e.file_name().to_string_lossy().into_owned();
            let stem = raw
                .trim_start_matches(BACKUP_PREFIX)
                .trim_end_matches(".exe");

            let (ver, backup_target) = match stem.split_once('_') {
                Some((v, t)) => (v, Some(t)),
                None => (stem, None),
            };

            if ver == current_version {
                return None;
            }

            // Filter out backups built for a different architecture.
            if let Some(t) = backup_target {
                if t != current_target {
                    return None;
                }
            }

            Some((ver.to_string(), e.path()))
        })
        .collect();

    if candidates.is_empty() {
        bail!(
            "All backups match the current version (v{current_version}). Nothing to roll back to."
        );
    }

    let (version, backup_path) = if candidates.len() == 1 {
        candidates.into_iter().next().unwrap()
    } else {
        // Multiple candidates: let the user pick.
        let labels: Vec<String> = candidates.iter().map(|(v, _)| v.clone()).collect();
        let selected = inquire::Select::new("Select version to roll back to:", labels)
            .prompt()
            .context("Rollback cancelled")?;
        candidates
            .into_iter()
            .find(|(v, _)| *v == selected)
            .expect("selected label must exist in candidates")
    };

    println!("{} v{}...", "Rolling back to".yellow().bold(), version);

    let current_exe = std::env::current_exe().context("Failed to get current exe path")?;
    replace_binary(&backup_path, &current_exe)?;

    // Clean staged updates so the rolled-back binary doesn't immediately re-apply.
    let _ = StagedUpdate::clean();

    // Record the current version as skipped so auto-update doesn't
    // re-download and re-apply the version the user just rolled back from.
    // Auto-update resumes once a newer release supersedes the skipped version.
    crate::util::check_update::UpdateCheck::skip_version(current_version);

    // Prune after rollback succeeds so the candidate list wasn't reduced
    // before the user picked.
    let _ = prune_backups(&dir, 3);

    drop(lock_file);

    println!("{} v{}", "Rolled back to".green().bold(), version);
    println!(
        "Auto-updates will skip v{}. Run {} to disable all auto-updates.",
        current_version,
        "railway autoupdate disable".bold()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_backups_removes_oldest() {
        let dir = tempfile::tempdir().unwrap();

        for i in 0..5u32 {
            let path = dir.path().join(format!("railway-v1.{i}.0"));
            fs::write(&path, format!("binary-{i}")).unwrap();
        }

        prune_backups(dir.path(), 3).unwrap();

        let remaining = list_backups(dir.path()).unwrap();
        assert_eq!(remaining.len(), 3);

        let names: Vec<_> = remaining
            .iter()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(!names.contains(&"railway-v1.0.0".to_string()));
        assert!(!names.contains(&"railway-v1.1.0".to_string()));
    }

    #[test]
    fn prune_backups_noop_when_fewer_than_keep() {
        let dir = tempfile::tempdir().unwrap();

        for i in 0..2 {
            let path = dir.path().join(format!("railway-v1.{i}.0"));
            fs::write(&path, "binary").unwrap();
        }

        prune_backups(dir.path(), 3).unwrap();
        assert_eq!(list_backups(dir.path()).unwrap().len(), 2);
    }

    #[test]
    fn list_backups_ignores_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();

        fs::write(dir.path().join("railway-v1.0.0"), "binary").unwrap();
        fs::write(dir.path().join("railway-v2.0.0"), "binary").unwrap();
        fs::write(dir.path().join("unrelated.txt"), "text").unwrap();
        fs::write(dir.path().join("railway.conf"), "config").unwrap();

        assert_eq!(list_backups(dir.path()).unwrap().len(), 2);
    }
}
