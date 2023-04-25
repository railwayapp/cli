use std::fmt::Display;

use anyhow::bail;
use is_terminal::IsTerminal;

use crate::{controllers::project::get_project, interact_or, util::prompt::prompt_options};

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
    let environments: Vec<_> = project
        .environments
        .edges
        .iter()
        .map(|env| Environment(&env.node))
        .collect();

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
