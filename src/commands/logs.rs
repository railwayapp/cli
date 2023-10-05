use crate::controllers::{
    deployment::{stream_build_logs, stream_deploy_logs},
    environment::get_matched_environment,
    project::get_project,
};
use anyhow::bail;

use super::{
    queries::deployments::{DeploymentListInput, DeploymentStatus},
    *,
};
use colored::Colorize;

/// View a deploy's logs
#[derive(Parser)]
pub struct Args {
    /// Service to view logs from (defaults to linked service)
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to view logs from (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

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
        _ => bail!("No service could be found. Please either link one with `railway service` or specify one via the `--service` flag."),
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

    let deployment;
    if let Some(deployment_id) = args.deployment_id {
        deployment = deployments
            .iter()
            .find(|deployment| deployment.id == deployment_id)
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
        stream_deploy_logs(deployment.id.clone(), |mut log| {
            if !log.attributes.is_empty() {
                let mut timestamp: Option<String> = None;
                let mut level: Option<String> = None;
                let message = log.message;
                let mut others = Vec::new();
                // for some reason, not all have "" around the value
                for attr in &mut log.attributes {
                    if !attr.value.starts_with('"') {
                        attr.value.insert(0, '"');
                    };
                    if !attr.value.ends_with('"') {
                        attr.value.push('"');
                    }
                }
                // get attributes using a match
                for attr in &log.attributes {
                    match attr.key.to_lowercase().as_str() {
                        "timestamp" | "ts" | "time" => {
                            timestamp = Some(attr.value.clone().replace('"', ""))
                        }
                        "level" | "lvl" => level = Some(attr.value.clone()),
                        _ => others.push(format!(
                            "{}{}{}",
                            attr.key.clone().bright_cyan(),
                            "=",
                            attr.value
                                .clone()
                                .replace('"', "\"".dimmed().to_string().as_str())
                        )),
                    }
                }
                // get the level and colour it
                let level = level.map(|level| {
                    // make it uppercase so we dont have to make another variable
                    // for some reason, .uppercase() removes formatting
                    let level = level.replace('"', "").to_uppercase();
                    match level.to_lowercase().as_str() {
                        "info" => level.blue(),
                        "error" => level.red(),
                        "warn" => level.yellow(),
                        "debug" => level.magenta(),
                        _ => level.normal(),
                    }
                    .bold()
                });
                println!(
                    "{}={} {}={} {}={}{}{5} {}",
                    "timestamp".bright_cyan(),
                    timestamp.unwrap_or_default().purple(),
                    "level".bright_cyan(),
                    level.unwrap_or_default(),
                    "msg".bright_cyan(),
                    "\"".dimmed(),
                    message,
                    others.join(" ")
                );
            } else {
                println!("{}", log.message);
            }
        })
        .await?;
    }

    Ok(())
}
