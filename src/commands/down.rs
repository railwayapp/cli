use std::time::Duration;

use anyhow::bail;

use super::{
    queries::{deployments::DeploymentListInput, deployments::DeploymentStatus},
    *,
};
use crate::{
    consts::TICK_STRING,
    controllers::{environment::get_matched_environment, project::get_project},
    errors::RailwayError,
    util::prompt::prompt_confirm_with_default,
};

/// Remove the most recent deployment
#[derive(Parser)]
pub struct Args {
    /// Service to remove the deployment from (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to remove the deployment from (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    bypass: bool,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let environment = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());

    let services = project.services.edges.iter().collect::<Vec<_>>();

    let environment_id = get_matched_environment(&project, environment)?.id;
    let service = match (args.service, linked_project.service) {
        // If the user specified a service, use that
        (Some(service_arg), _) => services
            .iter()
            .find(|service| service.node.name == service_arg || service.node.id == service_arg)
            .with_context(|| format!("Service '{service_arg}' not found"))?
            .node
            .id
            .to_owned(),
        // Otherwise if we have a linked service, use that
        (_, Some(linked_service)) => linked_service,
        // Otherwise it's a user error
        _ => bail!(RailwayError::NoServiceLinked),
    };

    let vars = queries::deployments::Variables {
        input: DeploymentListInput {
            project_id: Some(linked_project.project.clone()),
            environment_id: Some(environment_id),
            service_id: Some(service),
            include_deleted: None,
            status: None,
        },
    };

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

    let deployments =
        post_graphql::<queries::Deployments, _>(&client, configs.get_backboard(), vars)
            .await?
            .deployments;

    let mut deployments: Vec<_> = deployments
        .edges
        .into_iter()
        .filter_map(|deployment| {
            (deployment.node.status == DeploymentStatus::SUCCESS).then_some(deployment.node)
        })
        .collect();
    deployments.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let latest_deployment = deployments.first().context("No deployments found")?;

    if !args.bypass {
        let confirmed = prompt_confirm_with_default(
            format!("Delete the latest deployment for {linked_project_environment}?").as_str(),
            false,
        )?;

        if !confirmed {
            return Ok(());
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
