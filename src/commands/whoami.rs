use crate::{commands::queries::RailwayUser, controllers::user::get_user, workspace::workspaces};
use colored::*;
use serde::Serialize;

use super::*;

/// Get the current logged in user
#[derive(Parser)]
pub struct Args {
    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Serialize)]
struct WhoamiJson {
    name: Option<String>,
    email: String,
    workspaces: Vec<WorkspaceJson>,
}

#[derive(Serialize)]
struct WorkspaceJson {
    id: String,
    name: String,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let user: RailwayUser = get_user(&client, &configs).await?;

    if args.json {
        let ws = workspaces().await?;
        let output = WhoamiJson {
            name: user.name,
            email: user.email,
            workspaces: ws
                .into_iter()
                .map(|w| WorkspaceJson {
                    id: w.id().to_string(),
                    name: w.name().to_string(),
                })
                .collect(),
        };
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    } else {
        print_user(user);
    }

    Ok(())
}

pub fn print_user(user: RailwayUser) {
    let email_colored = user.email.bright_magenta().bold();

    match user.name {
        Some(name) => println!("Logged in as {name} ({email_colored}) 👋"),
        None => println!("Logged in as {email_colored} 👋"),
    }
}
