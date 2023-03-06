use std::time::Duration;

use anyhow::bail;
use is_terminal::IsTerminal;

use crate::consts::TICK_STRING;

use super::*;

/// Generates a domain for a service if there is not a railway provided domain
// Checks if the user is linked to a service, if not, it will generate a domain for the default service
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };

    let res = post_graphql::<queries::Project, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;

    if body.project.services.edges.is_empty() {
        bail!("No services found for project");
    }

    // If there is only one service, it will generate a domain for that service
    let service = if body.project.services.edges.len() == 1 {
        body.project.services.edges[0].node.clone().id
    } else {
        let Some(service) = linked_project.service.clone() else {
            bail!("No service linked. Run `railway service` to link to a service");
        };
        if body
            .project
            .services
            .edges
            .iter()
            .any(|s| s.node.id == service)
        {
            service
        } else {
            bail!("Service not found! Run `railway service` to link to a service");
        }
    };

    let vars = queries::domains::Variables {
        project_id: linked_project.project.clone(),
        environment_id: linked_project.environment.clone(),
        service_id: service.clone(),
    };

    let res = post_graphql::<queries::Domains, _>(&client, configs.get_backboard(), vars).await?;

    let body = res
        .data
        .context("Failed to retrieve to get domains for service.")?;

    let domain = body.domains;
    if !(domain.service_domains.is_empty() || domain.custom_domains.is_empty()) {
        bail!("Domain already exists on service");
    }

    let vars = mutations::service_domain_create::Variables {
        service_id: service,
        environment_id: linked_project.environment.clone(),
    };

    if std::io::stdout().is_terminal() {
        let spinner = indicatif::ProgressBar::new_spinner()
            .with_style(
                indicatif::ProgressStyle::default_spinner()
                    .tick_chars(TICK_STRING)
                    .template("{spinner:.green} {msg}")?,
            )
            .with_message("Creating domain...");
        spinner.enable_steady_tick(Duration::from_millis(100));

        let res = post_graphql::<mutations::ServiceDomainCreate, _>(
            &client,
            configs.get_backboard(),
            vars,
        )
        .await?;

        let body = res.data.context("Failed to create service domain.")?;
        let domain = body.service_domain_create.domain;

        spinner.finish_and_clear();

        println!("Service Domain created: {}", domain.bold());
    } else {
        println!("Creating domain...");

        let res = post_graphql::<mutations::ServiceDomainCreate, _>(
            &client,
            configs.get_backboard(),
            vars,
        )
        .await?;

        let body = res.data.context("Failed to create service domain.")?;
        let domain = body.service_domain_create.domain;

        println!("Service Domain created: {}", domain.bold());
    }

    Ok(())
}
