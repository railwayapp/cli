use anyhow::bail;
use regex::Regex;

use crate::{
    controllers::project::{ensure_project_and_environment_exist, get_project},
    errors::RailwayError,
    util::prompt::{prompt_options, prompt_text},
};

use super::*;

/// Add a custom domain for a service
#[derive(Parser)]
pub struct Args {
    #[clap(short, long)]
    domain: Option<String>,
}

pub async fn command(args: Args, json: bool) -> Result<()> {
    // it's too bad that we have to check twice, but I think the UX is better
    // if we immediately exit if the user enters an invalid domain
    if let Some(domain) = &args.domain {
        if !is_valid_domain(&domain) {
            bail!("Invalid domain");
        }
    }

    let configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    if project.services.edges.is_empty() {
        return Err(RailwayError::NoServices.into());
    }

    let service = get_service(&linked_project, &project)?;

    if !json {
        println!("Creating custom domain for service {}...", service.name);
    }

    let domain = match args.domain {
        Some(domain) => domain,
        None => prompt_text("Enter the domain")?,
    };

    if !is_valid_domain(&domain) {
        bail!("Invalid domain");
    }

    let is_available = post_graphql::<queries::CustomDomainAvailable, _>(
        &client,
        configs.get_backboard(),
        queries::custom_domain_available::Variables {
            domain: domain.clone(),
        },
    )
    .await?;

    if !is_available.custom_domain_available.available {
        bail!(
            "Domain is not available:\n{}",
            is_available.custom_domain_available.message
        );
    }

    let input = mutations::custom_domain_create::CustomDomainCreateInput {
        domain: domain.clone(),
        environment_id: linked_project.environment.clone(),
        project_id: linked_project.project.clone(),
        service_id: service.id.clone(),
        target_port: None,
    };

    let vars = mutations::custom_domain_create::Variables { input };

    let response =
        post_graphql::<mutations::CustomDomainCreate, _>(&client, configs.get_backboard(), vars)
            .await?;

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

    // TODO: What is the maximum length of the hostlabel that railway supports?
    // TODO: if the length is very long, consider checking the maximum length \
    //       and then printing the table header with different spacing
    println!("\tType\tHost\tValue");
    for record in response.custom_domain_create.status.dns_records {
        println!(
            "\t{}\t{}\t{}",
            record.record_type, record.hostlabel, record.required_value,
        );
    }

    println!("\nPlease be aware that DNS records can take up to 72 hours to propagate worldwide.");

    Ok(())
}

// Returns a reference to save on Heap allocations
fn get_service<'a>(
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

fn is_valid_domain(domain: &str) -> bool {
    let domain_regex = Regex::new(r"^(?:[a-zA-Z0-9-]+\.)+[a-zA-Z]{2,}$").unwrap();
    domain_regex.is_match(domain)
}
