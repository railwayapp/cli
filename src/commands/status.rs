use super::*;

/// Show information about the current project
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };

    let res = post_graphql::<queries::Project, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;
    if !json {
        println!("Project: {}", body.project.name.purple().bold());
        println!(
            "Environment: {}",
            body.project
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
        if !body.project.plugins.edges.is_empty() {
            println!("Plugins:");
            for plugin in body.project.plugins.edges.iter().map(|plugin| &plugin.node) {
                println!("{}", format!("{:?}", plugin.name).dimmed().bold());
            }
        }
        if !body.project.services.edges.is_empty() {
            println!("Services:");
            for service in body
                .project
                .services
                .edges
                .iter()
                .map(|service| &service.node)
            {
                println!("{}", service.name.dimmed().bold());
            }
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&body.project)?);
    }
    Ok(())
}
