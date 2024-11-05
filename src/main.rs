use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;
use commands::*;

mod client;
mod config;
mod consts;
mod controllers;
mod errors;
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
    connect,
    deploy,
    domain,
    docs,
    down,
    environment(env),
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
    whoami,
    volume,
    redeploy
);

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Args::parse();

    match Commands::exec(cli).await {
        Ok(_) => {}
        Err(e) => {
            // If the user cancels the operation, we want to exit successfully
            // This can happen if Ctrl+C is pressed during a prompt
            if e.root_cause().to_string() == inquire::InquireError::OperationInterrupted.to_string()
            {
                return Ok(());
            }

            eprintln!("{:?}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
