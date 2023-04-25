use anyhow::bail;

use crate::{
    controllers::project::get_project,
    errors::RailwayError,
    util::prompt::{prompt_select, PromptService},
};

use super::*;

/// Link a service to the current project
#[derive(Parser)]
pub struct Args {
    /// The service to link
    service: Option<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let services: Vec<_> = project
        .services
        .edges
        .iter()
        .map(|s| PromptService(&s.node))
        .collect();

    if let Some(service) = args.service {
        let service = services
            .iter()
            .find(|s| s.0.id == service || s.0.name == service)
            .ok_or_else(|| RailwayError::ServiceNotFound(service))?;

        configs.link_service(service.0.id.clone())?;
        configs.write()?;
        return Ok(());
    }

    if services.is_empty() {
        bail!("No services found");
    }

    let service = prompt_select("Select a service", services)?;

    configs.link_service(service.0.id.clone())?;
    configs.write()?;
    Ok(())
}
