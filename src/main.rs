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
// #[clap(author, about, long_about = None)]
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
    redeploy,
    check_updates
);

#[tokio::main]
async fn main() -> Result<()> {
    // intercept the args
    {
        let args: Vec<String> = std::env::args().collect();

        let flags: Vec<String> = vec!["--version", "-V", "-h", "--help", "help"]
            .into_iter()
            .map(|s| s.to_string())
            .collect();

        let check_version = args.into_iter().any(|arg| flags.contains(&arg));

        if check_version {
            let mut configs = Configs::new()?;
            check_update!(configs, false);
        }
    }

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
