macro_rules! commands_enum {
    ($($module:tt),*) => (
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
