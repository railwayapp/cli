use crate::{controllers::project::get_project};

use super::*;

/// Show information about the current project
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.to_owned()).await?;

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
        if !project.plugins.edges.is_empty() {
            println!("Plugins:");
            for plugin in project.plugins.edges.iter().map(|plugin| &plugin.node) {
                println!("{}", format!("{:?}", plugin.name).dimmed().bold());
            }
        }
        if !project.services.edges.is_empty() {
            println!("Services:");
            for service in project.services.edges.iter().map(|service| &service.node) {
                println!("{}", service.name.dimmed().bold());
            }
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&project)?);
    }
    Ok(())
}
