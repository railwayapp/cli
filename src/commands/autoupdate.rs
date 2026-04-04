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

fn pending_version(update: &UpdateCheck, staged_version: Option<String>) -> Option<String> {
    update.latest_version.clone().or(staged_version)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundUpdateKind {
    Download,
    PackageManager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BackgroundUpdate {
    pid: u32,
    kind: BackgroundUpdateKind,
}

fn running_background_update() -> Option<BackgroundUpdate> {
    let download_pid_path = crate::util::self_update::download_update_pid_path().ok()?;
    if let Some(pid) = crate::util::check_update::is_background_update_running(&download_pid_path) {
        return Some(BackgroundUpdate {
            pid,
            kind: BackgroundUpdateKind::Download,
        });
    }

    let package_pid_path = crate::util::self_update::package_update_pid_path().ok()?;
    crate::util::check_update::is_background_update_running(&package_pid_path).map(|pid| {
        BackgroundUpdate {
            pid,
            kind: BackgroundUpdateKind::PackageManager,
        }
    })
}

fn disable_in_flight_message(update: BackgroundUpdate) -> String {
    match update.kind {
        BackgroundUpdateKind::Download => format!(
            "Note: background download (PID {}) is already running and may still finish staging an update. \
             Disabling auto-updates prevents future automatic updates and automatic apply.",
            update.pid
        ),
        BackgroundUpdateKind::PackageManager => format!(
            "Note: background package-manager update (PID {}) is already running and may still finish. \
             Disabling auto-updates only prevents future automatic updates.",
            update.pid
        ),
    }
}

fn skip_in_flight_message(update: BackgroundUpdate, version: &str) -> Option<String> {
    match update.kind {
        BackgroundUpdateKind::Download => None,
        BackgroundUpdateKind::PackageManager => Some(format!(
            "Note: background package-manager update (PID {}) is already running and may still finish installing v{}. \
             Future auto-updates will skip this version.",
            update.pid, version
        )),
    }
}

fn background_update_status_message(update: BackgroundUpdate, auto_update_enabled: bool) -> String {
    match (update.kind, auto_update_enabled) {
        (BackgroundUpdateKind::Download, true) => {
            format!(
                "Background update: downloading and staging (PID {})",
                update.pid
            )
        }
        (BackgroundUpdateKind::Download, false) => format!(
            "Background update: downloading and staging (PID {}; started before auto-updates were disabled and may still finish)",
            update.pid
        ),
        (BackgroundUpdateKind::PackageManager, true) => {
            format!(
                "Background update: package manager running (PID {})",
                update.pid
            )
        }
        (BackgroundUpdateKind::PackageManager, false) => format!(
            "Background update: package manager running (PID {}; started before auto-updates were disabled and may still finish)",
            update.pid
        ),
    }
}

fn enable_status_message(env_disabled: bool, ci: bool) -> (&'static str, bool) {
    if env_disabled {
        (
            "Auto-update preference enabled, but updates remain disabled by RAILWAY_NO_AUTO_UPDATE.",
            false,
        )
    } else if ci {
        (
            "Auto-update preference enabled, but updates remain disabled in this CI environment.",
            false,
        )
    } else {
        ("Auto-updates enabled.", true)
    }
}

fn manual_upgrade_hint() -> &'static str {
    "Manual upgrade is still available via `railway upgrade --yes`."
}

fn should_show_manual_upgrade_hint(method: InstallMethod) -> bool {
    method.can_self_update() || method.can_auto_upgrade()
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Commands::Enable => {
            let mut prefs = Preferences::read();
            prefs.auto_update_disabled = false;
            prefs.write().context("Failed to save preferences")?;
            let env_disabled = is_auto_update_disabled_by_env();
            let ci = Configs::env_is_ci();
            let (message, effective_enabled) = enable_status_message(env_disabled, ci);
            if effective_enabled {
                println!("{}", message.green());
            } else {
                println!("{}", message.yellow());
            }
            let update = UpdateCheck::read_normalized();
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
            if let Some(update) = running_background_update() {
                println!("{}", disable_in_flight_message(update));
            }
        }
        Commands::Skip => {
            let update = UpdateCheck::read_normalized();
            let staged_version = crate::util::self_update::validated_staged_version();
            if let Some(version) = pending_version(&update, staged_version) {
                UpdateCheck::skip_version(&version);
                let _ = crate::util::self_update::clean_staged();
                println!(
                    "Skipping v{version}. Auto-update will resume when a newer version is released.",
                );
                if let Some(update) = running_background_update() {
                    if let Some(message) = skip_in_flight_message(update, &version) {
                        println!("{message}");
                    }
                }
            } else {
                println!("No pending update to skip.");
            }
        }
        Commands::Status => {
            let prefs = Preferences::read();
            let env_disabled = is_auto_update_disabled_by_env();
            let method = InstallMethod::detect();

            let ci = Configs::env_is_ci();
            let auto_update_enabled = !env_disabled && !ci && !prefs.auto_update_disabled;

            if env_disabled {
                println!(
                    "Auto-updates: {} (disabled by RAILWAY_NO_AUTO_UPDATE)",
                    "disabled".yellow()
                );
                if should_show_manual_upgrade_hint(method) {
                    println!("{}", manual_upgrade_hint());
                }
            } else if ci {
                println!(
                    "Auto-updates: {} (disabled in CI environment)",
                    "disabled".yellow()
                );
                if should_show_manual_upgrade_hint(method) {
                    println!("{}", manual_upgrade_hint());
                }
            } else if prefs.auto_update_disabled {
                println!(
                    "Auto-updates: {} (disabled via {})",
                    "disabled".yellow(),
                    "railway autoupdate disable".bold()
                );
                if should_show_manual_upgrade_hint(method) {
                    println!("{}", manual_upgrade_hint());
                }
            } else {
                println!("Auto-updates: {}", "enabled".green());
            }

            println!("Install method: {}", method.name().bold());
            println!("Update strategy: {}", method.update_strategy());

            let update = UpdateCheck::read_normalized();

            if let Some(ref version) = update.latest_version {
                println!("Latest known version: {}", format!("v{version}").cyan());
            }

            if let Some(ref staged) = crate::util::self_update::validated_staged_version() {
                if auto_update_enabled {
                    println!(
                        "Staged update: {} (will apply on next run)",
                        format!("v{staged}").green()
                    );
                } else {
                    println!(
                        "Staged update: {} (ready, but auto-updates are currently disabled)",
                        format!("v{staged}").yellow()
                    );
                }
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

            if let Some(update) = running_background_update() {
                println!(
                    "{}",
                    background_update_status_message(update, auto_update_enabled)
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_version_prefers_cached_latest() {
        let update = UpdateCheck {
            latest_version: Some("1.2.3".to_string()),
            ..Default::default()
        };

        assert_eq!(
            pending_version(&update, Some("1.2.2".to_string())).as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn pending_version_falls_back_to_staged_update() {
        let update = UpdateCheck::default();

        assert_eq!(
            pending_version(&update, Some("1.2.3".to_string())).as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn manual_upgrade_hint_is_hidden_for_unknown_install_method() {
        assert!(!should_show_manual_upgrade_hint(InstallMethod::Unknown));
    }

    #[test]
    fn manual_upgrade_hint_is_shown_for_auto_upgrade_methods() {
        assert!(should_show_manual_upgrade_hint(InstallMethod::Npm));
    }
}
