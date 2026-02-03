use super::{Delete as Args, *};
use crate::{
    Configs, GQLClient,
    controllers::project::get_project,
    errors::RailwayError,
    util::{
        progress::create_spinner_if,
        prompt::{prompt_confirm_with_default, prompt_options},
        two_factor::validate_two_factor_if_enabled,
    },
};
use anyhow::{Result, bail};
use is_terminal::IsTerminal;

pub async fn delete_environment(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;
    let is_terminal = std::io::stdout().is_terminal();

    let (id, name) = if let Some(environment) = args.environment {
        if let Some(env) = project.environments.edges.iter().find(|e| {
            (e.node.id.to_lowercase() == environment)
                || (e.node.name.to_lowercase() == environment.to_lowercase())
        }) {
            fake_select("Select the environment to delete", &env.node.name);
            (env.node.id.clone(), env.node.name.clone())
        } else {
            bail!(RailwayError::EnvironmentNotFound(environment))
        }
    } else if is_terminal {
        let all_environments = &project.environments.edges;
        let environments = all_environments
            .iter()
            .filter(|env| env.node.can_access)
            .map(|env| Environment(&env.node))
            .collect::<Vec<_>>();
        if environments.is_empty() {
            if all_environments.is_empty() {
                bail!("Project has no environments");
            } else {
                bail!("All environments in this project are restricted");
            }
        }
        let r = prompt_options("Select the environment to delete", environments)?;
        (r.0.id.clone(), r.0.name.clone())
    } else {
        bail!("Environment must be specified when not running in a terminal");
    };

    let confirmed = if args.bypass {
        true
    } else if is_terminal {
        prompt_confirm_with_default(
            format!(
                r#"Are you sure you want to delete the environment "{}"?"#,
                name.red()
            )
            .as_str(),
            false,
        )?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };

    if !confirmed {
        return Ok(());
    }

    validate_two_factor_if_enabled(&client, &configs, is_terminal, args.two_factor_code).await?;

    let spinner = create_spinner_if(!args.json, "Deleting environment...".into());
    let _r = post_graphql::<mutations::EnvironmentDelete, _>(
        &client,
        &configs.get_backboard(),
        mutations::environment_delete::Variables { id: id.clone() },
    )
    .await?;
    if args.json {
        println!("{}", serde_json::json!({"id": id}));
    } else if let Some(spinner) = spinner {
        spinner.finish_with_message("Environment deleted!");
    }
    Ok(())
}
