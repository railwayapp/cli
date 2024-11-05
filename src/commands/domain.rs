use std::time::Duration;

use anyhow::bail;
use colored::Colorize;
use create_custom_domain::create_custom_domain;
use is_terminal::IsTerminal;
use queries::domains::DomainsDomains;
use serde_json::json;

use crate::{
    consts::TICK_STRING,
    controllers::project::{ensure_project_and_environment_exist, get_project},
    errors::RailwayError,
    util::prompt::prompt_options,
};

use super::*;

/// Add a custom domain or generate a railway provided domain for a service.
///
/// There is a maximum of 1 railway provided domain per service.
#[derive(Parser)]
pub struct Args {
    /// The service to generate a domain for
    #[clap(short, long)]
    port: Option<u16>,

    /// Optionally, specify a custom domain to use. If not specified, a domain will be generated.
    ///
    /// Specifying a custom domain will also return the required DNS records
    /// to add to your DNS settings
    domain: Option<String>,
}

pub async fn command(args: Args, json: bool) -> Result<()> {
    if let Some(domain) = args.domain {
        create_custom_domain(domain, args.port, json).await?;

        return Ok(());
    }

    let configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    if project.services.edges.is_empty() {
        bail!(RailwayError::NoServices);
    }

    let service = get_service(&linked_project, &project)?;

    let vars = queries::domains::Variables {
        project_id: linked_project.project.clone(),
        environment_id: linked_project.environment.clone(),
        service_id: service.id.clone(),
    };

    let domains = post_graphql::<queries::Domains, _>(&client, configs.get_backboard(), vars)
        .await?
        .domains;

    let domain_count = domains.service_domains.len() + domains.custom_domains.len();
    if domain_count > 0 {
        return print_existing_domains(&domains);
    }

    let spinner = if std::io::stdout().is_terminal() && !json {
        Some(creating_domain_spiner(None)?)
    } else {
        None
    };

    let vars = mutations::service_domain_create::Variables {
        service_id: service.id.clone(),
        environment_id: linked_project.environment.clone(),
    };
    let domain =
        post_graphql::<mutations::ServiceDomainCreate, _>(&client, configs.get_backboard(), vars)
            .await?
            .service_domain_create
            .domain;

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    let formatted_domain = format!("https://{}", domain);
    if json {
        let out = json!({
            "domain": formatted_domain
        });

        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
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

// Returns a reference to save on Heap allocations
pub fn get_service<'a>(
    linked_project: &'a LinkedProject,
    project: &'a queries::project::ProjectProject,
) -> Result<&'a queries::project::ProjectProjectServicesEdgesNode, anyhow::Error> {
    let services = project.services.edges.iter().collect::<Vec<_>>();

    if services.is_empty() {
        bail!(RailwayError::NoServices);
    }

    if project.services.edges.len() == 1 {
        return Ok(&project.services.edges[0].node);
    }

    if let Some(service) = linked_project.service.clone() {
        if project.services.edges.iter().any(|s| s.node.id == service) {
            return Ok(&project
                .services
                .edges
                .iter()
                .find(|s| s.node.id == service)
                .unwrap()
                .node);
        }
    }

    let service = prompt_options("Select a service", services)?;

    Ok(&service.node)
}

pub fn creating_domain_spiner(message: Option<String>) -> anyhow::Result<indicatif::ProgressBar> {
    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")?,
        )
        .with_message(message.unwrap_or_else(|| "Creating domain...".to_string()));
    spinner.enable_steady_tick(Duration::from_millis(100));

    Ok(spinner)
}
