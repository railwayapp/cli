use anyhow::Result;
use clap::Parser;

use crate::{client::GQLClient, config::Configs};

mod common;
mod keys;
mod native;

use common::*;

/// Connect to a service via SSH or manage SSH keys
#[derive(Parser, Clone)]
pub struct Args {
    #[clap(subcommand)]
    subcommand: Option<Commands>,

    /// Project to connect to (defaults to linked project)
    #[clap(short, long)]
    project: Option<String>,

    /// Service to connect to (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to connect to (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Deployment instance ID to connect to (defaults to first active instance)
    #[clap(short, long)]
    #[arg(long = "deployment-instance", value_name = "deployment-instance-id")]
    deployment_instance: Option<String>,

    /// SSH into the service inside a tmux session. Installs tmux if not present. Optionally provide a session name (--session name)
    #[clap(long, value_name = "SESSION_NAME", default_missing_value = "railway", num_args = 0..=1)]
    session: Option<String>,

    /// Deprecated: native SSH is now the default, this flag has no effect
    #[clap(long, hide = true)]
    native: bool,

    /// Command to execute instead of starting an interactive shell
    #[clap(trailing_var_arg = true)]
    command: Vec<String>,
}

#[derive(Parser, Clone)]
enum Commands {
    /// Manage SSH keys registered with Railway
    Keys(keys::Args),
}

pub async fn command(args: Args) -> Result<()> {
    if let Some(Commands::Keys(keys_args)) = args.subcommand {
        return keys::command(keys_args).await;
    }

    if args.native {
        eprintln!(
            "Warning: --native flag is deprecated and has no effect; native SSH is now the default."
        );
    }

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    native::ensure_ssh_key(&client, &configs).await?;

    let ssh_target = if let Some(ref instance_id) = args.deployment_instance {
        instance_id.clone()
    } else {
        let params = get_ssh_connect_params(args.clone(), &configs, &client).await?;
        native::get_service_instance_id(
            &client,
            &configs,
            &params.environment_id,
            &params.service_id,
        )
        .await?
    };

    if let Some(session_name) = args.session {
        return native::run_native_ssh_with_session(&ssh_target, &session_name);
    }

    let command = if args.command.is_empty() {
        None
    } else {
        Some(args.command.as_slice())
    };
    let exit_code = native::run_native_ssh(&ssh_target, command)?;
    if exit_code != 0 {
        std::process::exit(exit_code);
    }

    Ok(())
}
