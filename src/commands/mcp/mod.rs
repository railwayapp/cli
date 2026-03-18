use super::*;
use rmcp::{ServiceExt, transport::stdio};

mod handler;
pub(crate) mod params;
mod tools;
use handler::RailwayMcp;

/// Starts a local MCP server for AI-agent access
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args) -> Result<()> {
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
