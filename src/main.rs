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
mod telemetry;

// Generates the commands based on the modules in the commands directory
// Specify the modules you want to include in the commands_enum! macro
commands!(
    add,
    completion,
    connect,
    delete,
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
    project,
    run(local),
    service,
    shell,
    ssh,
    starship,
    status,
    telemetry_cmd(telemetry),
    unlink,
    up,
    upgrade,
    variable(variables, vars, var),
    whoami,
    volume,
    redeploy,
    restart,
    scale,
    metrics,
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
        telemetry::show_notice_if_needed();

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

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<clap::ArgMatches, clap::Error> {
        let mut full_args = vec!["railway"];
        full_args.extend(args);
        build_args().try_get_matches_from(full_args)
    }

    fn assert_parses(args: &[&str]) {
        assert!(
            parse(args).is_ok(),
            "Command should parse: railway {}",
            args.join(" ")
        );
    }

    fn assert_subcommand(args: &[&str], expected: &str) {
        let matches = parse(args).unwrap_or_else(|_| panic!("Failed to parse: {:?}", args));
        assert_eq!(
            matches.subcommand_name(),
            Some(expected),
            "Expected subcommand '{}' for args {:?}",
            expected,
            args
        );
    }

    mod backwards_compat {
        use super::*;

        #[test]
        fn root_commands_exist() {
            assert_subcommand(&["logs"], "logs");
            assert_subcommand(&["list"], "list");
            assert_subcommand(&["delete"], "delete");
            assert_subcommand(&["restart"], "restart");
            assert_subcommand(&["scale"], "scale");
            assert_subcommand(&["link"], "link");
            assert_subcommand(&["up"], "up");
            assert_subcommand(&["redeploy"], "redeploy");
            assert_subcommand(&["metrics"], "metrics");
        }

        #[test]
        fn variable_aliases() {
            assert_subcommand(&["variable"], "variable");
            assert_subcommand(&["variables"], "variable");
            assert_subcommand(&["vars"], "variable");
            assert_subcommand(&["var"], "variable");
        }

        #[test]
        fn variable_legacy_flags() {
            assert_parses(&["variable", "--set", "KEY=value"]);
            assert_parses(&["variable", "--set", "KEY=value", "--set", "KEY2=value2"]);
            assert_parses(&["variable", "-s", "myservice"]);
            assert_parses(&["variable", "-e", "production"]);
            assert_parses(&["variable", "--kv"]);
            assert_parses(&["variable", "--json"]);
            assert_parses(&["variable", "--skip-deploys", "--set", "KEY=value"]);
            assert_parses(&["variables", "--set", "KEY=value"]); // via alias
        }

        #[test]
        fn environment_implicit_link() {
            assert_parses(&["environment", "production"]); // legacy positional
            assert_parses(&["env", "production"]); // alias
        }

        #[test]
        fn service_implicit_link() {
            assert_parses(&["service"]); // prompts for link
            assert_parses(&["service", "myservice"]); // legacy positional link
        }

        #[test]
        fn functions_aliases() {
            assert_subcommand(&["functions", "list"], "functions");
            assert_subcommand(&["function", "list"], "functions");
            assert_subcommand(&["func", "list"], "functions");
            assert_subcommand(&["fn", "list"], "functions");
            assert_subcommand(&["funcs", "list"], "functions");
            assert_subcommand(&["fns", "list"], "functions");
        }

        #[test]
        fn dev_run_aliases() {
            assert_subcommand(&["dev"], "dev");
            assert_subcommand(&["develop"], "dev");
            assert_subcommand(&["run"], "run");
            assert_subcommand(&["local"], "run");
        }

        #[test]
        fn variable_set_from_stdin_legacy() {
            assert_parses(&["variable", "--set-from-stdin", "MY_KEY"]);
            assert_parses(&["variable", "--set-from-stdin", "KEY", "-s", "myservice"]);
            assert_parses(&["variable", "--set-from-stdin", "KEY", "--skip-deploys"]);
            assert_parses(&["variables", "--set-from-stdin", "KEY"]);
        }

        #[test]
        fn variable_list_kv_format() {
            assert_parses(&["variable", "--kv"]);
            assert_parses(&["variable", "-k"]);
            assert_parses(&["variables", "--kv"]);
        }
    }

    mod new_commands {
        use super::*;

        #[test]
        fn variable_subcommands() {
            assert_parses(&["variable", "list"]);
            assert_parses(&["variable", "list", "-s", "myservice"]);
            assert_parses(&["variable", "list", "--json"]);
            assert_parses(&["variable", "set", "KEY=value"]);
            assert_parses(&["variable", "set", "KEY=value", "KEY2=value2"]); // multiple
            assert_parses(&["variable", "set", "A=1", "B=2", "C=3", "--skip-deploys"]);
            assert_parses(&["variable", "set", "KEY", "--stdin"]);
            assert_parses(&["variable", "set", "KEY=value", "--skip-deploys"]);
            assert_parses(&["variable", "delete", "KEY"]);
            assert_parses(&["variable", "rm", "KEY"]); // alias
            assert_parses(&["variable", "delete", "KEY", "--json"]);
        }

        #[test]
        fn environment_link_subcommand() {
            assert_parses(&["environment", "link"]);
            assert_parses(&["environment", "link", "production"]);
            assert_parses(&["environment", "link", "--json"]);
        }

        #[test]
        fn service_subcommands() {
            assert_parses(&["service", "link"]);
            assert_parses(&["service", "status"]);
            assert_parses(&["service", "status", "--all"]);
            assert_parses(&["service", "status", "--json"]);
            assert_parses(&["service", "logs"]);
            assert_parses(&["service", "logs", "-s", "myservice"]);
            assert_parses(&["service", "redeploy"]);
            assert_parses(&["service", "redeploy", "-s", "myservice"]);
            assert_parses(&["service", "restart"]);
            assert_parses(&["service", "scale"]);
        }

        #[test]
        fn project_subcommands() {
            assert_parses(&["project", "list"]);
            assert_parses(&["project", "ls"]); // alias
            assert_parses(&["project", "list", "--json"]);
            assert_parses(&["project", "link"]);
            assert_parses(&["project", "delete"]);
            assert_parses(&["project", "rm"]); // alias
            assert_parses(&["project", "delete", "-y"]);
        }

        #[test]
        fn variable_list_aliases() {
            assert_parses(&["variable", "ls"]);
            assert_parses(&["variable", "ls", "--kv"]);
            assert_parses(&["variable", "ls", "-s", "myservice"]);
        }

        #[test]
        fn variable_delete_remove_alias() {
            assert_parses(&["variable", "remove", "KEY"]);
        }

        #[test]
        fn variable_set_stdin_key_only() {
            assert_parses(&["variable", "set", "KEY", "--stdin"]);
            assert_parses(&["variable", "set", "MY_VAR", "--stdin", "-s", "myservice"]);
            assert_parses(&["variable", "set", "SECRET", "--stdin", "--skip-deploys"]);
        }

        #[test]
        fn metrics_command_flags() {
            assert_parses(&["metrics"]);
            assert_parses(&["metrics", "--service", "myservice"]);
            assert_parses(&["metrics", "-s", "myservice"]);
            assert_parses(&["metrics", "--time", "1h"]);
            assert_parses(&["metrics", "--time", "6h"]);
            assert_parses(&["metrics", "--time", "1d"]);
            assert_parses(&["metrics", "--time", "7d"]);
            assert_parses(&["metrics", "--json"]);
            assert_parses(&["metrics", "--watch"]);
            assert_parses(&["metrics", "-w"]);
            assert_parses(&["metrics", "-s", "myservice", "--time", "1h", "--json"]);
        }
    }
}
