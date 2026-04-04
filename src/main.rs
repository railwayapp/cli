use std::cmp::Ordering;

use anyhow::Result;
use clap::error::ErrorKind;

mod commands;
use commands::*;
use config::Configs;
use is_terminal::IsTerminal;
use util::{check_update::UpdateCheck, compare_semver::compare_semver};

mod client;
mod config;
mod consts;
mod controllers;
mod errors;
mod gql;
mod oauth;
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
    autoupdate,
    bucket,
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
    mcp,
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
    check_updates,
    functions(function, func, fn, funcs, fns)
);

fn spawn_update_task(
    known_version: Option<String>,
    auto_update_enabled: bool,
    skipped_version: Option<String>,
    check_gate_armed: bool,
) -> tokio::task::JoinHandle<anyhow::Result<Option<String>>> {
    tokio::spawn(async move {
        // When the check gate is armed (last check was <12h ago) the API
        // call returns instantly (no network request), so we can safely
        // start the background update from the cached version before the
        // await — the API cannot return a newer version that would race
        // with this download.
        //
        // When the gate is NOT armed, the API call may discover a newer
        // release, so we must not eagerly spawn for a potentially stale
        // cached version.
        if auto_update_enabled && check_gate_armed {
            if let Some(ref version) = known_version {
                let is_skipped = skipped_version.as_deref() == Some(version.as_str());
                if !is_skipped {
                    let method = util::install_method::InstallMethod::detect();
                    if method.can_self_update() && method.can_write_binary() {
                        let _ = util::self_update::spawn_background_download(version);
                    } else if method.can_auto_run_package_manager() {
                        let _ = util::check_update::spawn_package_manager_update(method);
                    }
                }
            }
        }

        // Fresh API check (respects 12h gate).  Fall back to the
        // cached version when the gate fires or the API errors.
        let (from_cache, latest_version) = match util::check_update::check_update(false).await {
            Ok(Some(v)) => (false, Some(v)),
            Ok(None) | Err(_) => (known_version.is_some(), known_version),
        };

        if let Some(ref version) = latest_version {
            if auto_update_enabled {
                let is_skipped = skipped_version.as_deref() == Some(version.as_str());

                if !is_skipped && !from_cache {
                    // API returned a fresh version — spawn for it.
                    let method = util::install_method::InstallMethod::detect();
                    if method.can_self_update() && method.can_write_binary() {
                        let _ = util::self_update::spawn_background_download(version);
                    } else if method.can_auto_run_package_manager() {
                        let _ = util::check_update::spawn_package_manager_update(method);
                    }
                }
            }

            // Never refresh `last_update_check` when we only reused a cached
            // pending version. Otherwise frequent CLI invocations can keep
            // pushing the 12-hour window forward and prevent a real API check
            // from discovering newer hotfixes.
            //
            // Fresh API results are already persisted inside `check_update()`;
            // cache hits and API failures should remain read-only here.
        }

        Ok(latest_version)
    })
}

/// Waits for the background update task to finish, but no longer than a
/// couple of seconds so that short-lived commands are not noticeably delayed.
/// The heavy download work runs in a detached process, so this timeout only
/// gates the fast version-check API call.
async fn handle_update_task(
    handle: Option<tokio::task::JoinHandle<anyhow::Result<Option<String>>>>,
) {
    use std::time::Duration;

    if let Some(handle) = handle {
        match tokio::time::timeout(Duration::from_secs(1), handle).await {
            Ok(Ok(Ok(_))) => {}
            Ok(Ok(Err(_))) | Ok(Err(_)) => {} // update error or task panic — non-fatal
            Err(_) => {} // timeout — the API check was slow; next invocation retries
        }
    }
}

/// Runs in a detached child process to download and stage an update.
async fn background_stage_update(version: &str) -> Result<()> {
    use util::check_update::UpdateCheck;

    let result = async {
        if telemetry::is_auto_update_disabled() {
            return Ok(());
        }

        match util::self_update::download_and_stage(version).await {
            Ok(true) => {}  // Staged successfully; cache stays until try_apply_staged() succeeds.
            Ok(false) => {} // Lock held by another process, will retry
            Err(_) => UpdateCheck::record_download_failure(),
        }
        Ok(())
    }
    .await;

    if let Ok(pid_path) = util::self_update::download_update_pid_path() {
        let _ = std::fs::remove_file(pid_path);
    }

    result
}

#[tokio::main]
async fn main() -> Result<()> {
    // Internal: detached background download spawned by a prior invocation.
    if let Ok(version) = std::env::var(consts::RAILWAY_STAGE_UPDATE_ENV) {
        return background_stage_update(&version).await;
    }

    let args = build_args().try_get_matches();
    let is_tty = std::io::stdout().is_terminal();
    // Help, version, and parse-error paths are read-only: no staged-binary
    // apply, no background update spawn, no extra latency.
    let is_help_or_error = args.as_ref().is_err();

    // Peek at the subcommand early so we can skip the staged-update
    // apply and background updater when the user is explicitly managing
    // updates (`railway upgrade` or `railway autoupdate`).
    // Check raw args too so that help/error paths (where clap returns Err)
    // are also detected — e.g. `railway upgrade --help` should not apply
    // a staged update as a side effect.
    let raw_subcommand = std::env::args().nth(1).filter(|a| !a.starts_with('-'));

    let is_update_management_cmd = matches!(
        raw_subcommand.as_deref(),
        Some("upgrade" | "autoupdate" | "check_updates" | "check-updates")
    );
    // Bare `railway` and `railway help` show help — treat as read-only so
    // first-time users don't trigger update side effects.
    let is_read_only_invocation = is_help_or_error
        || raw_subcommand.is_none()
        || matches!(raw_subcommand.as_deref(), Some("help"));
    let auto_update_enabled = !telemetry::is_auto_update_disabled();

    // Non-TTY invocations are a supported path for coding agents and other
    // automated CLI users. They are allowed to refresh the update cache and
    // kick off background installs, but we keep staged-binary apply TTY-only
    // so the running binary never changes under a scripted invocation.
    let auto_applied_version =
        if auto_update_enabled && is_tty && !is_update_management_cmd && !is_read_only_invocation {
            util::self_update::try_apply_staged()
        } else {
            None
        };

    let update = UpdateCheck::read_normalized();
    let skipped_version = update.skipped_version.clone();
    let check_gate_armed = update
        .last_update_check
        .map(|t| (chrono::Utc::now() - t) < chrono::Duration::hours(12))
        .unwrap_or(false);

    // Pass any pending version to spawn_update_task so it can skip the
    // 12h short-circuit and retry a download that timed out in a
    // prior run.  The background task clears latest_version on success.
    //
    // If the running binary has already caught up to (or surpassed) the
    // cached version, clear the stale cache so spawn_update_task falls
    // through to a fresh check_update() and can discover newer releases.
    let known_pending = update.latest_version;

    // Show the "new version available" banner only for TTY users. Coding
    // agents and other non-interactive callers should still refresh update
    // state in the background, but they should not receive human-facing
    // upgrade prompts in command output.
    //
    // When auto-update is disabled via preference, we still show the banner
    // to cautious interactive users who want release visibility. Suppress it
    // when disabled via env var or CI, where extra output is noise.
    let env_or_ci_suppressed = telemetry::is_auto_update_disabled_by_env() || Configs::env_is_ci();
    if is_tty && !env_or_ci_suppressed {
        if let Some(ref latest_version) = known_pending {
            let is_skipped = skipped_version.as_deref() == Some(latest_version.as_str());
            if !is_skipped
                && matches!(
                    compare_semver(env!("CARGO_PKG_VERSION"), latest_version),
                    Ordering::Less
                )
            {
                eprintln!(
                    "{} v{} visit {} for more info",
                    "New version available:".green().bold(),
                    latest_version.yellow(),
                    "https://docs.railway.com/guides/cli".purple(),
                );
            }
        }
    }

    // Spawn the background version check for all invocations (including
    // non-TTY) so the version cache stays fresh for both humans and coding
    // agents. Non-TTY callers are a first-class auto-update path: they may
    // trigger background downloads/package-manager installs, but staged-binary
    // apply and user-facing banners remain TTY-only.
    let check_updates_handle = if is_update_management_cmd || is_read_only_invocation {
        None
    } else {
        Some(spawn_update_task(
            known_pending,
            auto_update_enabled,
            skipped_version,
            check_gate_armed,
        ))
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

    // Commands that do not require authentication -- skip token refresh for these.
    const NO_AUTH_COMMANDS: &[&str] = &[
        "login",
        "logout",
        "completion",
        "docs",
        "upgrade",
        "autoupdate",
        "telemetry_cmd",
        "check_updates",
    ];

    let needs_refresh = cli
        .subcommand_name()
        .map(|cmd| !NO_AUTH_COMMANDS.contains(&cmd))
        .unwrap_or(false);

    if needs_refresh {
        if let Ok(mut configs) = Configs::new() {
            if let Err(e) = client::ensure_valid_token(&mut configs).await {
                eprintln!("{}: {e}", "Warning: failed to refresh OAuth token".yellow());
            }
        }
    }

    let exec_result = exec_cli(cli).await;

    // Send telemetry for silent auto-update apply (after auth is available).
    if let Some(ref version) = auto_applied_version {
        telemetry::send(telemetry::CliTrackEvent {
            command: "autoupdate_apply".to_string(),
            sub_command: Some(version.clone()),
            success: true,
            error_message: None,
            duration_ms: 0,
            cli_version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            is_ci: Configs::env_is_ci(),
        })
        .await;
    }

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
        }

        #[test]
        fn variable_aliases() {
            assert_subcommand(&["variable"], "variable");
            assert_subcommand(&["variables"], "variable");
            assert_subcommand(&["vars"], "variable");
            assert_subcommand(&["var"], "variable");
        }

        #[test]
        fn logs_http_flag_parses() {
            assert_parses(&["logs", "--http"]);
            assert_parses(&["logs", "--http", "--lines", "50"]);
            assert_parses(&["service", "logs", "--http"]);
        }

        #[test]
        fn logs_http_examples_parse() {
            assert_parses(&["logs", "--http", "--lines", "50"]);
            assert_parses(&[
                "logs",
                "--http",
                "--filter",
                "@path:/api/users @httpStatus:200",
            ]);
            assert_parses(&[
                "logs",
                "--http",
                "--json",
                "--filter",
                "@requestId:abcd1234",
            ]);
            assert_parses(&[
                "service",
                "logs",
                "--http",
                "--lines",
                "10",
                "--filter",
                "@httpStatus:404",
            ]);
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
    }
}
