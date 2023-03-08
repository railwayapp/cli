use anyhow::bail;

use crate::{
    consts::SERVICE_NOT_FOUND,
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

    let vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };

    let res = post_graphql::<queries::Project, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;

    let services: Vec<_> = body
        .project
        .services
        .edges
        .iter()
        .map(|s| PromptService(&s.node))
        .collect();

    if let Some(service) = args.service {
        let service = services
            .iter()
            .find(|s| s.0.id == service || s.0.name == service)
            .context(SERVICE_NOT_FOUND)?;

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
