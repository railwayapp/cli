use anyhow::bail;
use is_terminal::IsTerminal;

use crate::{
    controllers::project::{ensure_project_and_environment_exist, get_project},
    errors::RailwayError,
};

use domain::{creating_domain_spiner, get_service};

use super::*;

pub async fn create_custom_domain(domain: String, port: Option<u16>, json: bool) -> Result<()> {
    let configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    if project.services.edges.is_empty() {
        return Err(RailwayError::NoServices.into());
    }

    let service = get_service(&linked_project, &project)?;

    let spinner = if std::io::stdout().is_terminal() && !json {
        Some(creating_domain_spiner(Some(format!(
            "Creating custom domain for service {}{}...",
            service.name,
            port.unwrap_or_default()
        )))?)
    } else {
        None
    };

    let is_available = post_graphql::<queries::CustomDomainAvailable, _>(
        &client,
        configs.get_backboard(),
        queries::custom_domain_available::Variables {
            domain: domain.clone(),
        },
    )
    .await?;

    if !is_available.custom_domain_available.available {
        bail!("Domain is not available:\n\t{}", domain);
    }

    let input = mutations::custom_domain_create::CustomDomainCreateInput {
        domain: domain.clone(),
        environment_id: linked_project.environment.clone(),
        project_id: linked_project.project.clone(),
        service_id: service.id.clone(),
        target_port: port.map(|p| p as i64),
    };

    let vars = mutations::custom_domain_create::Variables { input };

    let response =
        post_graphql::<mutations::CustomDomainCreate, _>(&client, configs.get_backboard(), vars)
            .await?;

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

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

    println!("\tType\tHost\tValue");
    for record in response.custom_domain_create.status.dns_records {
        let not_empty_hostlabel = if record.hostlabel.is_empty() {
            "@".into()
        } else {
            record.hostlabel
        };
        println!(
            "\t{}\t{}\t{}",
            record.record_type, not_empty_hostlabel, record.required_value,
        );
    }

    println!("\nNote: if the Host is \"@\", the DNS record should be created for the root of the domain.");
    println!("Please be aware that DNS records can take up to 72 hours to propagate worldwide.");

    Ok(())
}
