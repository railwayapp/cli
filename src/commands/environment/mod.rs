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
/*
TODO: railway env edit:
- allow input from STDIN in JSON of environment config (see PR from JR)
- a --message flag for setting a patch message
- a --stage option to not commit immediately
*/

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
    #[allow(clippy::large_enum_variant)]
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
            #[clap(long = "service-source", number_of_values = 3, value_names = &["SERVICE", "PLATFORM", "SOURCE"])]
            pub service_sources: Vec<String>,

            /// Assign a service a new start command
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-start-command <service name/service uuid> bun run start
            #[clap(long = "service-start-command", number_of_values = 2, value_names = &["SERVICE", "CMD"])]
            pub service_start_commands: Vec<String>,

            /// Assign a service a healthcheck endpoint & timeout (in seconds)
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-healthcheck <service name/service uuid> /health 60
            #[clap(long = "service-healthcheck", number_of_values = 3, value_names = &["SERVICE", "ENDPOINT", "TIMEOUT"])]
            pub service_healthchecks: Vec<String>,

            /// Assign a service a restart policy (and if needed, a restart policy maximum retries)
            ///
            /// Max retries is only needed if the policy specified is on_failure
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-restart-policy <service name/service uuid> on_failure 10
            ///
            /// railway environment new foo --duplicate bar --service-restart-policy <service name/service uuid> always
            #[clap(long = "service-restart-policy", num_args = 2..=3, value_names = &["SERVICE", "POLICY", "[MAX_RETRIES]"])]
            pub service_restart_policies: Vec<String>,

            /// Assign a service a cron schedule
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-cron <service name/service uuid> 0 * * * *
            #[clap(long = "service-cron", number_of_values = 2, value_names = &["SERVICE", "CRON"])]
            pub service_crons: Vec<String>,

            /// Enable/disable service sleeping on a service
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-sleeping <service name/service uuid> true/false/yes/no/y/n
            #[clap(long, number_of_values = 2, value_names = &["SERVICE", "ENABLED"])]
            pub service_sleeping: Vec<String>,

            /// Change the build command for a service
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-build-command <service name/service uuid> cargo build --release .
            #[clap(long = "service-build-command", number_of_values = 2, value_names = &["SERVICE", "CMD"])]
            pub service_build_commands: Vec<String>,

            /// Change the builder for a service (and, if supported, specify a path)
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-builder <service name/service uuid> railpack
            ///
            /// railway environment new foo --duplicate bar --service-builder <service name/service uuid> dockerfile ./path/to/Dockerfile
            ///
            /// railway environment new foo --duplicate bar --service-builder <service name/service uuid> nixpacks ./path/to/nixpacks/config.toml
            #[clap(long = "service-builder", num_args = 2..=3, value_names = &["SERVICE", "BUILDER", "[PATH]"])]
            pub service_builders: Vec<String>,

            /// Change the watch paths for a service. Patterns should be comma separated
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-watch-paths <service name/service uuid> "src/**/*.rs,!/*.md"
            #[clap(long, number_of_values = 2, value_names = &["SERVICE", "PATHS"])]
            pub service_watch_paths: Vec<String>,

            /// Change the root directory for a service (useful for monorepos)
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-root <service name/service uuid> packages/api
            #[clap(long = "service-root", number_of_values = 2, value_names = &["SERVICE", "PATH"])]
            pub service_roots: Vec<String>,

            /// Enable/disable waiting for GitHub check suites before deploying on a service
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-check-suites <service name/service uuid> true/false/yes/no/y/n
            #[clap(long, number_of_values = 2, value_names = &["SERVICE", "ENABLED"])]
            pub service_check_suites: Vec<String>,

            /// Configure auto-update type for a Docker image
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-auto-update <service name/service uuid> disabled/patch/minor
            #[clap(long = "service-auto-update", number_of_values = 2, value_names = &["SERVICE", "TYPE"])]
            pub service_auto_update_types: Vec<String>,

            /// Configure service regions and replicas
            ///
            /// Set replicas amount to 0 for a region to remove that region
            ///
            /// Examples:
            ///
            /// railway environment new foo --duplicate bar --service-region <service name/service uuid> europe-west4-drams3a 5
            #[clap(long = "service-region", number_of_values = 3, value_names = &["SERVICE", "REGION", "REPLICAS"])]
            pub service_regions: Vec<String>
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
