use crate::{
    controllers::project::{ensure_project_and_environment_exist, get_project},
    errors::RailwayError,
    util::prompt::{fake_select, prompt_options, prompt_text, PromptService},
};
use anyhow::bail;
use clap::Subcommand;
use reqwest::Client;

use super::*;

/// Link a service to the current project
#[derive(Parser)]
pub struct Args {
    /// The service ID/name to link
    service: Option<String>,

    /// Create a new service
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a new service
    Create {
        /// The name of the new service to create
        #[clap(long, short)]
        name: Option<String>,
    },
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;
    if let Some(command) = args.command {
        match command {
            Commands::Create { name } => {
                let name = if let Some(name) = name {
                    fake_select("Enter a service name", &name);
                    name
                } else {
                    // prompt for it
                    prompt_text("Enter a service name")?
                };
                let service_id = create_service(
                    &client,
                    &configs,
                    name.clone(),
                    linked_project.project.clone(),
                    linked_project.environment.clone(),
                )
                .await?;
                // link the service
                configs.link_service(service_id)?;
                configs.write()?;
                println!(
                    "Succesfully created the service \"{}\" and linked to it",
                    name.blue()
                );
            }
        }
        return Ok(());
    }

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

pub async fn create_service(
    client: &Client,
    configs: &Configs,
    name: String,
    project_id: String,
    environment_id: String,
) -> Result<String> {
    let vars = mutations::service_create::Variables {
        name: Some(name),
        project_id,
        environment_id,
    };
    let response =
        post_graphql::<mutations::ServiceCreate, _>(client, configs.get_backboard(), vars).await?;
    Ok(response.service_create.id)
}
