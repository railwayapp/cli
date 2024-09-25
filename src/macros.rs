#[macro_export]
macro_rules! commands_enum {
    // Case when command has aliases (e.g. add(a, b, c))
    ($($module:ident $(($($alias:ident),*))?),*) => (
        paste::paste! {
            #[derive(Subcommand)]
            enum Commands {
                $(
                    #[clap(
                        $(aliases = &[$( stringify!($alias) ),*])?
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

/// Ensure running in a terminal or bail with the provided message
#[macro_export]
macro_rules! interact_or {
    ($message:expr) => {
        if !std::io::stdout().is_terminal() {
            use anyhow::bail;
            bail!($message);
        }
    };
}
