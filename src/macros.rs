#[macro_export]
macro_rules! commands_enum {
    ($($module:ident),*) => (
      paste::paste! {
        #[derive(Subcommand)]
        enum Commands {
            $(
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

// Macro that bails if not running in a terminal
#[macro_export]
macro_rules! interact_or {
    ($message:expr) => {
        if !std::io::stdout().is_terminal() {
            use anyhow::bail;
            bail!($message);
        }
    };
}
