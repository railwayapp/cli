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
