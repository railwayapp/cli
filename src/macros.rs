#[macro_export]
macro_rules! commands {
    ($($module:ident $(($($alias:ident),*))?),*) => {
        pastey::paste! {
            /// Build the global CLI (root command) and attach module subcommands.
            pub fn build_args() -> clap::Command {
                // Use your desired root name here (for example, "railway" rather than a derived name).
                let mut cmd = clap::Command::new("railway")
                    .about("Railway CLI")
                    .author(clap::crate_authors!())
                    .propagate_version(true)
                    .about(clap::crate_description!())
                    .long_about(None)
                    .version(clap::crate_version!());
                $(
                    {
                        // Get the subcommand as defined by the module.
                        let sub = <$module::Args as ::clap::CommandFactory>::command();
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
                            s = s.name(stringify!($module));
                            s
                        };
                        // Add this subcommand into the global CLI.
                        cmd = cmd.subcommand(sub);
                    }
                )*
                cmd
            }

            /// Dispatches the selected subcommand (after parsing) to its handler.
            pub async fn exec_cli(matches: clap::ArgMatches) -> anyhow::Result<()> {
                match matches.subcommand() {
                    $(
                        Some((stringify!([<$module:snake>]), sub_matches)) => {
                            let subcommand_name = sub_matches.subcommand_name().map(|s| s.to_string());
                            let args = <$module::Args as ::clap::FromArgMatches>::from_arg_matches(sub_matches)
                                .map_err(anyhow::Error::from)?;
                            let start = ::std::time::Instant::now();
                            let result = $module::command(args).await;
                            let duration = start.elapsed();
                            $crate::telemetry::send($crate::telemetry::CliTrackEvent {
                                command: stringify!([<$module:snake>]).to_string(),
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
