use anyhow::bail;
use colored::Colorize;
use is_terminal::IsTerminal;
use serde::Serialize;

use crate::{
    controllers::{
        project::resolve_service_context,
        tcp_proxy::{self, PatchMode, TcpProxy, parse_port},
    },
    util::{progress::create_spinner_if, prompt::prompt_confirm_with_default},
};

use super::*;

/// Manage public TCP proxies for a service
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway tcp-proxy list --service postgres --json\n  railway tcp-proxy create --port 5432 --service postgres\n  railway tcp-proxy status tcp-proxy-id\n  railway tcp-proxy delete tcp-proxy-id --yes\n\nAutomation notes:\n  Only one TCP proxy is allowed per service instance.\n  TCP proxy creation updates service networking config. If the proxy does not become active, redeploy the service and check its status."
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

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,
}

#[derive(Parser)]
enum Commands {
    /// List TCP proxies for a service
    #[clap(visible_alias = "ls")]
    List,

    /// Create a TCP proxy for an application port
    #[clap(visible_alias = "add", visible_alias = "new")]
    Create {
        /// Application port to expose through the TCP proxy
        #[clap(long, value_parser = parse_port)]
        port: u16,
    },

    /// Show status for a TCP proxy
    Status {
        /// TCP proxy ID, domain, endpoint, proxy port, or application port
        #[clap(value_name = "PROXY")]
        proxy: String,
    },

    /// Delete a TCP proxy
    #[clap(visible_alias = "remove", visible_alias = "rm")]
    Delete {
        /// TCP proxy ID, domain, endpoint, proxy port, or application port
        #[clap(value_name = "PROXY")]
        proxy: String,

        /// Skip confirmation dialog
        #[clap(short = 'y', long = "yes")]
        yes: bool,
    },
}

pub async fn command(args: Args) -> Result<()> {
    let Args {
        command,
        service,
        environment,
        project,
        json,
    } = args;

    crate::util::reporter::set_mode(json);

    match command {
        Commands::List => list(project, service, environment, json).await?,
        Commands::Create { port } => create(project, service, environment, port, json).await?,
        Commands::Status { proxy } => status(project, service, environment, proxy, json).await?,
        Commands::Delete { proxy, yes } => {
            delete(project, service, environment, proxy, yes, json).await?
        }
    }

    Ok(())
}

async fn list(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let outputs = tcp_proxy::fetch_tcp_proxies(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
    )
    .await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ListOutput { proxies: outputs })?
        );
        return Ok(());
    }

    if outputs.is_empty() {
        println!(
            "No TCP proxies found for service {} in environment {}.",
            ctx.service_name.bold(),
            ctx.environment_name.bold()
        );
        return Ok(());
    }

    println!(
        "TCP proxies for service {} in environment {}:",
        ctx.service_name.bold(),
        ctx.environment_name.bold()
    );
    print_proxy_table(&outputs);

    Ok(())
}

async fn status(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    proxy: String,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let proxy = tcp_proxy::resolve_tcp_proxy(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
        &proxy,
    )
    .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&ProxyOutput { proxy })?);
        return Ok(());
    }

    print_proxy_details(&proxy, "TCP proxy status");

    Ok(())
}

async fn create(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    port: u16,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let existing = tcp_proxy::fetch_tcp_proxies(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
    )
    .await?;
    if let Some(proxy) = tcp_proxy::existing_proxy_for_create(&existing, port)? {
        let proxy = proxy.clone();
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&CreateOutput {
                    application_port: port,
                    staged: false,
                    committed: false,
                    proxy: Some(proxy),
                })?
            );
        } else {
            println!(
                "TCP proxy already exists for application port {}:",
                port.to_string().cyan()
            );
            print_proxy_details(&proxy, "Existing TCP proxy");
        }
        return Ok(());
    }

    let spinner = create_spinner_if(!json, "Configuring TCP proxy...".into());
    let patch_mode = tcp_proxy::apply_tcp_proxy_patch(
        &ctx.client,
        &ctx.configs,
        &ctx.project,
        &ctx.environment_id,
        &ctx.service_id,
        &ctx.service_name,
        port,
    )
    .await?;
    if patch_mode == PatchMode::Commit {
        tcp_proxy::verify_tcp_proxy_configured(
            &ctx.client,
            &ctx.configs,
            &ctx.environment_id,
            &ctx.service_id,
            port,
        )
        .await?;
    }
    let active_proxy = tcp_proxy::fetch_tcp_proxies(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
    )
    .await?
    .into_iter()
    .find(|proxy| proxy.application_port == i64::from(port));

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&CreateOutput {
                application_port: port,
                staged: patch_mode == PatchMode::Stage,
                committed: patch_mode == PatchMode::Commit,
                proxy: active_proxy,
            })?
        );
        return Ok(());
    }

    let msg = match patch_mode {
        PatchMode::Commit => format!(
            "Configured TCP proxy for service {} on application port {}.",
            ctx.service_name.blue(),
            port.to_string().cyan()
        ),
        PatchMode::Stage => format!(
            "Staged TCP proxy for service {} on application port {} in {} {}",
            ctx.service_name.blue(),
            port.to_string().cyan(),
            ctx.environment_name.magenta().bold(),
            "(use 'railway environment edit' to commit)".dimmed()
        ),
    };

    if let Some(spinner) = spinner {
        spinner.finish_with_message(msg);
    } else {
        println!("{msg}");
    }

    if let Some(proxy) = active_proxy {
        print_proxy_details(&proxy, "Active TCP proxy");
    } else {
        println!(
            "The TCP proxy is configured but is not readable yet. Run {} shortly; if it does not become active, redeploy the service.",
            format!("railway tcp-proxy list --service {}", ctx.service_name).bold()
        );
    }

    Ok(())
}

async fn delete(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    proxy: String,
    yes: bool,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let proxy = tcp_proxy::resolve_tcp_proxy(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
        &proxy,
    )
    .await?;

    let confirmed = if yes {
        true
    } else if std::io::stdout().is_terminal() {
        prompt_confirm_with_default(
            &format!(
                "Delete TCP proxy {}? This action cannot be undone.",
                proxy.endpoint.red()
            ),
            false,
        )?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };

    if !confirmed {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "deleted": false,
                    "proxy": proxy,
                }))?
            );
        } else {
            println!("Deletion cancelled.");
        }
        return Ok(());
    }

    let spinner = create_spinner_if(!json, "Deleting TCP proxy...".into());
    tcp_proxy::delete_tcp_proxy(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
        &proxy,
    )
    .await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DeleteOutput {
                deleted: true,
                id: proxy.id,
                endpoint: proxy.endpoint,
                application_port: proxy.application_port,
                staged: false,
                committed: true,
            })?
        );
    } else if let Some(spinner) = spinner {
        spinner.finish_with_message(format!("Deleted TCP proxy {}", proxy.endpoint.blue()));
    } else {
        println!("Deleted TCP proxy {}.", proxy.endpoint.blue());
    }

    Ok(())
}

fn print_proxy_table(proxies: &[TcpProxy]) {
    let endpoint_width = proxies
        .iter()
        .map(|proxy| proxy.endpoint.len())
        .max()
        .unwrap_or("Endpoint".len())
        .max("Endpoint".len())
        + 3;
    let app_width = proxies
        .iter()
        .map(|proxy| proxy.application_port.to_string().len())
        .max()
        .unwrap_or("App Port".len())
        .max("App Port".len())
        + 3;
    let id_width = proxies
        .iter()
        .map(|proxy| proxy.id.len())
        .max()
        .unwrap_or("ID".len())
        .max("ID".len())
        + 3;

    println!(
        "{:<endpoint_width$}{:<app_width$}{:<id_width$}Sync",
        "Endpoint".bold(),
        "App Port".bold(),
        "ID".bold(),
        endpoint_width = endpoint_width,
        app_width = app_width,
        id_width = id_width,
    );

    for proxy in proxies {
        println!(
            "{:<endpoint_width$}{:<app_width$}{:<id_width$}{}",
            proxy.endpoint,
            proxy.application_port,
            proxy.id,
            proxy.sync_status,
            endpoint_width = endpoint_width,
            app_width = app_width,
            id_width = id_width,
        );
    }
}

fn print_proxy_details(proxy: &TcpProxy, title: &str) {
    println!("{}:", title.bold());
    println!("  Endpoint: {}", proxy.endpoint.magenta().bold());
    println!("  ID: {}", proxy.id);
    println!("  Domain: {}", proxy.domain);
    println!("  Proxy port: {}", proxy.proxy_port);
    println!("  Application port: {}", proxy.application_port);
    println!("  Sync status: {}", proxy.sync_status);

    if let Some(created_at) = &proxy.created_at {
        println!("  Created: {}", created_at);
    }
    if let Some(updated_at) = &proxy.updated_at {
        println!("  Updated: {}", updated_at);
    }
}

#[derive(Serialize)]
struct ListOutput {
    proxies: Vec<TcpProxy>,
}

#[derive(Serialize)]
struct ProxyOutput {
    proxy: TcpProxy,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateOutput {
    application_port: u16,
    staged: bool,
    committed: bool,
    proxy: Option<TcpProxy>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteOutput {
    deleted: bool,
    id: String,
    endpoint: String,
    application_port: i64,
    staged: bool,
    committed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn sample_proxy() -> TcpProxy {
        TcpProxy {
            id: "tcp_123".to_string(),
            domain: "containers-us-west.railway.app".to_string(),
            proxy_port: 15432,
            application_port: 5432,
            endpoint: "containers-us-west.railway.app:15432".to_string(),
            sync_status: "ACTIVE".to_string(),
            service_id: "svc_123".to_string(),
            environment_id: "env_123".to_string(),
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn parses_subcommands() {
        assert!(matches!(
            Args::parse_from(["tcp-proxy", "list"]).command,
            Commands::List
        ));
        assert!(matches!(
            Args::parse_from(["tcp-proxy", "create", "--port", "5432"]).command,
            Commands::Create { port: 5432 }
        ));
        assert!(matches!(
            Args::parse_from(["tcp-proxy", "status", "tcp_123"]).command,
            Commands::Status { proxy } if proxy == "tcp_123"
        ));
        assert!(matches!(
            Args::parse_from(["tcp-proxy", "delete", "tcp_123", "--yes"]).command,
            Commands::Delete { proxy, yes: true } if proxy == "tcp_123"
        ));
    }

    #[test]
    fn validates_port_range() {
        assert_eq!(parse_port("1").unwrap(), 1);
        assert_eq!(parse_port("65535").unwrap(), 65535);
        assert!(parse_port("0").is_err());
        assert!(parse_port("65536").is_err());
    }

    #[test]
    fn create_output_keeps_proxy_key_when_proxy_is_unavailable() {
        let output = CreateOutput {
            application_port: 5432,
            staged: false,
            committed: true,
            proxy: None,
        };

        let value = serde_json::to_value(output).unwrap();

        assert_eq!(value["applicationPort"], 5432);
        assert_eq!(value["staged"], false);
        assert_eq!(value["committed"], true);
        assert!(value.get("proxy").is_some());
        assert!(value["proxy"].is_null());
    }

    #[test]
    fn delete_output_is_compact_and_not_a_stale_proxy_snapshot() {
        let output = DeleteOutput {
            deleted: true,
            id: "tcp_123".to_string(),
            endpoint: "containers-us-west.railway.app:15432".to_string(),
            application_port: 5432,
            staged: false,
            committed: true,
        };

        let value = serde_json::to_value(output).unwrap();

        assert_eq!(value["deleted"], true);
        assert_eq!(value["id"], "tcp_123");
        assert_eq!(value["endpoint"], "containers-us-west.railway.app:15432");
        assert_eq!(value["applicationPort"], 5432);
        assert_eq!(value["staged"], false);
        assert_eq!(value["committed"], true);
        assert!(value.get("proxy").is_none());
        assert!(value.get("syncStatus").is_none());
    }

    #[test]
    fn proxy_output_omits_internal_context_fields() {
        let value = serde_json::to_value(sample_proxy()).unwrap();

        assert_eq!(value["id"], "tcp_123");
        assert_eq!(value["endpoint"], "containers-us-west.railway.app:15432");
        assert!(value.get("serviceId").is_none());
        assert!(value.get("environmentId").is_none());
    }
}
