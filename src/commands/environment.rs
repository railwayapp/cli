use std::fmt::Display;

use crate::{
    controllers::project::get_project, errors::RailwayError, interact_or,
    util::prompt::prompt_options,
};
use anyhow::bail;

use super::{queries::project::ProjectProjectEnvironmentsEdgesNode, *};

/// Change the active environment
#[derive(Parser)]
pub struct Args {
    /// The environment to link to
    environment: Option<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    if project.deleted_at.is_some() {
        bail!(RailwayError::ProjectDeleted);
    }

    let environments = project
        .environments
        .edges
        .iter()
        .map(|env| Environment(&env.node))
        .collect::<Vec<_>>();

    let environment = match args.environment {
        // If the environment is specified, find it in the list of environments
        Some(environment) => {
            let environment = environments
                .iter()
                .find(|env| env.0.id == environment || env.0.name == environment)
                .context("Environment not found")?;
            environment.clone()
        }
        // If the environment is not specified, prompt the user to select one
        None => {
            interact_or!("Environment must be specified when not running in a terminal");
            let environment = if environments.len() == 1 {
                match environments.first() {
                    // Project has only one environment, so use that one
                    Some(environment) => environment.clone(),
                    // Project has no environments, so bail
                    None => bail!("Project has no environments"),
                }
            } else {
                // Project has multiple environments, so prompt the user to select one
                prompt_options("Select an environment", environments)?
            };
            environment
        }
    };

    let environment_name = environment.0.name.clone();
    println!("Activated environment {}", environment_name.purple().bold());

    configs.link_project(
        linked_project.project.clone(),
        linked_project.name.clone(),
        environment.0.id.clone(),
        Some(environment_name),
    )?;
    configs.write()?;
    Ok(())
}

#[derive(Debug, Clone)]
struct Environment<'a>(&'a ProjectProjectEnvironmentsEdgesNode);

impl Display for Environment<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}
