use crate::check_update;

use super::*;
use serde_json::json;

/// Test the update check
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, json: bool) -> Result<()> {
    let mut configs = Configs::new()?;

    if json {
        let result = configs.check_update(true).await;

        if let Ok(Some(latest_version)) = result {
            let json = json!({
                "latest_version": latest_version,
                "current_version": env!("CARGO_PKG_VERSION"),
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        } else {
            let json = json!({
                "latest_version": None::<String>,
                "current_version": env!("CARGO_PKG_VERSION"),
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
        return Ok(());
    }

    check_update!(configs, true);
    Ok(())
}
