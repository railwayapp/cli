use anyhow::bail;

use crate::{
    controllers::project::{ensure_project_and_environment_exist, get_project},
    errors::RailwayError,
    util::prompt::{fake_select, prompt_options, PromptService},
};

use super::*;

/// Link a service to the current project
#[derive(Parser)]
pub struct Args {
    /// The service ID/name to link
    service: Option<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let services: Vec<_> = project
        .services
        .edges
        .iter()
        .filter(|a| {
            a.node
                .service_instances
                .edges
                .iter()
                .any(|b| b.node.environment_id == linked_project.environment)
        })
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

    let service = if !services.is_empty() {
        Some(if let Some(service) = args.service {
            let service_norm = services.iter().find(|s| {
                (s.0.name.to_lowercase() == service.to_lowercase())
                    || (s.0.id.to_lowercase() == service.to_lowercase())
            });
            if let Some(service) = service_norm {
                fake_select("Select a service", &service.0.name);
                service.clone()
            } else {
                return Err(RailwayError::ServiceNotFound(service).into());
            }
        } else {
            prompt_options("Select a service", services)?
        })
    } else {
        None
    };

    if let Some(service) = service {
        configs.link_service(service.0.id.clone())?;
        configs.write()?;
        println!("Linked service {}", service.0.name.green())
    } else {
        bail!("No service found");
    }
    Ok(())
}
