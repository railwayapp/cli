use colored::Colorize;
use serde::Serialize;

use crate::{
    controllers::{
        private_network::{
            self, PatchMode, PrivateNetwork, PrivateNetworkEndpoint, parse_endpoint_prefix,
        },
        project::{resolve_environment_context, resolve_service_context},
    },
    util::progress::create_spinner_if,
};

use super::*;

/// Manage private networking using the same config workflow as the dashboard
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway private-network list --json\n  railway private-network status --service api --json\n  railway private-network enable --json\n  railway private-network enable --service api --endpoint api-internal --json\n  railway private-network enable --stage\n  railway private-network set-endpoint api-internal --service api --json\n  railway private-network name-available api-internal --json\n\nAliases:\n  list: ls\n\nDashboard parity notes:\n  This command mirrors the Railway dashboard private networking settings.\n  `enable` and `set-endpoint` update environment config; they do not call low-level endpoint create or rename mutations directly.\n  The dashboard does not expose durable private-network disable or endpoint delete actions, so this command does not either.\n\nAutomation notes:\n  Passing --project requires --environment. By default, changes are committed immediately. Use --stage to stage config changes without committing."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Environment to use (defaults to linked environment)
    #[clap(short, long, global = true)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID", global = true)]
    project: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,
}

#[derive(Parser)]
enum Commands {
    /// List private networks for an environment
    #[clap(
        visible_alias = "ls",
        after_help = "Examples:\n\n  railway private-network list\n  railway private-network list --environment production --json\n\nDashboard parity:\n  Mirrors the dashboard privateNetworks(environmentId) query."
    )]
    List,

    /// Show the selected service's private network endpoint status
    #[clap(
        after_help = "Examples:\n\n  railway private-network status --service api\n  railway private-network status --service api --network railway --json\n\nDashboard parity:\n  Mirrors the dashboard privateNetworkEndpoint(privateNetworkId, environmentId, serviceId) query.\n  If --network is omitted, the CLI uses the environment network named `railway`, falling back to the first network."
    )]
    Status {
        /// Service name or ID (defaults to linked service, or the only service in the project)
        #[clap(short, long)]
        service: Option<String>,

        /// Private network ID, name, DNS suffix, or numeric network ID
        #[clap(long)]
        network: Option<String>,
    },

    /// Enable private networking through environment config
    #[clap(
        after_help = "Examples:\n\n  railway private-network enable\n  railway private-network enable --stage\n  railway private-network enable --service api --endpoint api-internal --message \"Enable private networking\"\n\nDashboard parity:\n  Mirrors the dashboard Enable Private Networking action by setting privateNetworkDisabled=false in environment config.\n  When --endpoint is passed, the same config patch also sets the selected service's networking.privateNetworkEndpoint.\n  The backend apply workflow creates or updates endpoints; this command does not call direct endpoint create or rename mutations."
    )]
    Enable {
        /// Service name or ID, required only when --endpoint is passed unless a service is linked
        #[clap(short, long)]
        service: Option<String>,

        /// Optional endpoint DNS prefix to set on the selected service
        #[clap(long, value_parser = parse_endpoint_prefix)]
        endpoint: Option<String>,

        /// Stage the config change instead of committing it immediately
        #[clap(long)]
        stage: bool,

        /// Commit message to use when applying immediately
        #[clap(short, long)]
        message: Option<String>,
    },

    /// Set a service's private network endpoint name through config
    #[clap(
        after_help = "Examples:\n\n  railway private-network set-endpoint api-internal --service api\n  railway private-network set-endpoint api-renamed --stage\n  railway private-network set-endpoint api-internal --message \"Update private endpoint\" --json\n\nDashboard parity:\n  Mirrors editing the private endpoint field in the dashboard by staging or committing services.<serviceId>.networking.privateNetworkEndpoint.\n  The backend apply workflow performs the endpoint rename; this command does not call the direct rename mutation."
    )]
    SetEndpoint {
        /// Endpoint DNS prefix
        #[clap(value_name = "DNS_PREFIX", value_parser = parse_endpoint_prefix)]
        endpoint: String,

        /// Service name or ID (defaults to linked service, or the only service in the project)
        #[clap(short, long)]
        service: Option<String>,

        /// Stage the config change instead of committing it immediately
        #[clap(long)]
        stage: bool,

        /// Commit message to use when applying immediately
        #[clap(short, long)]
        message: Option<String>,
    },

    /// Check whether a private endpoint name is available
    #[clap(
        after_help = "Examples:\n\n  railway private-network name-available api-internal\n  railway private-network name-available api-internal --network railway --json\n\nDashboard parity:\n  Mirrors the dashboard privateNetworkEndpointNameAvailable query and validates the same simple DNS-prefix shape before querying."
    )]
    NameAvailable {
        /// Endpoint DNS prefix
        #[clap(value_name = "DNS_PREFIX", value_parser = parse_endpoint_prefix)]
        endpoint: String,

        /// Private network ID, name, DNS suffix, or numeric network ID
        #[clap(long)]
        network: Option<String>,
    },
}

pub async fn command(args: Args) -> Result<()> {
    let Args {
        command,
        environment,
        project,
        json,
    } = args;

    crate::util::reporter::set_mode(json);

    match command {
        Commands::List => list(project, environment, json).await?,
        Commands::Status { service, network } => {
            status(project, service, environment, network, json).await?
        }
        Commands::Enable {
            service,
            endpoint,
            stage,
            message,
        } => {
            enable(
                project,
                service,
                environment,
                endpoint,
                stage,
                message,
                json,
            )
            .await?
        }
        Commands::SetEndpoint {
            endpoint,
            service,
            stage,
            message,
        } => {
            set_endpoint(
                project,
                service,
                environment,
                endpoint,
                stage,
                message,
                json,
            )
            .await?
        }
        Commands::NameAvailable { endpoint, network } => {
            name_available(project, environment, endpoint, network, json).await?
        }
    }

    Ok(())
}

async fn list(project: Option<String>, environment: Option<String>, json: bool) -> Result<()> {
    let ctx = resolve_environment_context(project, environment).await?;
    let networks =
        private_network::fetch_private_networks(&ctx.client, &ctx.configs, &ctx.environment_id)
            .await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ListOutput { networks })?
        );
        return Ok(());
    }

    if networks.is_empty() {
        println!(
            "No private networks found in environment {}.",
            ctx.environment_name.bold()
        );
        return Ok(());
    }

    println!(
        "Private networks for environment {}:",
        ctx.environment_name.bold()
    );
    print_network_table(&networks);

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
    let networks =
        private_network::fetch_private_networks(&ctx.client, &ctx.configs, &ctx.environment_id)
            .await?;
    let network = private_network::resolve_private_network(&networks, network.as_deref())?.cloned();

    let endpoint = match &network {
        Some(network) => {
            private_network::fetch_private_network_endpoint(
                &ctx.client,
                &ctx.configs,
                &network.public_id,
                &ctx.environment_id,
                &ctx.service_id,
            )
            .await?
        }
        None => None,
    };
    let configured_endpoint_name = if network.is_some() && endpoint.is_some() {
        private_network::fetch_configured_endpoint_name(
            &ctx.client,
            &ctx.configs,
            &ctx.environment_id,
            &ctx.service_id,
        )
        .await?
    } else {
        None
    };
    let internal_url = network
        .as_ref()
        .zip(endpoint.as_ref())
        .map(|(network, endpoint)| {
            private_network::internal_url_for_dns_name(
                network,
                configured_endpoint_name
                    .as_deref()
                    .unwrap_or(&endpoint.dns_name),
            )
        });

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&StatusOutput {
                network,
                endpoint,
                internal_url,
            })?
        );
        return Ok(());
    }

    print_status(
        &ctx.service_name,
        network.as_ref(),
        endpoint.as_ref(),
        internal_url.as_deref(),
    );

    Ok(())
}

async fn enable(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    endpoint: Option<String>,
    stage: bool,
    message: Option<String>,
    json: bool,
) -> Result<()> {
    if let Some(endpoint) = endpoint {
        let ctx = resolve_service_context(project, service, environment).await?;
        let spinner = create_spinner_if(!json, "Updating private networking config...".into());
        let mode = private_network::apply_enable_patch(
            &ctx.client,
            &ctx.configs,
            &ctx.environment_id,
            Some(&ctx.service_id),
            Some(&endpoint),
            stage,
            Some(message.unwrap_or_else(|| {
                format!(
                    "Enable private networking for {} with endpoint {}",
                    ctx.service_name, endpoint
                )
            })),
        )
        .await?;

        if mode == PatchMode::Commit {
            private_network::verify_private_network_enabled(
                &ctx.client,
                &ctx.configs,
                &ctx.environment_id,
            )
            .await?;
            private_network::verify_endpoint_configured(
                &ctx.client,
                &ctx.configs,
                &ctx.environment_id,
                &ctx.service_id,
                &endpoint,
            )
            .await?;
        }

        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&MutationOutput {
                    staged: mode == PatchMode::Stage,
                    committed: mode == PatchMode::Commit,
                    environment_id: ctx.environment_id,
                    service_id: Some(ctx.service_id),
                })?
            );
            return Ok(());
        }

        finish_private_network_change(spinner, mode, &ctx.environment_name, Some(&endpoint));
        return Ok(());
    }

    let ctx = resolve_environment_context(project, environment).await?;
    let spinner = create_spinner_if(!json, "Updating private networking config...".into());
    let mode = private_network::apply_enable_patch(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        None,
        None,
        stage,
        Some(message.unwrap_or_else(|| "Enable private networking".to_string())),
    )
    .await?;

    if mode == PatchMode::Commit {
        private_network::verify_private_network_enabled(
            &ctx.client,
            &ctx.configs,
            &ctx.environment_id,
        )
        .await?;
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&MutationOutput {
                staged: mode == PatchMode::Stage,
                committed: mode == PatchMode::Commit,
                environment_id: ctx.environment_id,
                service_id: None,
            })?
        );
        return Ok(());
    }

    finish_private_network_change(spinner, mode, &ctx.environment_name, None);

    Ok(())
}

async fn set_endpoint(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    endpoint: String,
    stage: bool,
    message: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let spinner = create_spinner_if(!json, "Updating private endpoint config...".into());
    let mode = private_network::apply_set_endpoint_patch(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
        &endpoint,
        stage,
        Some(message.unwrap_or_else(|| {
            format!(
                "Set private network endpoint for {} to {}",
                ctx.service_name, endpoint
            )
        })),
    )
    .await?;

    if mode == PatchMode::Commit {
        private_network::verify_endpoint_configured(
            &ctx.client,
            &ctx.configs,
            &ctx.environment_id,
            &ctx.service_id,
            &endpoint,
        )
        .await?;
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&MutationOutput {
                staged: mode == PatchMode::Stage,
                committed: mode == PatchMode::Commit,
                environment_id: ctx.environment_id,
                service_id: Some(ctx.service_id),
            })?
        );
        return Ok(());
    }

    finish_private_network_change(spinner, mode, &ctx.environment_name, Some(&endpoint));

    Ok(())
}

async fn name_available(
    project: Option<String>,
    environment: Option<String>,
    endpoint: String,
    network: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_environment_context(project, environment).await?;
    let networks =
        private_network::fetch_private_networks(&ctx.client, &ctx.configs, &ctx.environment_id)
            .await?;
    let network = private_network::resolve_private_network(&networks, network.as_deref())?
        .cloned()
        .context("No private network found in the selected environment")?;
    let available = private_network::private_network_endpoint_name_available(
        &ctx.client,
        &ctx.configs,
        &network.public_id,
        &ctx.environment_id,
        &endpoint,
    )
    .await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&NameAvailableOutput {
                endpoint,
                available,
                network,
            })?
        );
        return Ok(());
    }

    if available {
        println!("Endpoint name {} is available.", endpoint.green());
    } else {
        println!("Endpoint name {} is already used.", endpoint.red());
    }

    Ok(())
}

fn finish_private_network_change(
    spinner: Option<indicatif::ProgressBar>,
    mode: PatchMode,
    environment_name: &str,
    endpoint: Option<&str>,
) {
    let suffix = endpoint
        .map(|endpoint| format!(" Endpoint: {}.", endpoint.cyan()))
        .unwrap_or_default();
    let msg = match mode {
        PatchMode::Commit => format!(
            "Committed private networking config in environment {}.{}",
            environment_name.magenta().bold(),
            suffix
        ),
        PatchMode::Stage => format!(
            "Staged private networking config in environment {}.{} {}",
            environment_name.magenta().bold(),
            suffix,
            "(use 'railway environment edit' to commit)".dimmed()
        ),
    };

    if let Some(spinner) = spinner {
        spinner.finish_with_message(msg);
    } else {
        println!("{msg}");
    }
}

fn print_network_table(networks: &[PrivateNetwork]) {
    for network in networks {
        let family = if network.supports_ipv4 {
            "IPv4 & IPv6"
        } else {
            "IPv6"
        };
        println!(
            "- {} (id: {}, dns: {}.internal, networkId: {}, {})",
            network.name.bold(),
            network.public_id,
            network.dns_name,
            network.network_id,
            family
        );
    }
}

fn print_status(
    service_name: &str,
    network: Option<&PrivateNetwork>,
    endpoint: Option<&PrivateNetworkEndpoint>,
    internal_url: Option<&str>,
) {
    let Some(network) = network else {
        println!("No private network found for the selected environment.");
        return;
    };

    println!(
        "Private network endpoint for service {}:",
        service_name.bold()
    );
    println!("  {} {}", "network:".dimmed(), network.name);
    println!("  {} {}", "network id:".dimmed(), network.public_id);
    println!("  {} {}.internal", "dns suffix:".dimmed(), network.dns_name);
    println!(
        "  {} {}",
        "address family:".dimmed(),
        if network.supports_ipv4 {
            "IPv4 & IPv6"
        } else {
            "IPv6"
        }
    );

    let Some(endpoint) = endpoint else {
        println!(
            "  {} {}",
            "endpoint:".dimmed(),
            "initializing; deploy or apply pending config changes to create it".yellow()
        );
        return;
    };

    println!("  {} {}", "endpoint id:".dimmed(), endpoint.public_id);
    println!("  {} {}", "endpoint name:".dimmed(), endpoint.dns_name);
    if let Some(internal_url) = internal_url {
        println!("  {} {}", "internal url:".dimmed(), internal_url.cyan());
    }
    println!("  {} {}", "sync status:".dimmed(), endpoint.sync_status);
    if let Some(new_dns_name) = &endpoint.new_dns_name {
        println!("  {} {}", "pending name:".dimmed(), new_dns_name);
    }
    if !endpoint.private_ips.is_empty() {
        println!(
            "  {} {}",
            "private ips:".dimmed(),
            endpoint.private_ips.join(", ")
        );
    }
}

#[derive(Serialize)]
struct ListOutput {
    networks: Vec<PrivateNetwork>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusOutput {
    network: Option<PrivateNetwork>,
    endpoint: Option<PrivateNetworkEndpoint>,
    internal_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MutationOutput {
    staged: bool,
    committed: bool,
    environment_id: String,
    service_id: Option<String>,
}

#[derive(Serialize)]
struct NameAvailableOutput {
    endpoint: String,
    available: bool,
    network: PrivateNetwork,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_network() -> PrivateNetwork {
        PrivateNetwork {
            public_id: "pn_123".to_string(),
            project_id: "project_123".to_string(),
            environment_id: "env_123".to_string(),
            name: "railway".to_string(),
            dns_name: "railway".to_string(),
            network_id: 42,
            tags: vec![],
            supports_ipv4: false,
            created_at: None,
            deleted_at: None,
        }
    }

    fn sample_endpoint() -> PrivateNetworkEndpoint {
        PrivateNetworkEndpoint {
            public_id: "pne_123".to_string(),
            service_instance_id: "si_123".to_string(),
            dns_name: "api-internal".to_string(),
            new_dns_name: None,
            private_ips: vec!["fd12::1".to_string()],
            tags: vec![],
            sync_status: "ACTIVE".to_string(),
            created_at: None,
            deleted_at: None,
        }
    }

    #[test]
    fn json_outputs_use_documented_shapes() {
        let list = serde_json::to_value(ListOutput {
            networks: vec![sample_network()],
        })
        .unwrap();
        assert!(list.get("networks").is_some());

        let status = serde_json::to_value(StatusOutput {
            network: Some(sample_network()),
            endpoint: Some(sample_endpoint()),
            internal_url: Some("api-internal.railway.internal".to_string()),
        })
        .unwrap();
        assert!(status.get("network").is_some());
        assert!(status.get("endpoint").is_some());
        assert_eq!(
            status
                .get("internalUrl")
                .and_then(serde_json::Value::as_str),
            Some("api-internal.railway.internal")
        );

        let mutation = serde_json::to_value(MutationOutput {
            staged: true,
            committed: false,
            environment_id: "env_123".to_string(),
            service_id: Some("svc_123".to_string()),
        })
        .unwrap();
        assert_eq!(
            mutation.get("staged").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            mutation
                .get("committed")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            mutation
                .get("environmentId")
                .and_then(serde_json::Value::as_str),
            Some("env_123")
        );
        assert_eq!(
            mutation
                .get("serviceId")
                .and_then(serde_json::Value::as_str),
            Some("svc_123")
        );
    }
}
