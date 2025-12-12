use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use fs2::FileExt;

use super::ports::get_develop_dir;

pub struct DevelopSessionLock {
    _file: File,
    path: PathBuf,
}

impl DevelopSessionLock {
    /// Try to acquire exclusive lock for code services in this project.
    /// Returns Ok(lock) if acquired, Err if another session is running.
    pub fn try_acquire(project_id: &str) -> Result<Self> {
        let develop_dir = get_develop_dir(project_id);
        Self::try_acquire_at(&develop_dir)
    }

    /// Try to acquire lock at a specific directory (for testing)
    pub fn try_acquire_at(develop_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(develop_dir)?;

        let path = develop_dir.join("session.lock");
        let file = File::create(&path)?;

        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                bail!(
                    "Another develop session is already running for this project.\n\
                     Stop it with Ctrl+C before starting a new one."
                )
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for DevelopSessionLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_acquire_lock() {
        let temp = TempDir::new().unwrap();
        let lock = DevelopSessionLock::try_acquire_at(temp.path());
        assert!(lock.is_ok());
    }

    #[test]
    fn test_concurrent_lock_fails() {
        let temp = TempDir::new().unwrap();
        let _lock1 = DevelopSessionLock::try_acquire_at(temp.path()).unwrap();
        let lock2 = DevelopSessionLock::try_acquire_at(temp.path());
        match lock2 {
            Ok(_) => panic!("should fail to acquire lock"),
            Err(e) => assert!(e.to_string().contains("Another develop session")),
        }
    }

    #[test]
    fn test_lock_released_on_drop() {
        let temp = TempDir::new().unwrap();
        {
            let _lock = DevelopSessionLock::try_acquire_at(temp.path()).unwrap();
        }
        // Lock should be released after drop
        let lock2 = DevelopSessionLock::try_acquire_at(temp.path());
        assert!(lock2.is_ok());
    }
}
