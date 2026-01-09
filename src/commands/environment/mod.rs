use std::{collections::HashMap, fmt::Display, str::FromStr, time::Duration};

use crate::{
    consts::TICK_STRING,
    controllers::{project::get_project, variables::Variable},
    errors::RailwayError,
    interact_or,
    util::{
        prompt::{
            PromptServiceInstance, fake_select, prompt_multi_options,
            prompt_multi_options_with_defaults, prompt_options, prompt_options_skippable,
            prompt_text, prompt_variables,
        },
        retry::{RetryConfig, retry_with_backoff},
    },
};
use anyhow::bail;
use derive_more::Display as DeriveDisplay;
use is_terminal::IsTerminal;
use strum::{Display, EnumDiscriminants, EnumIter, EnumString, IntoEnumIterator, VariantNames};

use super::{queries::project::ProjectProjectEnvironmentsEdgesNode, *};

mod delete;
mod link;
mod new;

/// Create, delete or link an environment
#[derive(Parser)]
pub struct Args {
    /// The environment to link to
    pub environment: Option<String>,

    #[clap(subcommand)]
    command: Option<Commands>,
}

structstruck::strike! {
    #[strikethrough[derive(Parser)]]
    enum Commands {
        /// Create a new environment
        New(pub struct {
            /// The name of the environment to create
            pub name: Option<String>,

            /// The name/ID of the environment to duplicate
            #[clap(long, short, visible_alias = "copy", visible_short_alias = 'c')]
            pub duplicate: Option<String>,

            /// Variables to assign in the new environment
            ///
            /// Note: This will only work if the environment is being duplicated, and that the service specified is present in the original environment
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-variable <service name/service uuid> BACKEND_PORT=3000
            #[clap(long = "service-variable", short = 'v', number_of_values = 2, value_names = &["SERVICE", "VARIABLE"])]
            pub service_variables: Vec<String>,

            /// Assign services new sources in the new environment
            ///
            /// GitHub repo format: <owner>/<repo>/<branch>
            ///
            /// Docker image format: [optional registry url]/<owner>[/repo][:tag]
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-source <service name/service uuid> docker ubuntu:latest
            ///
            /// railway environment new foo --duplicate bar --service-source <service name/service uuid> github nodejs/node/branch
            #[clap(long = "service-source", short = 's', number_of_values = 3, value_names = &["SERVICE", "PLATFORM", "SOURCE"])]
            pub service_sources: Vec<String>,
        }),

        /// Delete an environment
        Delete(pub struct {
            /// Skip confirmation dialog
            #[clap(short = 'y', long = "yes")]
            pub bypass: bool,

            /// The environment to delete
            pub environment: Option<String>,
        })

    }
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Some(Commands::New(args)) => new::new_environment(args).await,
        Some(Commands::Delete(args)) => delete::delete_environment(args).await,
        None => link::link_environment(args).await,
    }
}

#[derive(Debug, Clone)]
pub struct Environment<'a>(&'a ProjectProjectEnvironmentsEdgesNode);

impl Display for Environment<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}
