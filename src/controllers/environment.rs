use crate::{
    LinkedProject,
    commands::queries::{RailwayProject, project::ProjectProjectEnvironmentsEdgesNode},
    errors::RailwayError,
    queries::project::ProjectProject,
    util::prompt::{PromptEnvironment, prompt_select},
};
use anyhow::{Context, Result, bail};
use is_terminal::IsTerminal;

pub fn get_matched_environment(
    project: &RailwayProject,
    environment: String,
) -> Result<ProjectProjectEnvironmentsEdgesNode> {
    let environment = project
        .environments
        .edges
        .iter()
        .find(|env| env.node.name == environment || env.node.id == environment)
        .ok_or_else(|| RailwayError::EnvironmentNotFound(environment))?;

    Ok(environment.node.clone())
}

pub async fn get_or_prompt_environment(
    linked_project: Option<LinkedProject>,
    project: &ProjectProject,
    environment_arg: Option<String>,
) -> Result<Option<String>> {
    let environments = project.environments.edges.iter().collect::<Vec<_>>();

    let environment_id = if let Some(environment_arg) = environment_arg {
        // If the user specified a service, use that
        let environment_id = environments.iter().find(|environment| {
            environment.node.name == environment_arg || environment.node.id == environment_arg
        });
        if let Some(environment_id) = environment_id {
            Some(environment_id.node.id.to_owned())
        } else {
            bail!(RailwayError::EnvironmentNotFound(environment_arg));
        }
    } else if let Some(environment) = linked_project.and_then(|lp| lp.environment) {
        Some(environment)
    } else {
        // If the user didn't specify an environment, and we don't have a linked environment, get the first environment

        if environments.is_empty() {
            // If there are no environments, backboard will generate one for us
            None
        } else {
            // If there are multiple environments, prompt the user to select one
            if std::io::stdout().is_terminal() {
                let prompt_environments: Vec<_> = environments
                    .iter()
                    .map(|s| PromptEnvironment(&s.node))
                    .collect();
                let service = prompt_select("Select an environment", prompt_environments)
                    .context("Please specify an environment via the `--environment` flag.")?;
                Some(service.0.id.clone())
            } else {
                bail!(
                    "Multiple environments found. Please specify an environment via the `--environment` flag."
                )
            }
        }
    };

    Ok(environment_id)
}
