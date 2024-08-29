use std::time::Duration;

use anyhow::bail;
use colored::Colorize;
use is_terminal::IsTerminal;
use queries::domains::DomainsDomains;

use crate::{
    consts::TICK_STRING,
    controllers::project::{ensure_project_and_environment_exist, get_project},
    errors::RailwayError,
};

use super::*;

/// Generates a domain for a service if there is not a railway provided domain
// Checks if the user is linked to a service, if not, it will generate a domain for the default service
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    if project.services.edges.is_empty() {
        return Err(RailwayError::NoServices.into());
    }

    // If there is only one service, it will generate a domain for that service
    let service = if project.services.edges.len() == 1 {
        project.services.edges[0].node.clone().id
    } else {
        let Some(service) = linked_project.service.clone() else {
            bail!("No service linked. Run `railway service` to link to a service");
        };
        if project.services.edges.iter().any(|s| s.node.id == service) {
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

    let domains = post_graphql::<queries::Domains, _>(&client, configs.get_backboard(), vars)
        .await?
        .domains;

    let domain_count = domains.service_domains.len() + domains.custom_domains.len();

    if domain_count > 0 {
        return print_existing_domains(&domains);
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

        let domain = post_graphql::<mutations::ServiceDomainCreate, _>(
            &client,
            configs.get_backboard(),
            vars,
        )
        .await?
        .service_domain_create
        .domain;

        spinner.finish_and_clear();

        let formatted_domain = format!("https://{}", domain);
        println!(
            "Service Domain created:\n🚀 {}",
            formatted_domain.magenta().bold()
        );
    } else {
        println!("Creating domain...");

        let domain = post_graphql::<mutations::ServiceDomainCreate, _>(
            &client,
            configs.get_backboard(),
            vars,
        )
        .await?
        .service_domain_create
        .domain;

        let formatted_domain = format!("https://{}", domain);
        println!(
            "Service Domain created:\n🚀 {}",
            formatted_domain.magenta().bold()
        );
    }

    Ok(())
}

fn print_existing_domains(domains: &DomainsDomains) -> Result<()> {
    println!("Domains already exists on the service:");
    let domain_count = domains.service_domains.len() + domains.custom_domains.len();

    if domain_count == 1 {
        let domain = domains
            .service_domains
            .first()
            .map(|d| d.domain.clone())
            .unwrap_or_else(|| {
                domains
                    .custom_domains
                    .first()
                    .map(|d| d.domain.clone())
                    .unwrap_or_else(|| unreachable!())
            });

        let formatted_domain = format!("https://{}", domain);
        println!("🚀 {}", formatted_domain.magenta().bold());
        return Ok(());
    }

    for domain in &domains.custom_domains {
        let formatted_domain = format!("https://{}", domain.domain);
        println!("- {}", formatted_domain.magenta().bold());
    }
    for domain in &domains.service_domains {
        let formatted_domain = format!("https://{}", domain.domain);
        println!("- {}", formatted_domain.magenta().bold());
    }

    Ok(())
}
