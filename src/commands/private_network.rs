use colored::ColoredString;
use serde::Serialize;

use crate::commands::output::fields::{print_field, print_service_environment_context};
use crate::controllers::{
    private_network::{self, PrivateNetworkState, PrivateNetworkStatus, endpoint_dns_suffix},
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
                private_networks: statuses.iter().map(PrivateNetworkOutput::from).collect(),
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
    print_service_environment_context(&ctx.service_name, &ctx.environment_name, FIELD_LABEL_WIDTH);
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
                private_network: PrivateNetworkOutput::from(&status),
            })?
        );
        return Ok(());
    }

    println!("{}", "Private networking".bold());
    println!();
    print_service_environment_context(&ctx.service_name, &ctx.environment_name, FIELD_LABEL_WIDTH);
    print_status(&status);

    Ok(())
}

fn print_statuses(statuses: &[PrivateNetworkStatus]) {
    for status in statuses {
        print_status(status);
    }
}

fn print_status(status: &PrivateNetworkStatus) {
    println!();
    print_divider();
    print_field(
        "Network:",
        &status.network.name.purple().bold(),
        FIELD_LABEL_WIDTH,
    );
    print_field(
        "Network ID:",
        &status.network.id.clone().dimmed(),
        FIELD_LABEL_WIDTH,
    );
    print_field(
        "DNS suffix:",
        &format!("{}.internal", status.network.dns_name).dimmed(),
        FIELD_LABEL_WIDTH,
    );
    print_field(
        "Address family:",
        &status.network.ip_family.clone().magenta().bold(),
        FIELD_LABEL_WIDTH,
    );
    print_field("Status:", &state_label(status.state), FIELD_LABEL_WIDTH);

    if let Some(hostname) = &status.full_hostname {
        print_field(
            "Hostname:",
            &hostname.clone().magenta().bold(),
            FIELD_LABEL_WIDTH,
        );
    }
    if let Some(short_name) = &status.short_name {
        print_field(
            "Short name:",
            &short_name.clone().magenta(),
            FIELD_LABEL_WIDTH,
        );
    }
    if let Some(pending_hostname) = &status.pending_hostname {
        print_field(
            "Pending:",
            &pending_hostname.clone().blue().bold(),
            FIELD_LABEL_WIDTH,
        );
    }
    if let Some(endpoint) = &status.endpoint {
        print_field(
            "Endpoint ID:",
            &endpoint.id.clone().dimmed(),
            FIELD_LABEL_WIDTH,
        );
        if !endpoint.private_ips.is_empty() {
            print_field(
                "Private IPs:",
                &endpoint.private_ips.join(", "),
                FIELD_LABEL_WIDTH,
            );
        }
    } else {
        print_field(
            "Message:",
            &"Private networking is initializing and will be ready once the deployment of this service is complete."
                .blue(),
            FIELD_LABEL_WIDTH,
        );
    }
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
    private_networks: Vec<PrivateNetworkOutput>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateOutput {
    private_network: PrivateNetworkOutput,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PrivateNetworkOutput {
    network: NetworkOutput,
    state: PrivateNetworkState,
    #[serde(skip_serializing_if = "Option::is_none")]
    endpoint: Option<EndpointOutput>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkOutput {
    id: String,
    name: String,
    dns_name: String,
    dns_suffix: String,
    address_family: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EndpointOutput {
    id: String,
    short_name: String,
    hostname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_hostname: Option<String>,
    sync_status: String,
    private_ips: Vec<String>,
}

impl From<&PrivateNetworkStatus> for PrivateNetworkOutput {
    fn from(status: &PrivateNetworkStatus) -> Self {
        Self {
            network: NetworkOutput {
                id: status.network.id.clone(),
                name: status.network.name.clone(),
                dns_name: status.network.dns_name.clone(),
                dns_suffix: endpoint_dns_suffix(&status.network),
                address_family: status.network.ip_family.clone(),
            },
            state: status.state,
            endpoint: status.endpoint.as_ref().map(|endpoint| EndpointOutput {
                id: endpoint.id.clone(),
                short_name: endpoint.dns_name.clone(),
                hostname: private_network::full_hostname(&endpoint.dns_name, &status.network),
                pending_hostname: status.pending_hostname.clone(),
                sync_status: endpoint.sync_status.clone(),
                private_ips: endpoint.private_ips.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::controllers::private_network::{PrivateNetwork, PrivateNetworkEndpoint};

    use super::*;

    fn status() -> PrivateNetworkStatus {
        private_network::private_network_status(
            PrivateNetwork {
                id: "pn_123".to_string(),
                project_id: "project".to_string(),
                environment_id: "environment".to_string(),
                name: "railway".to_string(),
                dns_name: "railway".to_string(),
                ip_family: "IPv4 & IPv6".to_string(),
                network_id: 1,
                tags: vec!["SUPPORTS_IPV4_PRIVNETS".to_string()],
                created_at: None,
            },
            Some(PrivateNetworkEndpoint {
                id: "pne_123".to_string(),
                service_instance_id: "si_123".to_string(),
                dns_name: "api".to_string(),
                new_dns_name: None,
                private_ips: vec!["fd12::1".to_string()],
                sync_status: "ACTIVE".to_string(),
                tags: vec![],
                created_at: None,
            }),
        )
    }

    #[test]
    fn json_output_excludes_internal_fields() {
        let output = StatusOutput {
            private_networks: vec![PrivateNetworkOutput::from(&status())],
        };
        let value = serde_json::to_value(output).unwrap();

        assert_eq!(
            value["privateNetworks"][0]["network"],
            serde_json::json!({
                "id": "pn_123",
                "name": "railway",
                "dnsName": "railway",
                "dnsSuffix": "railway.internal",
                "addressFamily": "IPv4 & IPv6"
            })
        );
        assert_eq!(
            value["privateNetworks"][0]["endpoint"],
            serde_json::json!({
                "id": "pne_123",
                "shortName": "api",
                "hostname": "api.railway.internal",
                "syncStatus": "ACTIVE",
                "privateIps": ["fd12::1"]
            })
        );
        assert_eq!(value["privateNetworks"][0]["state"], "ready");

        let output = value.to_string();
        assert!(!output.contains("networkId"));
        assert!(!output.contains("tags"));
        assert!(!output.contains("projectId"));
        assert!(!output.contains("environmentId"));
        assert!(!output.contains("serviceInstanceId"));
        assert!(!output.contains("createdAt"));
        assert!(!output.contains("pendingHostname"));
    }
}
