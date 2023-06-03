use crate::controllers::deployment::{stream_build_logs, stream_deploy_logs};

use super::{queries::deployments::DeploymentStatus, *};

/// View a deploy's logs
#[derive(Parser)]
pub struct Args {
    /// Show deployment logs
    #[clap(short, long, group = "log_type")]
    deployment: bool,

    /// Show build logs
    #[clap(short, long, group = "log_type")]
    build: bool,

    /// Deployment ID to pull logs from. Omit to pull from latest deloy
    deployment_id: Option<String>,
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

    let deployment;
    if let Some(deployment_id) = args.deployment_id {
        deployment = deployments
            .iter()
            .find(|deployment| {
                deployment.id == deployment_id
            })
            .context("Deployment id does not exist")?;
    } else {
        // get the latest deloyment
        deployments.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        deployment = deployments.first().context("No deployments found")?;
    };

    if (args.build || deployment.status == DeploymentStatus::FAILED) && !args.deployment {
        stream_build_logs(deployment.id.clone(), |log| {
            if json {
                println!("{}", serde_json::to_string(&log).unwrap());
            } else {
                println!("{}", log.message);
            }
        })
        .await?;
    } else {
        stream_deploy_logs(deployment.id.clone(), |log| {
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
