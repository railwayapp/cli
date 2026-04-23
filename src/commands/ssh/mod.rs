use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use crate::{client::GQLClient, config::Configs};

mod common;
mod keys;
mod native;
mod tel;

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

    /// Path to identity (private key) file to use, like `ssh -i`.
    /// Skips the local ~/.ssh scan; forwarded directly to ssh.
    #[clap(short = 'i', long = "identity-file", value_name = "PATH")]
    identity_file: Option<PathBuf>,

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

    if args.identity_file.is_none() {
        tel::track("key_setup", native::ensure_ssh_key(&client, &configs).await).await?;
    }

    let ssh_target = if let Some(ref instance_id) = args.deployment_instance {
        instance_id.clone()
    } else {
        let params = tel::track(
            "resolve_target",
            get_ssh_connect_params(args.clone(), &configs, &client).await,
        )
        .await?;
        tel::track(
            "instance_lookup",
            native::get_service_instance_id(
                &client,
                &configs,
                &params.environment_id,
                &params.service_id,
            )
            .await,
        )
        .await?
    };

    let identity_file = args.identity_file.as_deref();

    if let Some(session_name) = args.session {
        tel::track(
            "tmux_install",
            native::ensure_tmux_installed(&ssh_target, identity_file),
        )
        .await?;
        return tel::track(
            "session_connect",
            native::run_tmux_session(&ssh_target, &session_name, identity_file),
        )
        .await;
    }

    let command = if args.command.is_empty() {
        None
    } else {
        Some(args.command.as_slice())
    };
    let exit_code = tel::track(
        "spawn",
        native::run_native_ssh(&ssh_target, command, identity_file),
    )
    .await?;
    if exit_code != 0 {
        // ssh::command is about to std::process::exit, which bypasses the
        // global telemetry hook in the commands! macro. Report the failure
        // here so connection errors (ssh's 255, auth failures, remote command
        // exits) still show up in CLI telemetry.
        tel::report_failure("exit_nonzero", &format!("ssh exited with code {exit_code}")).await;
        std::process::exit(exit_code);
    }

    Ok(())
}
