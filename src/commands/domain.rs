use std::{cmp::max, time::Duration};

use anyhow::bail;
use colored::Colorize;
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

    let spinner = (std::io::stdout().is_terminal() && !json)
        .then(|| creating_domain_spiner(None))
        .and_then(|s| s.ok());

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
) -> anyhow::Result<&'a queries::project::ProjectProjectServicesEdgesNode> {
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

    if !std::io::stdout().is_terminal() {
        bail!(RailwayError::NoServices);
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

async fn create_custom_domain(domain: String, port: Option<u16>, json: bool) -> Result<()> {
    let configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let service = get_service(&linked_project, &project)?;

    let spinner = (std::io::stdout().is_terminal() && !json)
        .then(|| {
            creating_domain_spiner(Some(format!(
                "Creating custom domain for service {}{}...",
                service.name,
                port.map(|p| format!(" on port {}", p)).unwrap_or_default()
            )))
        })
        .and_then(|s| s.ok());

    let is_available = post_graphql::<queries::CustomDomainAvailable, _>(
        &client,
        configs.get_backboard(),
        queries::custom_domain_available::Variables {
            domain: domain.clone(),
        },
    )
    .await?
    .custom_domain_available
    .available;

    if !is_available {
        bail!("Domain is not available:\n\t{}", domain);
    }

    let vars = mutations::custom_domain_create::Variables {
        input: mutations::custom_domain_create::CustomDomainCreateInput {
            domain: domain.clone(),
            environment_id: linked_project.environment.clone(),
            project_id: linked_project.project.clone(),
            service_id: service.id.clone(),
            target_port: port.map(|p| p as i64),
        },
    };

    let response =
        post_graphql::<mutations::CustomDomainCreate, _>(&client, configs.get_backboard(), vars)
            .await?;

    spinner.map(|s| s.finish_and_clear());

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!("Domain created: {}", response.custom_domain_create.domain);

    if response.custom_domain_create.status.dns_records.is_empty() {
        // Should never happen (would only be possible in a backend bug)
        // but just in case
        bail!("No DNS records found. Please check the Railway dashboard for more information.");
    }

    println!(
        "To finish setting up your custom domain, add the following to the DNS records for {}:\n",
        &response.custom_domain_create.status.dns_records[0].zone
    );

    print_dns(response.custom_domain_create.status.dns_records);

    println!("\nNote: if the Host is \"@\", the DNS record should be created for the root of the domain.");
    println!("Please be aware that DNS records can take up to 72 hours to propagate worldwide.");

    Ok(())
}

fn print_dns(
    domains: Vec<
        mutations::custom_domain_create::CustomDomainCreateCustomDomainCreateStatusDnsRecords,
    >,
) {
    let (padding_type, padding_hostlabel, padding_value) =
        domains
            .iter()
            .fold((5, 5, 5), |(max_type, max_hostlabel, max_value), d| {
                (
                    max(max_type, d.record_type.to_string().len()),
                    max(max_hostlabel, d.hostlabel.to_string().len()),
                    max(max_value, d.required_value.to_string().len()),
                )
            });

    // Add padding to each maximum length
    let [padding_type, padding_hostlabel, padding_value] =
        [padding_type + 3, padding_hostlabel + 3, padding_value + 3];

    // Print the header with consistent padding
    println!(
        "\t{:<width_type$}{:<width_host$}{:<width_value$}",
        "Type",
        "Host",
        "Value",
        width_type = padding_type,
        width_host = padding_hostlabel,
        width_value = padding_value
    );

    // Print each domain entry with the same padding
    for domain in &domains {
        println!(
            "\t{:<width_type$}{:<width_host$}{:<width_value$}",
            domain.record_type.to_string(),
            if domain.hostlabel.is_empty() {
                "@"
            } else {
                &domain.hostlabel
            },
            domain.required_value,
            width_type = padding_type,
            width_host = padding_hostlabel,
            width_value = padding_value
        );
    }
}
