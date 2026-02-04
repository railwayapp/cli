use std::process::Command;

use crate::{consts::NON_INTERACTIVE_FAILURE, interact_or};

use super::*;

/// Upgrade the Railway CLI to the latest version
#[derive(Parser)]
pub struct Args {
    /// Check install method without upgrading
    #[clap(long)]
    check: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallMethod {
    Homebrew,
    Npm,
    Bun,
    Cargo,
    Shell,
    Scoop,
    Unknown,
}

impl InstallMethod {
    fn detect() -> Self {
        let exe_path = match std::env::current_exe() {
            Ok(path) => path,
            Err(_) => return InstallMethod::Unknown,
        };

        let path_str = exe_path.to_string_lossy().to_lowercase();

        // Check for Homebrew (macOS/Linux)
        if path_str.contains("homebrew")
            || path_str.contains("cellar")
            || path_str.contains("linuxbrew")
        {
            return InstallMethod::Homebrew;
        }

        // Check for Bun global install (must be before npm since bun uses node_modules internally)
        if path_str.contains(".bun") {
            return InstallMethod::Bun;
        }

        // Check for npm global install
        if path_str.contains("node_modules")
            || path_str.contains("npm")
            || path_str.contains(".npm")
        {
            return InstallMethod::Npm;
        }

        // Check for Cargo install
        if path_str.contains(".cargo") && path_str.contains("bin") {
            return InstallMethod::Cargo;
        }

        // Check for Scoop (Windows)
        if path_str.contains("scoop") {
            return InstallMethod::Scoop;
        }

        // Check for shell script install (typically in /usr/local/bin or ~/.local/bin)
        if path_str.contains("/usr/local/bin") || path_str.contains("/.local/bin") {
            return InstallMethod::Shell;
        }

        // Check for Windows Program Files (shell install)
        if path_str.contains("program files") || path_str.contains("programfiles") {
            return InstallMethod::Shell;
        }

        InstallMethod::Unknown
    }

    fn name(&self) -> &'static str {
        match self {
            InstallMethod::Homebrew => "Homebrew",
            InstallMethod::Npm => "npm",
            InstallMethod::Bun => "Bun",
            InstallMethod::Cargo => "Cargo",
            InstallMethod::Shell => "Shell script",
            InstallMethod::Scoop => "Scoop",
            InstallMethod::Unknown => "Unknown",
        }
    }

    fn upgrade_command(&self) -> Option<&'static str> {
        match self {
            InstallMethod::Homebrew => Some("brew upgrade railway"),
            InstallMethod::Npm => Some("npm update -g @railway/cli"),
            InstallMethod::Bun => Some("bun update -g @railway/cli"),
            InstallMethod::Cargo => Some("cargo install railwayapp"),
            InstallMethod::Scoop => Some("scoop update railway"),
            InstallMethod::Shell => Some("bash <(curl -fsSL cli.new)"),
            InstallMethod::Unknown => None,
        }
    }

    fn can_auto_upgrade(&self) -> bool {
        matches!(
            self,
            InstallMethod::Homebrew
                | InstallMethod::Npm
                | InstallMethod::Bun
                | InstallMethod::Cargo
                | InstallMethod::Scoop
        )
    }
}

fn run_upgrade_command(method: InstallMethod) -> Result<()> {
    let (program, args): (&str, Vec<&str>) = match method {
        InstallMethod::Homebrew => ("brew", vec!["upgrade", "railway"]),
        InstallMethod::Npm => ("npm", vec!["update", "-g", "@railway/cli"]),
        InstallMethod::Bun => ("bun", vec!["update", "-g", "@railway/cli"]),
        InstallMethod::Cargo => ("cargo", vec!["install", "railwayapp"]),
        InstallMethod::Scoop => ("scoop", vec!["update", "railway"]),
        InstallMethod::Shell | InstallMethod::Unknown => {
            bail!("Cannot auto-upgrade for this install method");
        }
    };

    println!("{} {} {}", "Running:".bold(), program, args.join(" "));
    println!();

    let status = Command::new(program)
        .args(&args)
        .status()
        .context(format!("Failed to execute {program}"))?;

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

    interact_or!(NON_INTERACTIVE_FAILURE);

    println!(
        "{} {} ({})",
        "Current version:".bold(),
        env!("CARGO_PKG_VERSION"),
        method.name()
    );

    match method {
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
        InstallMethod::Shell => {
            println!();
            println!(
                "{}",
                "Detected shell script installation. To upgrade, run:".yellow()
            );
            println!();
            println!("  {}", "bash <(curl -fsSL cli.new)".cyan());
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
        _ => unreachable!(),
    }

    Ok(())
}
