use std::process::Command;

use is_terminal::IsTerminal;

use crate::util::install_method::InstallMethod;

use super::*;

/// Upgrade the Railway CLI to the latest version.
/// Use `--yes` for non-interactive agent/script usage.
#[derive(Parser)]
pub struct Args {
    /// Check install method without upgrading
    #[clap(long, conflicts_with = "rollback")]
    check: bool,

    /// Rollback to the previous version
    #[clap(long, conflicts_with = "check")]
    rollback: bool,

    /// Run without interactive prompts (useful for agents/scripts)
    #[clap(short = 'y', long = "yes")]
    yes: bool,
}

fn validate_interaction(yes: bool, is_tty: bool) -> Result<()> {
    if !yes && !is_tty {
        bail!(
            "Cannot run `railway upgrade` in non-interactive mode. Use `--yes` to continue without prompts."
        );
    }

    Ok(())
}

fn fail_if_non_interactive_requested(yes: bool, message: &str) -> Result<()> {
    if yes {
        bail!(message.to_string());
    }

    Ok(())
}

fn retry_command(rollback: bool, yes: bool, elevated: bool) -> String {
    let mut parts = Vec::new();

    if elevated {
        parts.push("sudo");
    }

    parts.push("railway");
    parts.push("upgrade");

    if rollback {
        parts.push("--rollback");
    }

    if yes {
        parts.push("--yes");
    }

    parts.join(" ")
}

fn run_upgrade_command(method: InstallMethod) -> Result<()> {
    let (program, args) = method
        .package_manager_command()
        .context("Cannot auto-upgrade for this install method")?;

    // Coordinate with background auto-updates: acquire the same lock and
    // check the PID file used by spawn_package_manager_update() so we
    // don't run two package-manager processes against the same global install.
    use fs2::FileExt;

    let lock_path = crate::util::self_update::package_update_lock_path()?;
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_file =
        std::fs::File::create(&lock_path).context("Failed to create package-update lock file")?;
    lock_file.try_lock_exclusive().map_err(|_| {
        anyhow::anyhow!("A background update is already in progress. Please try again shortly.")
    })?;

    let pid_path = crate::util::self_update::package_update_pid_path()?;
    if let Some(pid) = crate::util::check_update::is_background_update_running(&pid_path) {
        bail!(
            "A background update (pid {pid}) is already running. \
             Please wait for it to finish or try again shortly."
        );
    }

    println!("{} {} {}", "Running:".bold(), program, args.join(" "));
    println!();

    let status = Command::new(program)
        .args(&args)
        .status()
        .context(format!("Failed to execute {}", program))?;

    // Clean up stale PID file from a previous background updater.
    let _ = std::fs::remove_file(&pid_path);

    if !status.success() {
        bail!(
            "Upgrade command failed with exit code: {}",
            status.code().unwrap_or(-1)
        );
    }

    println!();
    println!("{}", "Upgrade complete!".green().bold());

    Ok(())
}

pub async fn command(args: Args) -> Result<()> {
    let method = InstallMethod::detect();
    let exe_path = std::env::current_exe().ok();

    if args.check {
        println!("{} {}", "Install method:".bold(), method.name());
        if let Some(path) = exe_path {
            println!("{} {}", "Binary path:".bold(), path.display());
        }
        if let Some(cmd) = method.upgrade_command() {
            println!("{} {}", "Upgrade command:".bold(), cmd);
        }
        return Ok(());
    }

    if args.rollback {
        if !method.can_self_update() {
            bail!(
                "Rollback is only supported for shell-script installs.\n\
                 Detected install method: {}. Use your package manager to \
                 install a specific version instead.",
                method.name()
            );
        }
        validate_interaction(args.yes, std::io::stdout().is_terminal())?;
        if !method.can_write_binary() {
            println!(
                "{}",
                "Cannot rollback: the CLI binary is not writable by the current user.".yellow()
            );
            println!();
            if cfg!(windows) {
                println!("To rollback, run the terminal as Administrator and retry:");
                println!("  {}", retry_command(true, args.yes, false).bold());
            } else {
                println!("To rollback, re-run with elevated permissions:");
                println!("  {}", retry_command(true, args.yes, true).bold());
            }
            fail_if_non_interactive_requested(
                args.yes,
                "Rollback could not be completed because the CLI binary is not writable by the current user.",
            )?;
            return Ok(());
        }
        return crate::util::self_update::rollback(args.yes);
    }

    validate_interaction(args.yes, std::io::stdout().is_terminal())?;

    println!(
        "{} {} ({})",
        "Current version:".bold(),
        env!("CARGO_PKG_VERSION"),
        method.name()
    );

    // Order matters: check self-update first, then unknown, then package manager.
    match method {
        method if method.can_self_update() && method.can_write_binary() => {
            println!();
            crate::util::self_update::self_update_interactive().await?;
        }
        method if method.can_self_update() => {
            // Shell install but binary location not writable by current user
            println!();
            println!(
                "{}",
                "Cannot upgrade: the CLI binary is not writable by the current user.".yellow()
            );
            println!();
            if cfg!(windows) {
                println!("To upgrade, run the terminal as Administrator and retry:");
                println!("  {}", retry_command(false, args.yes, false).bold());
            } else {
                println!("To upgrade, either:");
                println!("  1. Re-run with elevated permissions:");
                println!("     {}", retry_command(false, args.yes, true).bold());
                println!("  2. Reinstall using the install script:");
                println!("     {}", "bash <(curl -fsSL cli.new)".bold());
            }
            fail_if_non_interactive_requested(
                args.yes,
                "Upgrade could not be completed because the CLI binary is not writable by the current user.",
            )?;
        }
        InstallMethod::Unknown => {
            println!();
            println!(
                "{}",
                "Could not detect install method. Please upgrade manually:".yellow()
            );
            println!();
            println!("  {}", "Homebrew:".bold());
            println!("    brew upgrade railway");
            println!();
            println!("  {}", "npm:".bold());
            println!("    npm update -g @railway/cli");
            println!();
            println!("  {}", "Bun:".bold());
            println!("    bun update -g @railway/cli");
            println!();
            println!("  {}", "Cargo:".bold());
            println!("    cargo install railwayapp");
            println!();
            println!("  {}", "Shell script:".bold());
            println!("    bash <(curl -fsSL cli.new)");
            println!();
            println!("  {}", "Scoop (Windows):".bold());
            println!("    scoop update railway");
            println!();
            println!(
                "For more information, visit: {}",
                "https://docs.railway.com/guides/cli".purple()
            );
            fail_if_non_interactive_requested(
                args.yes,
                "Automatic upgrade could not be completed because the install method could not be detected.",
            )?;
        }
        method if method.can_auto_upgrade() => {
            println!();
            run_upgrade_command(method)?;
        }
        InstallMethod::Shell => {
            // Shell install on a platform where self-update is unsupported
            // (e.g. FreeBSD). Show the reinstall command.
            println!();
            println!(
                "{}",
                "Self-update is not available on this platform. To upgrade, re-run the install script:".yellow()
            );
            println!();
            println!("  {}", "bash <(curl -fsSL cli.new)".cyan());
            fail_if_non_interactive_requested(
                args.yes,
                "Automatic upgrade could not be completed because self-update is not available on this platform.",
            )?;
        }
        _ => {
            println!();
            println!(
                "{}",
                "Could not determine an upgrade strategy for this install method.".yellow()
            );
            println!(
                "Please upgrade manually. For more information, visit: {}",
                "https://docs.railway.com/guides/cli".purple()
            );
            fail_if_non_interactive_requested(
                args.yes,
                "Automatic upgrade could not be completed for this install method.",
            )?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Args, validate_interaction};
    use clap::Parser;

    #[test]
    fn parser_rejects_check_and_rollback_together() {
        let result = Args::try_parse_from(["railway", "--check", "--rollback"]);

        assert!(result.is_err());
    }

    #[test]
    fn parser_accepts_yes_with_rollback() {
        let result = Args::try_parse_from(["railway", "--yes", "--rollback"]);

        assert!(result.is_ok());
    }

    #[test]
    fn interactive_upgrade_does_not_require_yes() {
        assert!(validate_interaction(false, true).is_ok());
    }

    #[test]
    fn non_interactive_upgrade_requires_yes() {
        assert!(validate_interaction(false, false).is_err());
        assert!(validate_interaction(true, false).is_ok());
    }
}
