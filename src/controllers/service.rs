use anyhow::{Context, Result, bail};
use is_terminal::IsTerminal;

use crate::{
    config::LinkedProject,
    queries::project::ProjectProject,
    util::prompt::{PromptService, prompt_select},
};

/// Filter services to only those that have at least one instance in the given environment.
/// This ensures users only see services they can actually access in the current environment.
fn filter_services_by_environment<'a>(
    services: Vec<&'a crate::queries::project::ProjectProjectServicesEdges>,
    environment_id: &str,
) -> Vec<&'a crate::queries::project::ProjectProjectServicesEdges> {
    services
        .into_iter()
        .filter(|service| {
            service
                .node
                .service_instances
                .edges
                .iter()
                .any(|instance| instance.node.environment_id == environment_id)
        })
        .collect()
}

pub async fn get_or_prompt_service(
    linked_project: LinkedProject,
    project: ProjectProject,
    service_arg: Option<String>,
) -> Result<Option<String>> {
    get_or_prompt_service_for_environment(linked_project.clone(), project, service_arg, &linked_project.environment).await
}

pub async fn get_or_prompt_service_for_environment(
    linked_project: LinkedProject,
    project: ProjectProject,
    service_arg: Option<String>,
    environment_id: &str,
) -> Result<Option<String>> {
    let all_services = project.services.edges.iter().collect::<Vec<_>>();
    // Filter services to only those with instances in the current environment
    let services = filter_services_by_environment(all_services.clone(), environment_id);

    let service_id = if let Some(service_arg) = service_arg {
        // If the user specified a service, check in filtered services first
        let service_id = services
            .iter()
            .find(|service| service.node.name == service_arg || service.node.id == service_arg);
        if let Some(service_id) = service_id {
            Some(service_id.node.id.to_owned())
        } else {
            // Check if service exists but isn't accessible in this environment
            let exists_in_project = all_services
                .iter()
                .any(|service| service.node.name == service_arg || service.node.id == service_arg);
            if exists_in_project {
                bail!("Service '{}' exists but is not accessible in this environment. You may not have permission to access restricted environments.", service_arg);
            }
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
