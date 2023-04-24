use std::time::Duration;

use anyhow::bail;

use crate::{
    consts::{ABORTED_BY_USER, TICK_STRING},
    util::prompt::prompt_confirm_with_default,
};

use super::*;

/// Remove the most recent deployment
#[derive(Parser)]
pub struct Args {
    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    bypass: bool,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let linked_project_name = linked_project
        .name
        .expect("Linked project is missing the name");

    let linked_environment_name = linked_project
        .environment_name
        .expect("Linked environment is missing the name");

    let linked_project_environment = format!(
        "{} environment of project {}",
        linked_environment_name.bold(),
        linked_project_name.bold()
    );

    let vars = queries::deployments::Variables {
        project_id: linked_project.project.clone(),
    };

    let res =
        post_graphql::<queries::Deployments, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;

    let mut deployments: Vec<_> = body
        .project
        .deployments
        .edges
        .into_iter()
        .map(|deployment| deployment.node)
        .collect();
    deployments.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let latest_deployment = deployments.first().context("No deployments found")?;

    if !args.bypass {
        let confirmed = prompt_confirm_with_default(
            format!("Delete the latest deployment for {linked_project_environment}?").as_str(),
            false,
        )?;

        if !confirmed {
            bail!(ABORTED_BY_USER)
        }
    }

    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")?,
        )
        .with_message(format!(
            "Deleting the latest deployment for {linked_project_environment}..."
        ));
    spinner.enable_steady_tick(Duration::from_millis(100));

    let vars = mutations::deployment_remove::Variables {
        id: latest_deployment.id.clone(),
    };

    post_graphql::<mutations::DeploymentRemove, _>(&client, configs.get_backboard(), vars).await?;

    spinner.finish_with_message(format!(
        "The latest deployment for {linked_project_environment} was deleted."
    ));

    Ok(())
}
