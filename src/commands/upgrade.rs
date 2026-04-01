use std::process::Command;

use crate::util::install_method::InstallMethod;
use crate::{consts::NON_INTERACTIVE_FAILURE, interact_or};

use super::*;

/// Upgrade the Railway CLI to the latest version
#[derive(Parser)]
pub struct Args {
    /// Check install method without upgrading
    #[clap(long)]
    check: bool,

    /// Rollback to the previous version
    #[clap(long)]
    rollback: bool,
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
    if let Ok(contents) = std::fs::read_to_string(&pid_path) {
        let parts: Vec<&str> = contents.split_whitespace().collect();
        if let (Some(pid_str), Some(ts_str)) = (parts.first(), parts.get(1)) {
            if let (Ok(pid), Ok(ts)) = (pid_str.parse::<u32>(), ts_str.parse::<i64>()) {
                let now = chrono::Utc::now().timestamp();
                let age_secs = now.saturating_sub(ts);
                if age_secs < 600 && crate::util::check_update::is_pid_alive(pid) {
                    bail!(
                        "A background update (pid {pid}) is already running. \
                         Please wait for it to finish or try again shortly."
                    );
                }
            }
        }
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
        interact_or!(NON_INTERACTIVE_FAILURE);
        if !method.can_write_binary() {
            println!(
                "{}",
                "Cannot rollback: the CLI binary is not writable by the current user.".yellow()
            );
            println!();
            if cfg!(windows) {
                println!("To rollback, run the terminal as Administrator and retry:");
                println!("  {}", "railway upgrade --rollback".bold());
            } else {
                println!("To rollback, re-run with elevated permissions:");
                println!("  {}", "sudo railway upgrade --rollback".bold());
            }
            return Ok(());
        }
        return crate::util::self_update::rollback();
    }

    interact_or!(NON_INTERACTIVE_FAILURE);

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
                println!("  {}", "railway upgrade".bold());
            } else {
                println!("To upgrade, either:");
                println!("  1. Re-run with elevated permissions:");
                println!("     {}", "sudo railway upgrade".bold());
                println!("  2. Reinstall using the install script:");
                println!("     {}", "bash <(curl -fsSL cli.new)".bold());
            }
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
        }
        method if method.can_auto_upgrade() => {
            println!();
            run_upgrade_command(method)?;
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
        }
    }

    Ok(())
}
