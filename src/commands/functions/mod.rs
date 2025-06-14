use super::*;
use crate::{
    client::GQLClient, config::Configs, controllers::project::get_project, errors::RailwayError,
};
use std::path::PathBuf;

/// Manage project functions
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Environment ID/name
    #[clap(long, short)]
    environment: Option<String>,
}

mod common;
mod delete;
mod list;
mod new;

structstruck::strike! {
    #[strikethrough[derive(Parser)]]
    enum Commands {
        /// List functions
        #[clap(visible_alias = "ls")]
        List,

        /// Add a new function
        #[clap(visible_alias = "create")]
        New(struct {
            /// The path to the function locally
            #[clap(long, short)]
            path: Option<PathBuf>,

            /// The name of the function
            #[clap(long, short)]
            name: Option<String>,

            /// Cron schedule to run the function
            #[clap(long, short)]
            cron: Option<String>,

            /// Generate a domain
            #[clap(long, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
            http: Option<bool>,

            /// Serverless (a.k.a sleeping)
            #[clap(long, short, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
            serverless: Option<bool>,

            /// Watch for changes of the file and deploy upon save
            #[clap(long, short, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
            watch: Option<bool>,
        }),

        /// Delete a function
        #[clap(visible_alias = "remove", visible_alias = "rm")]
        Delete(struct {
            /// The ID/name of the function you wish to delete
            #[clap(long, short)]
            function: Option<String>,

            /// Skip confirmation for deleting
            #[clap(long, short, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
            yes: Option<bool>
        }),

        /// Test a function locally (requires Docker)
        #[clap(visible_alias = "t")]
        Test(struct {
            /// Watch for changes of the file and re-run upon save
            #[clap(long, short)]
            watch: bool
        })

        /// Push a new change to the function
        #[clap(visible_alias = "up")]
        Push(struct {
            /// Watch for changes of the file and automatically push
            #[clap(long, short)]
            watch: bool
        })

        /// Pull changes from the linked function remotely
        Pull,

        /// Link a function manually
        Link(struct {
            /// The path to the file
            #[clap(long, short)]
            path: PathBuf,

            /// The ID/name of the function you wish to link to
            #[clap(long, short)]
            function: Option<String>
        })
    }
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;
    let environment_input = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());
    let environment = project
        .environments
        .edges
        .iter()
        .find(|e| {
            (e.node.id.to_lowercase() == environment_input.to_lowercase())
                || (e.node.name.to_lowercase() == environment_input.to_lowercase())
        })
        .ok_or_else(|| RailwayError::EnvironmentNotFound(environment_input))?;

    match args.command {
        Commands::List => list::list(environment, project.clone()).await,
        Commands::New(args) => new::new(environment, project.clone(), args).await,
        Commands::Delete(args) => delete::delete(environment, project.clone(), args).await,
        _ => unreachable!(),
    }?;

    Ok(())
}
