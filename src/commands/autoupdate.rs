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
            UpdateCheck::clear_skipped_version();
            println!("{}", "Auto-updates enabled.".green());
        }
        Commands::Disable => {
            let mut prefs = Preferences::read();
            prefs.auto_update_disabled = true;
            prefs.write().context("Failed to save preferences")?;
            // Clean up any staged update that would otherwise sit on disk indefinitely.
            // Note: a package-manager child already spawned by a prior invocation runs
            // detached and cannot be cancelled here — it will finish regardless.
            // The preference flip takes effect on every subsequent invocation.
            let _ = crate::util::self_update::clean_staged();
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
