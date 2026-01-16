use std::{fmt::Display, time::Duration};

use crate::{
    consts::TICK_STRING,
    controllers::project::get_project,
    errors::RailwayError,
    util::{
        prompt::{
            PromptServiceInstance, fake_select, prompt_multi_options, prompt_options_skippable,
            prompt_text,
        },
        retry::{RetryConfig, retry_with_backoff},
    },
};
use anyhow::bail;
use is_terminal::IsTerminal;

use super::{queries::project::ProjectProjectEnvironmentsEdgesNode, *};

mod changes;
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
    #[allow(clippy::large_enum_variant)]
    enum Commands {
        /// Create a new environment
        New(pub struct {
            /// The name of the environment to create
            pub name: Option<String>,

            /// The name/ID of the environment to duplicate
            #[clap(long, short, visible_alias = "copy", visible_short_alias = 'c')]
            pub duplicate: Option<String>,

            #[clap(flatten)]
            pub config: EnvironmentConfigOptions,
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

#[derive(Parser, Clone, Debug, Default)]
pub struct EnvironmentConfigOptions {
    /// Configure a service using dot-path notation
    ///
    /// Format: --service-config <SERVICE> <PATH> <VALUE>
    ///
    /// Examples:
    ///   --service-config backend variables.API_KEY.value "secret"
    ///   --service-config api deploy.startCommand "npm start"
    ///   --service-config web source.image "nginx:latest"
    #[clap(long = "service-config", short = 's', number_of_values = 3, action = clap::ArgAction::Append, value_names = &["SERVICE", "PATH", "VALUE"])]
    pub service_configs: Vec<String>,

    /// Set a variable on a service (shorthand for --service-config <SERVICE> variables.<KEY>.value <VALUE>)
    ///
    /// Format: --service-variable <SERVICE> <KEY>=<VALUE>
    #[clap(long = "service-variable", short = 'v', number_of_values = 2, action = clap::ArgAction::Append, value_names = &["SERVICE", "KEY=VALUE"])]
    pub service_variables: Vec<String>,
}

impl EnvironmentConfigOptions {
    /// Get all service configs, including those converted from --service-variable
    pub fn get_all_service_configs(&self) -> Vec<String> {
        let mut configs = self.service_configs.clone();

        // Convert --service-variable entries to --service-config format
        // --service-variable <SERVICE> <KEY>=<VALUE>
        // becomes: <SERVICE> variables.<KEY>.value <VALUE>
        for chunk in self.service_variables.chunks(2) {
            if chunk.len() == 2 {
                let service = &chunk[0];
                let key_value = &chunk[1];

                if let Some((key, value)) = key_value.split_once('=') {
                    configs.push(service.clone());
                    configs.push(format!("variables.{}.value", key));
                    configs.push(value.to_string());
                }
            }
        }

        configs
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
