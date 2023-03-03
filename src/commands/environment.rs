use std::fmt::Display;

use anyhow::bail;
use is_terminal::IsTerminal;

use super::{queries::project::ProjectProjectEnvironmentsEdgesNode, *};

/// Change the active environment
#[derive(Parser)]
pub struct Args {
    /// The environment to link to
    environment: Option<String>,
}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };

    let res = post_graphql::<queries::Project, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;

    let environments: Vec<_> = body
        .project
        .environments
        .edges
        .iter()
        .map(|env| Environment(&env.node))
        .collect();

    if let Some(environment) = _args.environment {
        let environment = environments
            .iter()
            .find(|env| env.0.id == environment || env.0.name == environment)
            .context("Environment not found")?;
        configs.link_project(
            linked_project.project.clone(),
            linked_project.name.clone(),
            environment.0.id.clone(),
            Some(environment.0.name.clone()),
        )?;
        configs.write()?;
        return Ok(());
    }

    if !std::io::stdout().is_terminal() {
        bail!("Environment must be specified when not running in a terminal");
    }

    let environment = inquire::Select::new("Select an environment", environments)
        .with_render_config(configs.get_render_config())
        .prompt()?;

    configs.link_project(
        linked_project.project.clone(),
        linked_project.name.clone(),
        environment.0.id.clone(),
        Some(environment.0.name.clone()),
    )?;
    configs.write()?;
    Ok(())
}

#[derive(Debug, Clone)]
struct Environment<'a>(&'a ProjectProjectEnvironmentsEdgesNode);

impl<'a> Display for Environment<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}
