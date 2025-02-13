use anyhow::Result;

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

// Generates the commands based on the modules in the commands directory
// Specify the modules you want to include in the commands_enum! macro
commands!(
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
    scale
);

#[tokio::main]
async fn main() -> Result<()> {
    let args = build_args().get_matches();
    match exec_cli(args).await {
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
