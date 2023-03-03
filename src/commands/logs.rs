use futures::StreamExt;

use crate::subscription::subscribe_graphql;

use super::*;

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

    if args.build && !args.deployment {
        let vars = subscriptions::build_logs::Variables {
            deployment_id: latest_deployment.id.clone(),
            filter: Some(String::new()),
            limit: Some(500),
        };

        let (_client, mut log_stream) = subscribe_graphql::<subscriptions::BuildLogs>(vars).await?;
        while let Some(Ok(log)) = log_stream.next().await {
            let log = log.data.context("Failed to retrieve log")?;
            for line in log.build_logs {
                if json {
                    println!("{}", serde_json::to_string(&line)?);
                } else {
                    println!("{}", line.message);
                }
            }
        }
    } else {
        let vars = subscriptions::deployment_logs::Variables {
            deployment_id: latest_deployment.id.clone(),
            filter: Some(String::new()),
            limit: Some(500),
        };

        let (_client, mut log_stream) =
            subscribe_graphql::<subscriptions::DeploymentLogs>(vars).await?;
        while let Some(Ok(log)) = log_stream.next().await {
            let log = log.data.context("Failed to retrieve log")?;
            for line in log.deployment_logs {
                if json {
                    println!("{}", serde_json::to_string(&line)?);
                } else {
                    println!("{}", line.message);
                }
            }
        }
    }

    Ok(())
}
