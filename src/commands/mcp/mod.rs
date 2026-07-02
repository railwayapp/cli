use super::*;
use rmcp::{ServiceExt, transport::stdio};

mod handler;
pub(crate) mod install;
pub(crate) mod params;
mod tools;
use handler::RailwayMcp;

/// Starts a local MCP server for AI-agent access, or installs the MCP config into AI coding tools.
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,
}

#[derive(Parser)]
enum Commands {
    /// Install Railway's MCP server config into AI coding tools (Claude Code, Cursor, OpenCode, Codex, Pi)
    Install(install::Args),
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        None => serve_stdio().await,
        Some(Commands::Install(install_args)) => install::command(install_args).await,
    }
}

async fn serve_stdio() -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let handler = RailwayMcp::new(client, configs);

    let service = handler
        .serve(stdio())
        .await
        .context("Failed to start MCP server")?;

    service.waiting().await?;

    Ok(())
}
