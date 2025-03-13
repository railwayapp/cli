use crate::util::check_update::check_update;

use super::*;
use serde_json::json;

/// Test the update check
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, json: bool) -> Result<()> {
    let mut configs = Configs::new()?;

    if json {
        let result = configs.check_update(true).await;

        let json = json!({
            "latest_version": result.ok().flatten().as_ref(),
            "current_version": env!("CARGO_PKG_VERSION"),
        });

        println!("{}", serde_json::to_string_pretty(&json)?);

        return Ok(());
    }

    let is_latest = check_update(&mut configs, true).await?;
    if is_latest {
        println!(
            "You are on the latest version of the CLI, v{}",
            env!("CARGO_PKG_VERSION")
        );
    }
    Ok(())
}
