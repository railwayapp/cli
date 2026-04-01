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

pub fn auto_update_log_path() -> Result<PathBuf> {
    Ok(railway_dir()?.join("auto-update.log"))
}

fn detect_target_triple() -> Result<&'static str> {
    let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("linux", "arm") => "arm-unknown-linux-musleabihf",
        ("linux", "x86") => "i686-unknown-linux-musl",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "x86") => "i686-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        // FreeBSD is recognized by the install script but the release pipeline
        // does not publish a FreeBSD asset yet, so self-update is not supported.
        (os, arch) => bail!("Unsupported platform: {os}-{arch}"),
    };
    Ok(triple)
}

const RELEASE_BASE_URL: &str = "https://github.com/railwayapp/cli/releases/download";

fn release_asset_name(version: &str, target: &str) -> String {
    let ext = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("railway-v{version}-{target}.{ext}")
}

fn release_url(version: &str, asset_name: &str) -> String {
    format!("{RELEASE_BASE_URL}/v{version}/{asset_name}")
}

fn checksums_url(version: &str) -> String {
    format!("{RELEASE_BASE_URL}/v{version}/checksums.txt")
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

/// Fetch the checksums.txt file from the release and look up the expected
/// SHA-256 hash for the given asset filename.
/// Returns `Ok(None)` if the checksums file is not published (404).
/// Returns an error if the checksums file exists but the asset is not listed
/// (indicates a malformed or incomplete release manifest).
async fn fetch_expected_checksum(
    client: &reqwest::Client,
    version: &str,
    asset_name: &str,
) -> Result<Option<String>> {
    let url = checksums_url(version);
    let response = client
        .get(&url)
        .header("User-Agent", "railwayapp")
        .send()
        .await
        .context("Failed to fetch checksums file")?;

    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        bail!("Failed to fetch checksums file: HTTP {status}");
    }

    let body = response
        .text()
        .await
        .context("Failed to read checksums response")?;

    // Format: "<hex_hash>  <filename>" (two-space separated, or tab)
    for line in body.lines() {
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next() else {
            continue;
        };
        let Some(filename) = parts.last() else {
            continue;
        };
        if filename == asset_name || filename.ends_with(&format!("/{asset_name}")) {
            return Ok(Some(hash.to_lowercase()));
        }
    }

    // checksums.txt exists but our asset is not in it — the release manifest
    // is incomplete or malformed.  Treat this as an error rather than silently
    // skipping verification.
    bail!("checksums.txt for v{version} exists but does not contain an entry for {asset_name}");
}

/// Verify the SHA-256 hash of the downloaded bytes against the expected hash.
fn verify_checksum(bytes: &[u8], expected_hex: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let computed = format!("{:x}", Sha256::digest(bytes));
    if computed != expected_hex {
        bail!("Checksum verification failed.\n  Expected: {expected_hex}\n  Got:      {computed}");
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

    // Quick check before acquiring the lock.
    if let Ok(Some(staged)) = StagedUpdate::read() {
        if staged.version == version && staged.target == target {
            return Ok(true);
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

    // Re-check after acquiring the lock — another process may have just
    // finished staging the same version.
    if let Ok(Some(staged)) = StagedUpdate::read() {
        if staged.version == version && staged.target == target {
            return Ok(true);
        }
    }

    let asset_name = release_asset_name(version, target);
    let url = release_url(version, &asset_name);

    // 120 s applies to the interactive `railway upgrade` path.  Background
    // calls from spawn_update_task are bounded by handle_update_task's 2 s
    // outer cap, which will abort the tokio task before this fires.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    // Fetch expected checksum before downloading the asset
    let expected_checksum = fetch_expected_checksum(&client, version, &asset_name).await?;

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

    if let Some(ref expected) = expected_checksum {
        verify_checksum(&bytes, expected)?;
    } else {
        // checksums.txt was not published for this release.  Proceed but warn
        // so users know integrity was not verified.
        eprintln!(
            "{} no checksums.txt found for v{version}; skipping integrity check",
            "warning:".yellow().bold()
        );
    }

    let dir = staged_update_dir()?;
    fs::create_dir_all(&dir)?;

    let bin_name = binary_name();
    let extract_and_write = || -> Result<()> {
        if target.contains("windows") {
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

    Ok(true)
}

/// Spawns a detached child process that downloads and stages the update.
/// The child runs independently of the parent — it survives after the
/// parent exits, so slow downloads are not killed by the exit timeout.
pub fn spawn_background_download(version: &str) -> Result<()> {
    // Skip forking if this version is already staged.
    if let Ok(Some(staged)) = StagedUpdate::read() {
        if staged.target == detect_target_triple()? && staged.version == version {
            return Ok(());
        }
    }

    let exe = std::env::current_exe().context("Failed to get current exe path")?;
    let log_path = auto_update_log_path()?;
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let log_file = std::fs::File::create(&log_path)?;
    let log_stderr = log_file.try_clone()?;

    let mut cmd = std::process::Command::new(exe);
    cmd.env(crate::consts::RAILWAY_STAGE_UPDATE_ENV, version)
        .stdin(std::process::Stdio::null())
        .stdout(log_file)
        .stderr(log_stderr);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    cmd.spawn()
        .context("Failed to spawn background download process")?;
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

    if !backup_path.exists() && fs::hard_link(&current_exe, &backup_path).is_err() {
        fs::copy(&current_exe, &backup_path).context("Failed to backup current binary")?;
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

        fs::rename(&tmp_path, target).context(
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

/// Try to apply a previously staged self-update.
/// Uses file locking to prevent concurrent CLI instances from racing.
/// Prints a message on success, silently does nothing otherwise.
pub fn try_apply_staged() {
    use fs2::FileExt;

    #[cfg(windows)]
    clean_old_binary();

    let staged = match StagedUpdate::read() {
        Ok(Some(s)) => s,
        _ => return,
    };

    if staged.is_stale() {
        let _ = StagedUpdate::clean();
        return;
    }

    // Reject staged binary built for a different platform (e.g. shared
    // ~/.railway directory across machines or after an arch migration).
    if detect_target_triple()
        .map(|t| t != staged.target)
        .unwrap_or(true)
    {
        let _ = StagedUpdate::clean();
        return;
    }

    // Only apply if the staged version is actually newer
    if !matches!(
        crate::util::compare_semver::compare_semver(env!("CARGO_PKG_VERSION"), &staged.version),
        std::cmp::Ordering::Less
    ) {
        let _ = StagedUpdate::clean();
        return;
    }

    let lock_path = match update_lock_path() {
        Ok(p) => p,
        Err(_) => return,
    };

    let lock_file = match std::fs::File::create(&lock_path) {
        Ok(f) => f,
        Err(_) => return,
    };

    if lock_file.try_lock_exclusive().is_err() {
        return;
    }

    match apply_staged_update() {
        Ok(version) => {
            crate::util::check_update::UpdateCheck::clear_latest();

            eprintln!(
                "{} v{} (active on next run)",
                "Auto-updated Railway CLI to".green().bold(),
                version,
            );
        }
        Err(_) => {
            // Kept for retry; STAGED_UPDATE_MAX_AGE_DAYS handles permanent failures.
        }
    }

    let _ = std::fs::remove_file(&lock_path);
}

pub async fn self_update_interactive() -> Result<()> {
    use fs2::FileExt;

    let latest_version = crate::util::check_update::check_update(true)
        .await?
        .context("You are already on the latest version")?;

    println!("{} v{}...", "Downloading".green().bold(), latest_version);

    if !download_and_stage(&latest_version).await? {
        bail!("Another update process is already running. Please try again.");
    }

    // Hold the lock across apply so a concurrent try_apply_staged() cannot
    // race us between staging and applying.
    let lock_path = update_lock_path()?;
    let lock_file =
        std::fs::File::create(&lock_path).context("Failed to create update lock file")?;
    lock_file
        .lock_exclusive()
        .context("Another update process is already running")?;

    let version = apply_staged_update()?;

    // Clear the cached pending version so the next invocation doesn't
    // re-download the version we just installed.
    crate::util::check_update::UpdateCheck::clear_latest();

    let _ = std::fs::remove_file(&lock_path);

    println!("{} v{}", "Successfully updated to".green().bold(), version);

    Ok(())
}

pub fn rollback() -> Result<()> {
    use fs2::FileExt;

    let dir = backups_dir()?;
    let current_target = detect_target_triple()?;

    // Back up the current binary first so the rollback itself can be undone.
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

    // Acquire the update lock so background auto-update processes cannot
    // stage or apply while we are replacing the binary.
    let lock_path = update_lock_path()?;
    let lock_file =
        std::fs::File::create(&lock_path).context("Failed to create update lock file")?;
    lock_file
        .lock_exclusive()
        .context("Another update process is running. Please try again.")?;

    println!("{} v{}...", "Rolling back to".yellow().bold(), version);

    let current_exe = std::env::current_exe().context("Failed to get current exe path")?;
    replace_binary(&backup_path, &current_exe)?;

    // Clean staged updates so the rolled-back binary doesn't immediately re-apply.
    let _ = StagedUpdate::clean();

    // Prune after rollback succeeds so the candidate list wasn't reduced
    // before the user picked.
    let _ = prune_backups(&dir, 3);

    let _ = std::fs::remove_file(&lock_path);

    println!("{} v{}", "Rolled back to".green().bold(), version);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_checksum_accepts_valid_hash() {
        let data = b"hello world";
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_checksum(data, expected).is_ok());
    }

    #[test]
    fn verify_checksum_rejects_invalid_hash() {
        let data = b"hello world";
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(verify_checksum(data, wrong).is_err());
    }

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
