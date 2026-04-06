pub mod check_update;
pub mod compare_semver;
pub mod install_method;
pub mod logs;
pub mod progress;
pub mod prompt;
pub mod retry;
pub mod self_update;
pub mod time;
pub mod two_factor;
pub mod watcher;

/// Spawns a command in a fully detached process group so it survives after the
/// parent exits and Ctrl+C does not propagate.  stdout/stderr are redirected to
/// the given log file.
pub fn spawn_detached(
    cmd: &mut std::process::Command,
    log_path: &std::path::Path,
) -> anyhow::Result<std::process::Child> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log_file = std::fs::File::create(log_path)?;
    let log_stderr = log_file.try_clone()?;

    cmd.stdin(std::process::Stdio::null())
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

    cmd.spawn().map_err(Into::into)
}

/// Atomically writes `contents` to `path` via a temp file + rename.
/// The temp filename includes PID and nanosecond timestamp to avoid
/// collisions between concurrent processes.
pub fn write_atomic(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pid = std::process::id();
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let tmp_path = path.with_extension(format!("tmp.{pid}-{nanos}.json"));
    std::fs::write(&tmp_path, contents)?;
    rename_replacing(&tmp_path, path)?;
    Ok(())
}

/// Renames `from` to `to`, overwriting `to` if it already exists.
/// On Unix `std::fs::rename` already replaces the destination atomically.
/// On Windows we use `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` for an
/// atomic single-syscall replace.
pub fn rename_replacing(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    #[cfg(not(windows))]
    {
        std::fs::rename(from, to)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use winapi::um::winbase::{MOVEFILE_REPLACE_EXISTING, MoveFileExW};

        fn to_wide(path: &std::path::Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }

        let from_wide = to_wide(from);
        let to_wide = to_wide(to);
        let ret = unsafe {
            MoveFileExW(
                from_wide.as_ptr(),
                to_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING,
            )
        };
        if ret == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
