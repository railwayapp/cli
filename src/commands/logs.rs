use crate::controllers::deployment::{stream_build_logs, stream_deploy_logs};

use super::{queries::deployments::DeploymentStatus, *};

/// View the most-recent deploy's logs
#[derive(Parser)]
pub struct Args {
    /// Show deployment logs
    #[clap(short, long, group = "log_type")]
    deployment: bool,

    /// Show build logs
    #[clap(short, long, group = "log_type")]
    build: bool,
}

pub async fn command(args: Args, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let vars = queries::deployments::Variables {
        project_id: linked_project.project.clone(),
    };

    let deployments =
        post_graphql::<queries::Deployments, _>(&client, configs.get_backboard(), vars)
            .await?
            .project
            .deployments;

    let mut deployments: Vec<_> = deployments
        .edges
        .into_iter()
        .map(|deployment| deployment.node)
        .collect();
    deployments.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let latest_deployment = deployments.first().context("No deployments found")?;

    if (args.build || latest_deployment.status == DeploymentStatus::FAILED) && !args.deployment {
        stream_build_logs(latest_deployment.id.clone(), |log| {
            if json {
                println!("{}", serde_json::to_string(&log).unwrap());
            } else {
                println!("{}", log.message);
            }
        })
        .await?;
    } else {
        stream_deploy_logs(latest_deployment.id.clone(), |log| {
            if json {
                println!("{}", serde_json::to_string(&log).unwrap());
            } else {
                println!("{}", log.message);
            }
        })
        .await?;
    }

    Ok(())
}
