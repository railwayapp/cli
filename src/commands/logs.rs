use std::collections::HashMap;

use crate::{
    controllers::{
        deployment::{fetch_build_logs, fetch_deploy_logs, stream_build_logs, stream_deploy_logs},
        environment::get_matched_environment,
        project::{ensure_project_and_environment_exist, get_project},
    },
    util::logs::{format_attr_log, LogLike},
};
use anyhow::bail;
use serde_json::Value;

use super::{
    queries::deployments::{DeploymentListInput, DeploymentStatus},
    *,
};

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

    /// Output in JSON format
    #[clap(long)]
    json: bool,

    /// Limit the number of log lines returned (only applies when --stream is false)
    #[clap(long)]
    limit: Option<i64>,

    /// Stream logs continuously
    #[clap(long, default_value = "true", action = clap::ArgAction::Set)]
    stream: bool,
}

impl LogLike for queries::deployment_logs::LogFields {
    fn message(&self) -> &str {
        &self.message
    }
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn attributes(&self) -> Vec<(&str, &str)> {
        self.attributes
            .iter()
            .map(|a| (a.key.as_str(), a.value.as_str()))
            .collect()
    }
}

impl LogLike for subscriptions::build_logs::LogFields {
    fn message(&self) -> &str {
        &self.message
    }
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn attributes(&self) -> Vec<(&str, &str)> {
        self.attributes
            .iter()
            .map(|a| (a.key.as_str(), a.value.as_str()))
            .collect()
    }
}

impl LogLike for queries::build_logs::LogFields {
    fn message(&self) -> &str {
        &self.message
    }
    fn timestamp(&self) -> &str {
        &self.timestamp
    }
    fn attributes(&self) -> Vec<(&str, &str)> {
        self.attributes
            .iter()
            .map(|a| (a.key.as_str(), a.value.as_str()))
            .collect()
    }
}

// Helper function to print any log type
fn print_log<T>(log: T, json: bool, use_formatted: bool)
where
    T: LogLike + serde::Serialize,
{
    if json {
        // For JSON output, handle attributes specially
        let mut map: HashMap<String, Value> = HashMap::new();

        map.insert(
            "message".to_string(),
            serde_json::to_value(log.message()).unwrap(),
        );
        map.insert(
            "timestamp".to_string(),
            serde_json::to_value(log.timestamp()).unwrap(),
        );

        // Insert dynamic attributes
        for (key, value) in log.attributes() {
            let parsed_value = match value.trim_matches('"').parse::<Value>() {
                Ok(v) => v,
                Err(_) => serde_json::to_value(value.trim_matches('"')).unwrap(),
            };
            map.insert(key.to_string(), parsed_value);
        }

        let json_string = serde_json::to_string(&map).unwrap();
        println!("{json_string}");
    } else if use_formatted {
        // For formatted non-JSON output
        format_attr_log(&log);
    } else {
        // Simple output (just the message)
        println!("{}", log.message());
    }
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

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
        if args.stream {
            stream_build_logs(deployment.id.clone(), |log| {
                print_log(log, args.json, false) // Build logs use simple output
            })
            .await?;
        } else {
            fetch_build_logs(
                &client,
                &configs.get_backboard(),
                deployment.id.clone(),
                args.limit.or(Some(500)),
                |log| print_log(log, args.json, false), // Build logs use simple output
            )
            .await?;
        }
    } else {
        if args.stream {
            stream_deploy_logs(deployment.id.clone(), |log| {
                print_log(log, args.json, true) // Deploy logs use formatted output
            })
            .await?;
        } else {
            fetch_deploy_logs(
                &client,
                &configs.get_backboard(),
                deployment.id.clone(),
                args.limit.or(Some(500)),
                |log| print_log(log, args.json, true), // Deploy logs use formatted output
            )
            .await?;
        }
    }

    Ok(())
}
