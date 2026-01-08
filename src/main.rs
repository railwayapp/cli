use std::cmp::Ordering;

use anyhow::Result;
use clap::error::ErrorKind;

mod commands;
use commands::*;
use is_terminal::IsTerminal;
use util::{check_update::UpdateCheck, compare_semver::compare_semver};

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
    deployment,
    dev(develop),
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
    run(local),
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
    check_updates,
    functions(function, func, fn, funcs, fns)
);

fn spawn_update_task() -> tokio::task::JoinHandle<anyhow::Result<Option<String>>> {
    tokio::spawn(async move {
        // outputting would break json output on CI
        if !std::io::stdout().is_terminal() {
            anyhow::bail!("Stdout is not a terminal");
        }
        let latest_version = util::check_update::check_update(false).await?;

        Ok(latest_version)
    })
}

async fn handle_update_task(
    handle: Option<tokio::task::JoinHandle<anyhow::Result<Option<String>>>>,
) {
    if let Some(handle) = handle {
        match handle.await {
            Ok(Ok(_)) => {} // Task completed successfully
            Ok(Err(e)) => {
                if !std::io::stdout().is_terminal() {
                    eprintln!("Failed to check for updates (not fatal)");
                    eprintln!("{e}");
                }
            }
            Err(e) => {
                eprintln!("Check Updates: Task panicked or failed to execute.");
                eprintln!("{e}");
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = build_args().try_get_matches();
    let check_updates_handle = if std::io::stdout().is_terminal() {
        let update = UpdateCheck::read().unwrap_or_default();

        if let Some(latest_version) = update.latest_version {
            if matches!(
                compare_semver(env!("CARGO_PKG_VERSION"), &latest_version),
                Ordering::Less
            ) {
                println!(
                    "{} v{} visit {} for more info",
                    "New version available:".green().bold(),
                    latest_version.yellow(),
                    "https://docs.railway.com/guides/cli".purple(),
                );
            }
            let update = UpdateCheck {
                last_update_check: Some(chrono::Utc::now()),
                latest_version: None,
            };
            update
                .write()
                .context("Failed to save time since last update check")?;
        }

        Some(spawn_update_task())
    } else {
        None
    };

    // https://github.com/clap-rs/clap/blob/cb2352f84a7663f32a89e70f01ad24446d5fa1e2/clap_builder/src/error/mod.rs#L210-L215
    let cli = match args {
        Ok(args) => args,
        // Clap's source code specifically says that these errors should be
        // printed to stdout and exit with a status of 0.
        Err(e) if e.kind() == ErrorKind::DisplayHelp || e.kind() == ErrorKind::DisplayVersion => {
            println!("{e}");
            handle_update_task(check_updates_handle).await;
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("{e}");
            handle_update_task(check_updates_handle).await;
            std::process::exit(2); // The default behavior is exit 2
        }
    };

    let exec_result = exec_cli(cli).await;

    if let Err(e) = exec_result {
        if e.root_cause().to_string() == inquire::InquireError::OperationInterrupted.to_string() {
            return Ok(()); // Exit gracefully if interrupted
        }

        eprintln!("{e:?}");

        handle_update_task(check_updates_handle).await;
        std::process::exit(1);
    }

    handle_update_task(check_updates_handle).await;

    Ok(())
}
