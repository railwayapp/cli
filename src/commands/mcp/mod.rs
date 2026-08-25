use super::*;
use rmcp::{ServiceExt, transport::stdio};

mod handler;
pub(crate) mod install;
pub(crate) mod params;
mod proxy;
mod tools;
use handler::RailwayMcp;

/// Connects AI agents to Railway's MCP server, or installs the MCP config into AI coding tools.
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,
}

#[derive(Parser)]
enum Commands {
    /// Install Railway's MCP server config into AI coding tools (Claude Code, Cursor, OpenCode, Codex)
    Install(install::Args),
    /// Serve the in-process MCP server over stdio, without reaching mcp.railway.com
    Local,
    /// Proxy the remote MCP server over stdio — what a bare `railway mcp` now does
    Proxy,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        // A bare `railway mcp` bridges to mcp.railway.com. The remote surface
        // is the one that gets new tools, and it fails at a lower rate than
        // the in-process server; the proxy reproduces the in-process server's
        // one real advantage by completing a `railway link` project on calls
        // that name no scope of their own.
        //
        // `mcp local` keeps the in-process server for anything that cannot
        // reach mcp.railway.com, and `mcp proxy` still resolves here so
        // configs written before the cutover keep working.
        None | Some(Commands::Proxy) => proxy::serve_proxy().await,
        Some(Commands::Local) => serve_stdio().await,
        Some(Commands::Install(install_args)) => install::command(install_args).await,
    }
}

async fn serve_stdio() -> Result<()> {
    let configs = Configs::new()?;
    // Start even when there are no usable credentials. Refusing to boot makes
    // the harness report an opaque "MCP server failed" that no later
    // `railway login` can clear, because the process is already gone. Serving
    // with an unauthenticated client instead means tool calls return an
    // actionable auth error, and `refresh_credentials` swaps in an authorized
    // client as soon as the user signs in — no editor restart.
    let client = match GQLClient::new_authorized(&configs) {
        Ok(client) => client,
        Err(_) => GQLClient::new_public()?,
    };
    let handler = RailwayMcp::new(client, configs);

    let service = handler
        .serve(stdio())
        .await
        .context("Failed to start MCP server")?;

    service.waiting().await?;

    Ok(())
}
