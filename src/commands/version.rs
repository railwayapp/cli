use serde::Serialize;

use super::*;
use crate::util::check_update::check_update;

/// Display version information
#[derive(Parser)]
pub struct Args {
    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Serialize)]
struct VersionJson {
    version: String,
    git_sha: String,
    build_date: String,
    rust_version: String,
    target: String,
    update_available: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION").to_string();
    let git_sha = env!("GIT_SHA").to_string();
    let build_date = env!("BUILD_DATE").to_string();
    let target = env!("BUILD_TARGET").to_string();
    let rust_version = env!("RUSTC_VERSION").to_string();

    // Check for updates (non-blocking, don't fail if check fails)
    let update_available = check_update(false).await.ok().flatten();
    
    if args.json {
        let output = VersionJson {
            version: version.clone(),
            git_sha: git_sha.clone(),
            build_date: build_date.clone(),
            rust_version: rust_version.clone(),
            target: target.clone(),
            update_available: update_available.clone(),
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    // Human-readable output
    println!(
        "{} {}",
        "Railway CLI".purple().bold(),
        version.green().bold()
    );
    println!();
    println!("{}", "Build Info:".bold());
    println!("  Version:    {}", version);
    println!("  Commit:     {}", git_sha.dimmed());
    println!("  Build Date: {}", build_date);
    println!("  Rust:       {}", rust_version);
    println!("  Target:     {}", target);
    println!();
    println!("{}", "Update Status:".bold());
    match update_available {
        Some(new_version) => {
            println!(
                "  {} New version available: {} -> {}",
                "*".yellow(),
                version.red(),
                new_version.green()
            );
            println!("  Run {} to upgrade", "`railway upgrade`".cyan());
        }
        None => {
            println!("  {} You are on the latest version", "*".green());
        }
    }

    Ok(())
}
