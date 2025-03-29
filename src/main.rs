use std::cmp::Ordering;

use anyhow::Result;
use clap::error::ErrorKind;

mod commands;
use commands::*;
use is_terminal::IsTerminal;
use util::compare_semver::compare_semver;

mod client;
mod config;
mod consts;
mod controllers;
mod errors;
mod gql;
mod subscription;
mod table;
mod util;
mod workspace;

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
    ssh,
    starship,
    status,
    unlink,
    up,
    variables,
    whoami,
    volume,
    redeploy,
    scale,
    check_updates
);

fn spawn_update_task(mut configs: Configs) -> tokio::task::JoinHandle<Result<(), anyhow::Error>> {
    tokio::spawn(async move {
        if !std::io::stdout().is_terminal() {
            return Ok::<(), anyhow::Error>(());
        }

        let result = configs.check_update(false).await;
        if let Ok(Some(latest_version)) = result {
            configs.root_config.new_version_available = Some(latest_version);
        }
        configs.write()?;
        Ok::<(), anyhow::Error>(())
    })
}

async fn handle_update_task(handle: Option<tokio::task::JoinHandle<Result<(), anyhow::Error>>>) {
    if let Some(handle) = handle {
        match handle.await {
            Ok(Ok(_)) => {} // Task completed successfully
            Ok(Err(e)) => {
                if !std::io::stdout().is_terminal() {
                    eprintln!("Failed to check for updates (not fatal)");
                    eprintln!("{}", e);
                }
            }
            Err(e) => {
                eprintln!("Check Updates: Task panicked or failed to execute.");
                eprintln!("{}", e);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = build_args().try_get_matches();
    // Avoid grabbing configs multiple times, and avoid grabbing configs if we're not in a terminal
    let mut check_updates_handle: Option<tokio::task::JoinHandle<Result<(), anyhow::Error>>> = None;
    if std::io::stdout().is_terminal() {
        let mut configs = Configs::new()?;
        if let Some(new_version_available) = &configs.root_config.new_version_available {
            match compare_semver(env!("CARGO_PKG_VERSION"), new_version_available) {
                Ordering::Less => {
                    println!(
                        "{} v{} visit {} for more info",
                        "New version available:".green().bold(),
                        new_version_available.yellow(),
                        "https://docs.railway.com/guides/cli".purple(),
                    );
                }
                _ => {
                    configs.root_config.new_version_available = None;
                    configs.write()?;
                }
            }
        }
        check_updates_handle = Some(spawn_update_task(configs));
    }

    // https://github.com/clap-rs/clap/blob/cb2352f84a7663f32a89e70f01ad24446d5fa1e2/clap_builder/src/error/mod.rs#L210-L215
    let cli = match args {
        Ok(args) => args,
        // Clap's source code specifically says that these errors should be
        // printed to stdout and exit with a status of 0.
        Err(e) if e.kind() == ErrorKind::DisplayHelp || e.kind() == ErrorKind::DisplayVersion => {
            println!("{}", e);
            handle_update_task(check_updates_handle).await;
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("{}", e);
            handle_update_task(check_updates_handle).await;
            std::process::exit(2); // The default behavior is exit 2
        }
    };

    let exec_result = exec_cli(cli).await;

    if let Err(e) = exec_result {
        if e.root_cause().to_string() == inquire::InquireError::OperationInterrupted.to_string() {
            return Ok(()); // Exit gracefully if interrupted
        }
        eprintln!("{:?}", e);
        handle_update_task(check_updates_handle).await;
        std::process::exit(1);
    }

    handle_update_task(check_updates_handle).await;

    Ok(())
}
