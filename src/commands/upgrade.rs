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

    println!("{} {} {}", "Running:".bold(), program, args.join(" "));
    println!();

    let status = Command::new(program)
        .args(&args)
        .status()
        .context(format!("Failed to execute {}", program))?;

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
            println!("To upgrade, either:");
            println!("  1. Re-run with elevated permissions:");
            println!("     {}", "sudo railway upgrade".bold());
            println!("  2. Reinstall using the install script:");
            println!("     {}", "bash <(curl -fsSL cli.new)".bold());
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
