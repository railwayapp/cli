//! `railway ca herdr`: Railway cloud agents as herdr saved machines.
//!
//! herdr 0.9 can hold several SSH machines in one window, each running its own
//! herdr server. A cloud agent VM is exactly such a machine: the server on the
//! VM owns the panes and detects the coding agent natively, and after a sleep
//! herdr's own restore brings the layout and `claude --resume` back. What
//! herdr cannot do is create, wake, sleep or delete the VM, so these verbs are
//! the control plane, and the herdr plugin is a generated manifest whose every
//! command calls one of them.

mod agents;
mod attention;
mod bootstrap;
mod harness;
mod herdr_cli;
mod install;
mod known_hosts;
mod new;
mod relay;
mod state;
mod sync;
mod target;

use anyhow::Result;
use clap::Parser;

pub const PLUGIN_ID: &str = "railway.ca";

#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Parser)]
enum Command {
    /// Register the herdr plugin: write its manifest and link it
    Install(install::Args),

    /// Create a cloud agent and add it to herdr as a machine
    New(new::Args),

    /// Pick an agent: connect, sleep, wake, delete
    Agents(agents::Args),

    /// Reconcile herdr's saved machines with your cloud agents
    Sync(sync::Args),

    /// Prepare an agent's VM for herdr: integrations, config, workspace
    Bootstrap(bootstrap::Args),
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Command::Install(a) => install::command(a).await,
        Command::New(a) => new::command(a).await,
        Command::Agents(a) => agents::command(a).await,
        Command::Sync(a) => sync::command(a).await,
        Command::Bootstrap(a) => bootstrap::command(a).await,
    }
}

/// `~/.railway/herdr-plugin`: the manifest herdr links, and the plugin's state.
pub fn plugin_dir() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to get home directory"))?;
    Ok(home.join(".railway").join("herdr-plugin"))
}
