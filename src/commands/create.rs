use clap::Subcommand;

use super::*;

/// Create something on Railway (an account, etc.)
#[derive(Parser)]
pub struct Args {
    #[command(subcommand)]
    command: CreateCommands,
}

#[derive(Subcommand)]
enum CreateCommands {
    /// Sign up for a new Railway account (or sign in if you have one).
    /// Opens the browser to a signup-friendly landing page and writes
    /// the CLI token on success.
    Account,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        CreateCommands::Account => {
            super::login::command(super::login::Args {
                browserless: false,
                signup: true,
            })
            .await
        }
    }
}
