use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;
use commands::*;

mod client;
mod config;
mod consts;
mod gql;
mod subscription;
mod table;
mod util;

#[macro_use]
mod macros;

/// Interact with 🚅 Railway via CLI
#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
#[clap(propagate_version = true)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Output in JSON format
    #[clap(global = true, long)]
    json: bool,
}

// Generates the commands based on the modules in the commands directory
// Specify the modules you want to include in the commands_enum! macro
commands_enum!(
    add,
    completion,
    delete,
    domain,
    docs,
    down,
    environment,
    init,
    link,
    list,
    login,
    logout,
    logs,
    open,
    run,
    service,
    shell,
    starship,
    status,
    unlink,
    up,
    variables,
    whoami
);

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Args::parse();

    Commands::exec(cli).await?;

    Ok(())
}
