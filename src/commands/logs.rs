use crate::{
    controllers::{
        deployment::{fetch_build_logs, fetch_deploy_logs, stream_build_logs, stream_deploy_logs},
        environment::get_matched_environment,
        project::{ensure_project_and_environment_exist, get_project},
    },
    util::logs::print_log,
};
use anyhow::bail;

use super::{
    queries::deployments::{DeploymentListInput, DeploymentStatus},
    *,
};

#[derive(Parser)]
#[clap(
    about = "View build or deploy logs from a Railway deployment",
    long_about = "View build or deploy logs from a Railway deployment. This will stream logs by default, or fetch historical logs if the --lines flag is provided.",
    after_help = "Examples:

  railway logs                                                       # Stream live logs from latest deployment
  railway logs --build 7422c95b-c604-46bc-9de4-b7a43e1fd53d          # Stream build logs from a specific deployment
  railway logs --lines 100                                           # Pull last 100 logs without streaming
  railway logs --service backend --environment production            # Stream latest deployment logs from a specific service in a specific environment
  railway logs --lines 10 --filter \"@level:error\"                    # View 10 latest error logs
  railway logs --lines 10 --filter \"@level:warn AND rate limit\"      # View 10 latest warning logs related to rate limiting
  railway logs --json                                                # Get logs in JSON format"
)]
pub struct Args {
    /// Service to view logs from (defaults to linked service). Can be service name or service ID
    #[clap(short, long)]
    service: Option<String>,

    /// Environment to view logs from (defaults to linked environment). Can be environment name or environment ID
    #[clap(short, long)]
    environment: Option<String>,

    /// Show deployment logs
    #[clap(short, long, group = "log_type")]
    deployment: bool,

    /// Show build logs
    #[clap(short, long, group = "log_type")]
    build: bool,

    /// Deployment ID to view logs from. Defaults to most recent successful deployment, or latest deployment if none succeeded
    deployment_id: Option<String>,

    /// Output logs in JSON format. Each log line becomes a JSON object with timestamp, message, and any other attributes
    #[clap(long)]
    json: bool,

    /// Number of log lines to fetch (disables streaming)
    #[clap(short = 'n', long = "lines", visible_alias = "tail")]
    lines: Option<i64>,

    /// Filter logs using Railway's query syntax
    ///
    /// Can be a text search ("error message" or "user signup"), attribute filters (@level:error, @level:warn), or a combination with the operators AND, OR, - (not). See https://docs.railway.com/guides/logs for full syntax.
    #[clap(long, short = 'f')]
    filter: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    // Stream only if no line limit is specified
    let should_stream = args.lines.is_none();

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
        _ => bail!(
            "No service could be found. Please either link one with `railway service` or specify one via the `--service` flag."
        ),
    };

    // Fetch all deployments so we can find a sensible default deployment id if
    // none is provided
    let vars = queries::deployments::Variables {
        input: DeploymentListInput {
            project_id: Some(linked_project.project.clone()),
            environment_id: Some(environment_id),
            service_id: Some(service),
            include_deleted: None,
            status: None,
        },
        first: None,
    };
    let deployments =
        post_graphql::<queries::Deployments, _>(&client, configs.get_backboard(), vars)
            .await?
            .deployments;
    let mut all_deployments: Vec<_> = deployments
        .edges
        .into_iter()
        .map(|deployment| deployment.node)
        .collect();
    all_deployments.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let default_deployment = all_deployments
        .iter()
        .find(|d| d.status == DeploymentStatus::SUCCESS)
        .or_else(|| all_deployments.first())
        .context("No deployments found")?;

    let deployment_id = if let Some(deployment_id) = args.deployment_id {
        // Use the provided deployment ID directly
        deployment_id
    } else {
        default_deployment.id.clone()
    };

    let show_build_logs = args.build
        || (default_deployment.status == DeploymentStatus::FAILED
            && deployment_id == default_deployment.id);

    if show_build_logs {
        if should_stream {
            stream_build_logs(deployment_id.clone(), args.filter.clone(), |log| {
                print_log(log, args.json, false) // Build logs use simple output
            })
            .await?;
        } else {
            fetch_build_logs(
                &client,
                &configs.get_backboard(),
                deployment_id.clone(),
                args.lines.or(Some(500)),
                args.filter.clone(),
                |log| print_log(log, args.json, false), // Build logs use simple output
            )
            .await?;
        }
    } else {
        if should_stream {
            stream_deploy_logs(deployment_id.clone(), args.filter.clone(), |log| {
                print_log(log, args.json, true) // Deploy logs use formatted output
            })
            .await?;
        } else {
            fetch_deploy_logs(
                &client,
                &configs.get_backboard(),
                deployment_id.clone(),
                args.lines.or(Some(500)),
                args.filter.clone(),
                |log| print_log(log, args.json, true), // Deploy logs use formatted output
            )
            .await?;
        }
    }

    Ok(())
}
