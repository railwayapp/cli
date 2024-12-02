#[macro_export]
macro_rules! commands_enum {
    // Case when command has aliases (e.g. add(a, b, c))
    ($($module:ident $(($($alias:ident),*))?),*) => (
        paste::paste! {
            #[derive(Subcommand)]
            enum Commands {
                $(
                    #[clap(
                        $(visible_aliases = &[$( stringify!($alias) ),*])?
                    )]
                    [<$module:camel>]($module::Args),
                )*
            }

            impl Commands {
                async fn exec(cli: Args) -> Result<()> {
                    match cli.command {
                        $(
                            Commands::[<$module:camel>](args) => $module::command(args, cli.json).await?,
                        )*
                    }
                    Ok(())
                }
            }
        }
    );
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

#[macro_export]
macro_rules! check_update {
    ($obj:expr, $force:expr) => {{
        let result = $obj.check_update($force).await;

        if let Ok(Some(latest_version)) = result {
            println!(
                "{} v{} visit {} for more info",
                "New version available:".green().bold(),
                latest_version.yellow(),
                "https://docs.railway.com/guides/cli".purple(),
            );
        }
    }};
}
