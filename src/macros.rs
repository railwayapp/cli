#[macro_export]
macro_rules! commands {
    (@name $module:ident as $name:literal) => {
        $name
    };
    (@name $module:ident) => {
        stringify!($module)
    };
    ($($module:ident $(as $name:literal)? $(($($alias:ident),*))?),*) => {
        pastey::paste! {
            /// Build the global CLI (root command) and attach module subcommands.
            pub fn build_args() -> clap::Command {
                // Use your desired root name here (for example, "railway" rather than a derived name).
                let mut cmd = clap::Command::new("railway")
                    .about("Railway CLI")
                    .author(clap::crate_authors!())
                    .propagate_version(true)
                    .about(concat!(
                        clap::crate_description!(),
                        "\n\nTip: Using an AI coding agent? Run `railway setup agent -y` to install Railway skills and the Railway MCP server."
                    ))
                    .long_about(None)
                    .version(clap::crate_version!());
                $(
                    {
                        let command_name = commands!(@name $module $(as $name)?);
                        // Get the subcommand as defined by the module.
                        let sub = <$crate::commands::$module::Args as ::clap::CommandFactory>::command();
                        // Allow the module to add dynamic arguments (if needed) and add any aliases.
                        let sub = {
                            let mut s = sub;
                            $(
                                $(
                                    s = s.visible_alias(stringify!($alias));
                                )*
                            )?
                            #[allow(unused_imports)]
                            {
                                // First import any definitions from the module.
                                // Then import everything (including the fallback).
                                use $crate::commands::get_dynamic_args;
                                {
                                    use $crate::commands::$module::*;
                                    s = get_dynamic_args(s);
                                }
                            }
                            s = s.name(command_name);
                            s
                        };
                        // Add this subcommand into the global CLI.
                        cmd = cmd.subcommand(sub.name(command_name));
                    }
                )*
                cmd = cmd
                    .mut_subcommand("list", |cmd| cmd.visible_alias("ls"))
                    .mut_subcommand("delete", |cmd| {
                        cmd.visible_alias("rm").visible_alias("remove")
                    })
                    .mut_subcommand("project", |cmd| cmd.visible_alias("projects"))
                    .mut_subcommand("bucket", |cmd| cmd.visible_alias("buckets"))
                    .mut_subcommand("volume", |cmd| cmd.visible_alias("volumes"))
                    .mut_subcommand("deployment", |cmd| cmd.visible_alias("deployments"))
                    .mut_subcommand("templates", |cmd| cmd.visible_alias("template"))
                    .mut_subcommand("check_updates", |cmd| cmd.visible_alias("check-updates"));
                cmd
            }

            /// Dispatches the selected subcommand (after parsing) to its handler.
            pub async fn exec_cli(matches: clap::ArgMatches) -> anyhow::Result<()> {
                match matches.subcommand() {
                    $(
                        Some((commands!(@name $module $(as $name)?), sub_matches)) => {
                            // Walk nested subcommand levels so telemetry can
                            // distinguish e.g. `sandbox template build` from
                            // `sandbox template status` ("template:build").
                            let subcommand_name = {
                                let mut parts: Vec<&str> = Vec::new();
                                let mut current = sub_matches;
                                while let Some((name, next)) = current.subcommand() {
                                    parts.push(name);
                                    current = next;
                                }
                                if parts.is_empty() { None } else { Some(parts.join(":")) }
                            };
                            let command_name = commands!(@name $module $(as $name)?);
                            let args = <$crate::commands::$module::Args as ::clap::FromArgMatches>::from_arg_matches(sub_matches)
                                .map_err(anyhow::Error::from)?;
                            let start = ::std::time::Instant::now();
                            let result = $crate::commands::$module::command(args).await;
                            let duration = start.elapsed();
                            $crate::telemetry::send($crate::telemetry::CliTrackEvent {
                                command: command_name.to_string(),
                                sub_command: subcommand_name,
                                success: result.is_ok(),
                                error_message: result.as_ref().err().map(|e| {
                                    let msg = format!("{e}");
                                    if msg.len() > 256 { msg[..256].to_string() } else { msg }
                                }),
                                duration_ms: duration.as_millis() as u64,
                                cli_version: env!("CARGO_PKG_VERSION"),
                                os: ::std::env::consts::OS,
                                arch: ::std::env::consts::ARCH,
                                is_ci: $crate::config::Configs::env_is_ci(),
                            }).await;
                            result?;
                        },
                    )*
                    _ => {
                        build_args().print_help()?;
                        println!();
                        // Bare `railway` shows the same root help as --help;
                        // mirror its agent-tooling health check.
                        $crate::commands::setup::print_agent_health_check();
                    }
                }
                Ok(())
            }
        }
    };
}

use is_terminal::IsTerminal;

pub fn is_stdout_terminal() -> bool {
    std::io::stdout().is_terminal()
}

/// Ensure running in a terminal or bail with the provided message
#[macro_export]
macro_rules! interact_or {
    ($message:expr) => {
        if !$crate::macros::is_stdout_terminal() {
            ::anyhow::bail!($message);
        }
    };
}
