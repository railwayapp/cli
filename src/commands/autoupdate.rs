use super::*;
use crate::config::Configs;
use crate::telemetry::{Preferences, is_auto_update_disabled_by_env};
use crate::util::check_update::UpdateCheck;
use crate::util::install_method::InstallMethod;

/// Manage auto-update preferences
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser)]
enum Commands {
    /// Enable automatic updates
    Enable,
    /// Disable automatic updates
    Disable,
    /// Show current auto-update status
    Status,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Commands::Enable => {
            let mut prefs = Preferences::read();
            prefs.auto_update_disabled = false;
            prefs.write().context("Failed to save preferences")?;
            println!("{}", "Auto-updates enabled.".green());
            let update = UpdateCheck::read().unwrap_or_default();
            if let Some(ref skipped) = update.skipped_version {
                println!(
                    "Note: v{skipped} is still skipped from rollback; auto-update resumes on next release."
                );
            }
        }
        Commands::Disable => {
            let mut prefs = Preferences::read();
            prefs.auto_update_disabled = true;
            prefs.write().context("Failed to save preferences")?;
            // Acquire the update lock so we wait for any in-flight background
            // download to finish before cleaning, and prevent new staging while
            // we remove the directory.
            {
                use fs2::FileExt;
                let lock_path = crate::util::self_update::update_lock_path()?;
                if let Some(parent) = lock_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let lock_file = std::fs::File::create(&lock_path)
                    .context("Failed to create update lock file")?;
                // Wait for any concurrent stager/applier to finish.
                lock_file
                    .lock_exclusive()
                    .context("Failed to acquire update lock")?;
                let _ = crate::util::self_update::clean_staged();
                // Lock released on drop.
            }
            // Wait for any in-flight detached package manager updater
            // (npm/Bun/Scoop) to exit.  The spawn lock is only held during
            // the spawn window, not for the child's lifetime, so acquiring
            // it doesn't prove the child has finished.  We read the PID
            // file and poll the process directly.
            {
                let pid_path = crate::util::self_update::package_update_pid_path()?;
                if let Ok(contents) = std::fs::read_to_string(&pid_path) {
                    let parts: Vec<&str> = contents.split_whitespace().collect();
                    if let Some(pid_str) = parts.first() {
                        if let Ok(pid) = pid_str.parse::<u32>() {
                            if crate::util::check_update::is_pid_alive(pid) {
                                eprint!(
                                    "Waiting for in-flight package manager update (PID {pid}) to finish..."
                                );
                                let start = std::time::Instant::now();
                                let timeout = std::time::Duration::from_secs(30);
                                while crate::util::check_update::is_pid_alive(pid)
                                    && start.elapsed() < timeout
                                {
                                    std::thread::sleep(std::time::Duration::from_millis(500));
                                }
                                if crate::util::check_update::is_pid_alive(pid) {
                                    eprintln!(" timed out.");
                                    eprintln!(
                                        "{}: package manager update (PID {}) may still be running in the background.",
                                        "warning".yellow().bold(),
                                        pid,
                                    );
                                } else {
                                    eprintln!(" done.");
                                }
                            }
                        }
                    }
                }
                let _ = std::fs::remove_file(&pid_path);
            }
            println!("{}", "Auto-updates disabled.".yellow());
        }
        Commands::Status => {
            let prefs = Preferences::read();
            let env_disabled = is_auto_update_disabled_by_env();
            let method = InstallMethod::detect();

            let ci = Configs::env_is_ci();

            if env_disabled {
                println!(
                    "Auto-updates: {} (disabled by RAILWAY_NO_AUTO_UPDATE)",
                    "disabled".yellow()
                );
            } else if ci {
                println!(
                    "Auto-updates: {} (disabled in CI environment)",
                    "disabled".yellow()
                );
            } else if prefs.auto_update_disabled {
                println!(
                    "Auto-updates: {} (disabled via {})",
                    "disabled".yellow(),
                    "railway autoupdate disable".bold()
                );
            } else {
                println!("Auto-updates: {}", "enabled".green());
            }

            println!("Install method: {}", method.name().bold());
            println!("Update strategy: {}", method.update_strategy());

            let update = UpdateCheck::read().unwrap_or_default();
            if let Some(ref skipped) = update.skipped_version {
                println!(
                    "Skipped version: {} (rolled back; auto-update resumes on next release)",
                    format!("v{skipped}").yellow()
                );
            }
        }
    }
    Ok(())
}
