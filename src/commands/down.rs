use std::time::Duration;

use super::{
    queries::{deployments::DeploymentListInput, deployments::DeploymentStatus},
    *,
};
use crate::{
    consts::TICK_STRING, controllers::project::resolve_service_context,
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

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    bypass: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let ctx = resolve_service_context(args.project, args.service, args.environment).await?;
    let project_id = ctx.project_id;
    let environment_id = ctx.environment_id;
    let environment_name = ctx.environment_name;
    let service = ctx.service_id;
    let project_name = ctx.project.name.clone();

    let vars = queries::deployments::Variables {
        input: DeploymentListInput {
            project_id: Some(project_id.clone()),
            environment_id: Some(environment_id.clone()),
            service_id: Some(service),
            include_deleted: None,
            status: None,
        },
        first: None,
    };

    let linked_project_environment = format!(
        "{} environment of project {}",
        environment_name.bold(),
        project_name.bold()
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
