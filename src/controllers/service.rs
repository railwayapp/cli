use anyhow::{Context, Result, bail};
use is_terminal::IsTerminal;

use crate::{
    config::LinkedProject,
    queries::project::ProjectProject,
    util::prompt::{PromptService, prompt_select},
};

pub async fn get_or_prompt_service(
    linked_project: LinkedProject,
    project: ProjectProject,
    service_arg: Option<String>,
) -> Result<Option<String>> {
    let services = project.services.edges.iter().collect::<Vec<_>>();

    let service_id = if let Some(service_arg) = service_arg {
        // If the user specified a service, use that
        let service_id = services
            .iter()
            .find(|service| service.node.name == service_arg || service.node.id == service_arg);
        if let Some(service_id) = service_id {
            Some(service_id.node.id.to_owned())
        } else {
            bail!("Service not found");
        }
    } else if let Some(service) = linked_project.service {
        // If the user didn't specify a service, but we have a linked service, use that
        Some(service)
    } else {
        // If the user didn't specify a service, and we don't have a linked service, get the first service

        if services.is_empty() {
            // If there are no services, backboard will generate one for us
            None
        } else {
            // If there are multiple services, prompt the user to select one
            if std::io::stdout().is_terminal() {
                let prompt_services: Vec<_> =
                    services.iter().map(|s| PromptService(&s.node)).collect();
                let service = prompt_select("Select a service", prompt_services)
                    .context("Please specify a service via the `--service` flag.")?;
                Some(service.0.id.clone())
            } else {
                bail!("Multiple services found. Please specify a service via the `--service` flag.")
            }
        }
    };

    Ok(service_id)
}
