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
    /// Skip the current pending version (useful if a release is broken)
    Skip,
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
            // Clean any staged binary so it isn't applied on next launch.
            // Best-effort: if a background download holds the lock, the staged
            // dir will be left behind but try_apply_staged() checks the
            // preference and won't apply it.
            let _ = crate::util::self_update::clean_staged();
            println!("{}", "Auto-updates disabled.".yellow());
        }
        Commands::Skip => {
            let update = UpdateCheck::read().unwrap_or_default();
            if let Some(ref version) = update.latest_version {
                UpdateCheck::skip_version(version);
                let _ = crate::util::self_update::clean_staged();
                println!(
                    "Skipping v{version}. Auto-update will resume when a newer version is released.",
                );
            } else {
                println!("No pending update to skip.");
            }
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

            if let Some(ref version) = update.latest_version {
                println!("Latest known version: {}", format!("v{version}").cyan());
            }

            if let Some(ref staged) = crate::util::self_update::staged_version() {
                println!(
                    "Staged update: {} (will apply on next run)",
                    format!("v{staged}").green()
                );
            }

            if let Some(ref skipped) = update.skipped_version {
                println!(
                    "Skipped version: {} (rolled back; auto-update resumes on next release)",
                    format!("v{skipped}").yellow()
                );
            }

            if let Some(last_check) = update.last_update_check {
                let ago = chrono::Utc::now() - last_check;
                let label = if ago.num_hours() < 1 {
                    format!("{}m ago", ago.num_minutes())
                } else if ago.num_hours() < 24 {
                    format!("{}h ago", ago.num_hours())
                } else {
                    format!("{}d ago", ago.num_days())
                };
                println!("Last check: {}", label);
            }

            if let Ok(pid_path) = crate::util::self_update::package_update_pid_path() {
                if let Some(pid) = crate::util::check_update::is_package_update_running(&pid_path) {
                    println!("Background update: {} (PID {})", "running".green(), pid);
                }
            }
        }
    }
    Ok(())
}
