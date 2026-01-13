use super::*;

/// Manage projects
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser)]
enum Commands {
    /// List all projects in your Railway account
    #[clap(alias = "ls")]
    List(crate::commands::list::Args),

    /// Link a project to the current directory
    Link(crate::commands::link::Args),

    /// Delete a project
    #[clap(alias = "rm", alias = "remove")]
    Delete(crate::commands::delete::Args),
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Commands::List(list_args) => crate::commands::list::command(list_args).await,
        Commands::Link(link_args) => crate::commands::link::command(link_args).await,
        Commands::Delete(delete_args) => crate::commands::delete::command(delete_args).await,
    }
}
