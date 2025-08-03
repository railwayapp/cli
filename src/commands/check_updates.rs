use crate::util::check_update::check_update;

use super::*;
use serde_json::json;

/// Test the update check
#[derive(Parser)]
pub struct Args {
    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let latest_version = match check_update(true).await? {
        Some(latest_version) => latest_version,
        None => {
            println!(
                "You are on the latest version of the CLI, v{}",
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        }
    };

    if args.json {
        let json = json!({
            "latest_version": latest_version,
            "current_version": env!("CARGO_PKG_VERSION"),
        });

        println!("{}", serde_json::to_string_pretty(&json)?);

        return Ok(());
    }

    println!(
        "{} v{} visit {} for more info",
        "New version available:".green().bold(),
        latest_version.yellow(),
        "https://docs.railway.com/guides/cli".purple(),
    );

    Ok(())
}
