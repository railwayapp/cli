use crate::controllers::project::{ensure_project_and_environment_exist, get_project};

use super::*;

/// Show information about the current project
#[derive(Parser)]
pub struct Args;

pub async fn command(_args: Args, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.to_owned()).await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    if !json {
        println!("Project: {}", project.name.purple().bold());
        println!(
            "Environment: {}",
            project
                .environments
                .edges
                .iter()
                .map(|env| &env.node)
                .find(|env| env.id == linked_project.environment)
                .context("Environment not found!")?
                .name
                .blue()
                .bold()
        );

        if let Some(linked_service) = linked_project.service {
            let service = project
                .services
                .edges
                .iter()
                .find(|service| service.node.id == linked_service)
                .expect("the linked service doesn't exist");
            println!("Service: {}", service.node.name.green().bold());
        } else {
            println!("Service: {}", "None".red().bold())
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&project)?);
    }
    Ok(())
}
