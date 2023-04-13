use crate::{commands::queries::User, controllers::user::get_user};
use colored::*;

use super::*;

/// Get the current logged in user
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let user: User = get_user(&client, &configs).await?;

    if let Some(name) = user.name {
        println!(
            "Logged in as {} ({}) 👋",
            name,
            user.email.bright_magenta().bold()
        )
    } else {
        println!("Logged in as {} 👋", user.email.bright_magenta().bold())
    }

    Ok(())
}
