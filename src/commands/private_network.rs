use colored::ColoredString;
use serde::Serialize;

use crate::controllers::{
    private_network::{self, PrivateNetworkState, PrivateNetworkStatus},
    project::resolve_service_context,
};

use super::*;

const FIELD_LABEL_WIDTH: usize = 16;

/// Manage private networking for a service
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway private-network status --service api\n  railway private-network status --network railway --json\n  railway private-network update api-internal --service api\n\nAutomation notes:\n  Private networking uses the selected service and environment. When multiple private networks exist, status shows all of them and update defaults to the network named `railway` unless --network is provided."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Service name or ID (defaults to linked service)
    #[clap(short, long, global = true)]
    service: Option<String>,

    /// Environment to use (defaults to linked environment)
    #[clap(short, long, global = true)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID", global = true)]
    project: Option<String>,

    /// Private network name, ID, or DNS name
    #[clap(long, global = true)]
    network: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,
}

#[derive(Parser)]
enum Commands {
    /// Show private networking status
    Status,

    /// Update the private networking endpoint name
    Update {
        /// Endpoint name prefix, without the .internal suffix
        #[clap(value_name = "NAME")]
        name: String,
    },
}

pub async fn command(args: Args) -> Result<()> {
    let Args {
        command,
        service,
        environment,
        project,
        network,
        json,
    } = args;

    crate::util::reporter::set_mode(json);

    match command {
        Commands::Status => status(project, service, environment, network, json).await?,
        Commands::Update { name } => {
            update(project, service, environment, network, name, json).await?
        }
    }

    Ok(())
}

async fn status(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    network: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let statuses = private_network::fetch_private_network_statuses(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
        network.as_deref(),
    )
    .await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&StatusOutput {
                private_networks: statuses,
            })?
        );
        return Ok(());
    }

    if statuses.is_empty() {
        println!(
            "No private networks found for service {} in environment {}.",
            ctx.service_name.bold(),
            ctx.environment_name.bold()
        );
        return Ok(());
    }

    println!("{}", "Private networking".bold());
    println!();
    print_context(&ctx.service_name, &ctx.environment_name);
    print_statuses(&statuses);

    Ok(())
}

async fn update(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    network: Option<String>,
    name: String,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let status = private_network::update_private_network_endpoint_name(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
        network.as_deref(),
        &name,
    )
    .await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&UpdateOutput {
                private_network: status,
            })?
        );
        return Ok(());
    }

    println!("{}", "Private networking".bold());
    println!();
    print_context(&ctx.service_name, &ctx.environment_name);
    print_status(&status);

    Ok(())
}

fn print_context(service_name: &str, environment_name: &str) {
    print_field("Service:", &service_name.green().bold());
    print_field("Environment:", &environment_name.blue().bold());
}

fn print_statuses(statuses: &[PrivateNetworkStatus]) {
    for status in statuses {
        print_status(status);
    }
}

fn print_status(status: &PrivateNetworkStatus) {
    println!();
    print_divider();
    print_field("Network:", &status.network.name.purple().bold());
    print_field("Network ID:", &status.network.id.clone().dimmed());
    print_field(
        "DNS suffix:",
        &format!("{}.internal", status.network.dns_name).dimmed(),
    );
    print_field(
        "Address family:",
        &status.network.ip_family.clone().magenta().bold(),
    );
    print_field("Status:", &state_label(status.state));

    if let Some(hostname) = &status.full_hostname {
        print_field("Hostname:", &hostname.clone().magenta().bold());
    }
    if let Some(short_name) = &status.short_name {
        print_field("Short name:", &short_name.clone().magenta());
    }
    if let Some(pending_hostname) = &status.pending_hostname {
        print_field("Pending:", &pending_hostname.clone().blue().bold());
    }
    if let Some(endpoint) = &status.endpoint {
        print_field("Endpoint ID:", &endpoint.id.clone().dimmed());
        if !endpoint.private_ips.is_empty() {
            print_field("Private IPs:", &endpoint.private_ips.join(", "));
        }
    } else {
        print_field(
            "Message:",
            &"Private networking is initializing and will be ready once the deployment of this service is complete."
                .blue(),
        );
    }
}

fn print_field(label: &str, value: &dyn std::fmt::Display) {
    let padded = format!("{label:<FIELD_LABEL_WIDTH$}");
    println!("{} {value}", padded.dimmed());
}

fn print_divider() {
    println!("{}", "─".repeat(48).dimmed());
}

fn state_label(state: PrivateNetworkState) -> ColoredString {
    match state {
        PrivateNetworkState::Ready => "ready".green().bold(),
        PrivateNetworkState::Creating => "creating".blue().bold(),
        PrivateNetworkState::Updating => "updating".blue().bold(),
        PrivateNetworkState::Deleting => "deleting".yellow().bold(),
        PrivateNetworkState::Initializing => "initializing".blue().bold(),
        PrivateNetworkState::Unknown => "unknown".yellow().bold(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusOutput {
    private_networks: Vec<PrivateNetworkStatus>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateOutput {
    private_network: PrivateNetworkStatus,
}
