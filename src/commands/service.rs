use std::fmt::Display;

use anyhow::bail;

use crate::consts::SERVICE_NOT_FOUND;

use super::{queries::project::ProjectProjectServicesEdgesNode, *};

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
        .map(|s| Service(&s.node))
        .collect();

    if let Some(service) = args.service {
        let service = services
            .iter()
            .find(|s| s.0.id == service || s.0.name == service)
            .context("Service not found")?;

        configs.link_service(service.0.id.clone())?;
        configs.write()?;
        return Ok(());
    }

    if services.is_empty() {
        bail!("No services found");
    }

    let service = inquire::Select::new("Select a service", services)
        .with_render_config(Configs::get_render_config())
        .prompt()?;

    configs.link_service(service.0.id.clone())?;
    configs.write()?;
    Ok(())
}

#[derive(Debug, Clone)]
struct Service<'a>(&'a ProjectProjectServicesEdgesNode);

impl<'a> Display for Service<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}
