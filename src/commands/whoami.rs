use crate::{commands::queries::RailwayUser, controllers::user::get_user, util::prompt};
use colored::*;

use super::*;

/// Get the current logged in user
#[derive(Parser)]
pub struct Args {
    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let user: RailwayUser = get_user(&client, &configs).await?;

    print_user(user, args.json);

    Ok(())
}

pub fn print_user(user: RailwayUser, use_json: bool) {
    if use_json {
        println!("{}", serde_json::to_string_pretty(&user).unwrap());
        return;
    }

    let email_colored = user.email.bright_magenta().bold();

    match user.name {
        Some(name) => println!("Logged in as {name} ({email_colored}) 👋"),
        None => println!("Logged in as {email_colored} 👋"),
    }
}
