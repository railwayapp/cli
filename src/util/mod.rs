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
/// On Windows it fails when the destination exists, so we remove it first.
pub fn rename_replacing(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let _ = std::fs::remove_file(to);
    }
    std::fs::rename(from, to)
}
