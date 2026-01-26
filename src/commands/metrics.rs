use crate::controllers::project::{ensure_project_and_environment_exist, get_project};

use super::*;

#[derive(Parser)]
#[clap(
    about = "View service resource metrics (CPU, Memory, Network, IO)",
    long_about = "View resource usage metrics for services in your project.

EXAMPLES:
  railway metrics                                    # View metrics for all services
  railway metrics --service myservice               # View metrics for a specific service
  railway metrics --time 6h                         # View metrics from the last 6 hours
  railway metrics --raw                             # Output metrics in JSON format
  railway metrics --watch                           # Live-updating top-like view"
)]
pub struct Args {
    #[clap(short, long)]
    service: Option<String>,

    #[clap(short, long, default_value = "1h")]
    time: String,

    #[clap(long)]
    raw: bool,

    #[clap(short, long)]
    watch: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    // Only filter by service if explicitly requested via --service flag
    // When no --service is provided, fetch metrics for ALL services in the project
    let service_id = if let Some(ref service_arg) = args.service {
        let service = project
            .services
            .edges
            .iter()
            .find(|s| {
                s.node.name.to_lowercase() == service_arg.to_lowercase()
                    || s.node.id == *service_arg
            })
            .context(format!("Service '{}' not found", service_arg))?;
        Some(service.node.id.clone())
    } else {
        // Don't filter - show all services
        None
    };

    let start_date = crate::controllers::metrics::parse_time_range(&args.time)?;

    let metrics_data = crate::controllers::metrics::fetch_metrics(
        &client,
        &configs,
        &linked_project.project,
        &linked_project.environment,
        service_id.as_deref(),
        start_date,
        &project,
    )
    .await?;

    if args.watch {
        crate::controllers::metrics::tui::run(
            &client,
            &configs,
            &linked_project.project,
            &linked_project.environment,
            service_id.as_deref(),
            &args.time,
            &project,
        )
        .await?;
    } else if args.raw {
        println!("{}", serde_json::to_string_pretty(&metrics_data)?);
    } else {
        crate::controllers::metrics::print_metrics_table(&metrics_data, &project)?;
    }

    Ok(())
}
