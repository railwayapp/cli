use std::collections::BTreeMap;

use anyhow::{anyhow, bail};
use colored::Colorize;
use is_terminal::IsTerminal;
use serde::Serialize;

use crate::{
    controllers::{
        config::{
            EnvironmentConfig, ServiceInstance, ServiceNetworking, TcpProxyConfig,
            environment::fetch_environment_config,
        },
        project::{ServiceContext, resolve_service_context},
    },
    util::{progress::create_spinner_if, prompt::prompt_confirm_with_default},
};

use super::*;

/// Manage public TCP proxies for a service
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway tcp-proxy list --service postgres --json\n  railway tcp-proxy create --port 5432 --service postgres\n  railway tcp-proxy status tcp-proxy-id\n  railway tcp-proxy delete tcp-proxy-id --yes\n\nAutomation notes:\n  Only one TCP proxy is allowed per service instance.\n  TCP proxy creation updates service networking config. Redeploy the service after committing the change for the proxy to become active."
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

fn parse_port(value: &str) -> std::result::Result<u16, String> {
    let port = value
        .parse::<u16>()
        .map_err(|_| "port must be a number from 1 to 65535".to_string())?;

    if port == 0 {
        return Err("port must be a number from 1 to 65535".to_string());
    }

    Ok(port)
}

async fn list(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let proxies = fetch_tcp_proxies(&ctx).await?;
    let outputs = proxy_outputs(&proxies);

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
    let proxy = resolve_proxy(&ctx, &proxy).await?;

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
    let existing = fetch_tcp_proxies(&ctx).await?;
    let existing_outputs = proxy_outputs(&existing);
    if let Some(proxy) = existing_proxy_for_create(&existing_outputs, port)? {
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
    let patch_mode = apply_tcp_proxy_patch(&ctx, port).await?;
    if patch_mode == PatchMode::Commit {
        verify_tcp_proxy_configured(&ctx, port).await?;
    }
    let active_proxy = fetch_tcp_proxies(&ctx)
        .await?
        .iter()
        .find(|proxy| proxy.application_port == i64::from(port) && proxy.deleted_at.is_none())
        .map(proxy_output);

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
            "Redeploy the service for the TCP proxy to become active, then run {}.",
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
    let proxy = resolve_proxy(&ctx, &proxy).await?;

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
    let deleted = post_graphql::<mutations::TcpProxyDelete, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::tcp_proxy_delete::Variables {
            id: proxy.id.clone(),
        },
    )
    .await?
    .tcp_proxy_delete;

    if !deleted {
        bail!("Failed to delete TCP proxy {}", proxy.id);
    }

    let remaining = fetch_tcp_proxies(&ctx).await?;
    if remaining
        .iter()
        .any(|item| item.id == proxy.id && item.deleted_at.is_none())
    {
        bail!(
            "TCP proxy deletion was requested, but {} still exists after verification.",
            proxy.id
        );
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DeleteOutput {
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

async fn fetch_tcp_proxies(
    ctx: &ServiceContext,
) -> Result<Vec<queries::tcp_proxies::TcpProxiesTcpProxies>> {
    Ok(post_graphql::<queries::TcpProxies, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::tcp_proxies::Variables {
            environment_id: ctx.environment_id.clone(),
            service_id: ctx.service_id.clone(),
        },
    )
    .await?
    .tcp_proxies)
}

fn existing_proxy_for_create(
    proxies: &[TcpProxyOutput],
    port: u16,
) -> Result<Option<&TcpProxyOutput>> {
    let Some(proxy) = proxies.first() else {
        return Ok(None);
    };

    if proxy.application_port == i64::from(port) {
        return Ok(Some(proxy));
    }

    bail!(
        "A TCP proxy already exists for application port {} ({}). Only one TCP proxy is allowed per service instance. Delete it before creating a TCP proxy for application port {}.",
        proxy.application_port,
        proxy.endpoint,
        port
    );
}

async fn resolve_proxy(ctx: &ServiceContext, identifier: &str) -> Result<TcpProxyOutput> {
    let proxies = fetch_tcp_proxies(ctx).await?;
    let outputs = proxy_outputs(&proxies);

    find_proxy(&outputs, identifier)?.cloned().ok_or_else(|| {
        anyhow!(
            "TCP proxy '{}' not found on the selected service",
            identifier
        )
    })
}

fn proxy_outputs(proxies: &[queries::tcp_proxies::TcpProxiesTcpProxies]) -> Vec<TcpProxyOutput> {
    proxies
        .iter()
        .filter(|proxy| proxy.deleted_at.is_none())
        .map(proxy_output)
        .collect()
}

fn proxy_output(proxy: &queries::tcp_proxies::TcpProxiesTcpProxies) -> TcpProxyOutput {
    TcpProxyOutput {
        id: proxy.id.clone(),
        domain: proxy.domain.clone(),
        proxy_port: proxy.proxy_port,
        application_port: proxy.application_port,
        endpoint: format!("{}:{}", proxy.domain, proxy.proxy_port),
        sync_status: tcp_proxy_sync_status(&proxy.sync_status),
        service_id: proxy.service_id.clone(),
        environment_id: proxy.environment_id.clone(),
        created_at: proxy.created_at.as_ref().map(chrono::DateTime::to_rfc3339),
        updated_at: proxy.updated_at.as_ref().map(chrono::DateTime::to_rfc3339),
    }
}

fn find_proxy<'a>(
    proxies: &'a [TcpProxyOutput],
    identifier: &str,
) -> Result<Option<&'a TcpProxyOutput>> {
    if let Some(proxy) = proxies
        .iter()
        .find(|proxy| proxy.id.eq_ignore_ascii_case(identifier))
    {
        return Ok(Some(proxy));
    }

    let normalized = normalize_proxy_identifier(identifier);
    let matches = matching_proxies(proxies, &normalized);

    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches[0])),
        _ => bail!(
            "TCP proxy selector '{}' matched multiple proxies: {}. Use a TCP proxy ID or full endpoint.",
            identifier,
            format_proxy_choices(&matches)
        ),
    }
}

fn matching_proxies<'a>(
    proxies: &'a [TcpProxyOutput],
    normalized: &NormalizedProxyIdentifier,
) -> Vec<&'a TcpProxyOutput> {
    if let Some(port) = normalized.port {
        return proxies
            .iter()
            .filter(|proxy| {
                proxy.domain.eq_ignore_ascii_case(&normalized.host) && proxy.proxy_port == port
            })
            .collect();
    }

    if let Ok(numeric) = normalized.host.parse::<i64>() {
        return proxies
            .iter()
            .filter(|proxy| proxy.proxy_port == numeric || proxy.application_port == numeric)
            .collect();
    }

    proxies
        .iter()
        .filter(|proxy| proxy.domain.eq_ignore_ascii_case(&normalized.host))
        .collect()
}

fn format_proxy_choices(proxies: &[&TcpProxyOutput]) -> String {
    proxies
        .iter()
        .map(|proxy| {
            format!(
                "{} (id: {}, app port: {})",
                proxy.endpoint, proxy.id, proxy.application_port
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedProxyIdentifier {
    host: String,
    port: Option<i64>,
}

fn normalize_proxy_identifier(identifier: &str) -> NormalizedProxyIdentifier {
    let trimmed = identifier.trim();
    let without_scheme = trimmed
        .strip_prefix("tcp://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let endpoint = without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .trim_end_matches('.');

    if let Some((host, port)) = endpoint.rsplit_once(':') {
        if let Ok(port) = port.parse::<i64>() {
            return NormalizedProxyIdentifier {
                host: host.trim_end_matches('.').to_string(),
                port: Some(port),
            };
        }
    }

    NormalizedProxyIdentifier {
        host: endpoint.to_string(),
        port: None,
    }
}

async fn apply_tcp_proxy_patch(ctx: &ServiceContext, port: u16) -> Result<PatchMode> {
    let patch = tcp_proxy_patch(&ctx.service_id, port);
    let mode = patch_mode(ctx);

    match mode {
        PatchMode::Commit => {
            post_graphql::<mutations::EnvironmentPatchCommit, _>(
                &ctx.client,
                ctx.configs.get_backboard(),
                mutations::environment_patch_commit::Variables {
                    environment_id: ctx.environment_id.clone(),
                    patch,
                    commit_message: Some(format!(
                        "Create TCP proxy for {} on port {}",
                        ctx.service_name, port
                    )),
                },
            )
            .await?;
        }
        PatchMode::Stage => {
            post_graphql::<mutations::EnvironmentStageChanges, _>(
                &ctx.client,
                ctx.configs.get_backboard(),
                mutations::environment_stage_changes::Variables {
                    environment_id: ctx.environment_id.clone(),
                    input: patch,
                    merge: Some(true),
                },
            )
            .await?;
        }
    }

    Ok(mode)
}

fn tcp_proxy_patch(service_id: &str, port: u16) -> EnvironmentConfig {
    EnvironmentConfig {
        services: BTreeMap::from([(
            service_id.to_string(),
            ServiceInstance {
                networking: Some(ServiceNetworking {
                    tcp_proxies: BTreeMap::from([(
                        port.to_string(),
                        Some(TcpProxyConfig::default()),
                    )]),
                    ..ServiceNetworking::default()
                }),
                ..ServiceInstance::default()
            },
        )]),
        ..EnvironmentConfig::default()
    }
}

fn patch_mode(ctx: &ServiceContext) -> PatchMode {
    let unmerged_changes = ctx
        .project
        .environments
        .edges
        .iter()
        .find(|env| env.node.id == ctx.environment_id)
        .and_then(|env| env.node.unmerged_changes_count)
        .unwrap_or_default();

    if unmerged_changes > 0 {
        PatchMode::Stage
    } else {
        PatchMode::Commit
    }
}

async fn verify_tcp_proxy_configured(ctx: &ServiceContext, port: u16) -> Result<()> {
    let response = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;

    let configured = response
        .services
        .get(&ctx.service_id)
        .and_then(|service| service.networking.as_ref())
        .is_some_and(|networking| networking.tcp_proxies.contains_key(&port.to_string()));

    if !configured {
        bail!(
            "TCP proxy configuration was requested, but port {} was not present after verification.",
            port
        );
    }

    Ok(())
}

fn print_proxy_table(proxies: &[TcpProxyOutput]) {
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

fn print_proxy_details(proxy: &TcpProxyOutput, title: &str) {
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

fn tcp_proxy_sync_status(value: &queries::tcp_proxies::TCPProxySyncStatus) -> String {
    match value {
        queries::tcp_proxies::TCPProxySyncStatus::ACTIVE => "ACTIVE".to_string(),
        queries::tcp_proxies::TCPProxySyncStatus::CREATING => "CREATING".to_string(),
        queries::tcp_proxies::TCPProxySyncStatus::DELETED => "DELETED".to_string(),
        queries::tcp_proxies::TCPProxySyncStatus::DELETING => "DELETING".to_string(),
        queries::tcp_proxies::TCPProxySyncStatus::UNSPECIFIED => "UNSPECIFIED".to_string(),
        queries::tcp_proxies::TCPProxySyncStatus::UPDATING => "UPDATING".to_string(),
        queries::tcp_proxies::TCPProxySyncStatus::Other(status) => status.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum PatchMode {
    Commit,
    Stage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TcpProxyOutput {
    id: String,
    domain: String,
    proxy_port: i64,
    application_port: i64,
    endpoint: String,
    sync_status: String,
    #[serde(skip_serializing)]
    service_id: String,
    #[serde(skip_serializing)]
    environment_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
}

#[derive(Serialize)]
struct ListOutput {
    proxies: Vec<TcpProxyOutput>,
}

#[derive(Serialize)]
struct ProxyOutput {
    proxy: TcpProxyOutput,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateOutput {
    application_port: u16,
    staged: bool,
    committed: bool,
    proxy: Option<TcpProxyOutput>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteOutput {
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

    fn sample_proxy() -> TcpProxyOutput {
        TcpProxyOutput {
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

    fn sample_proxy_with_ports(id: &str, proxy_port: i64, application_port: i64) -> TcpProxyOutput {
        TcpProxyOutput {
            id: id.to_string(),
            domain: "containers-us-west.railway.app".to_string(),
            proxy_port,
            application_port,
            endpoint: format!("containers-us-west.railway.app:{proxy_port}"),
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
    fn normalizes_proxy_identifier() {
        assert_eq!(
            normalize_proxy_identifier("tcp://containers-us-west.railway.app:15432/path"),
            NormalizedProxyIdentifier {
                host: "containers-us-west.railway.app".to_string(),
                port: Some(15432),
            }
        );
        assert_eq!(
            normalize_proxy_identifier("containers-us-west.railway.app."),
            NormalizedProxyIdentifier {
                host: "containers-us-west.railway.app".to_string(),
                port: None,
            }
        );
    }

    #[test]
    fn finds_proxy_by_id_endpoint_domain_or_port() {
        let proxy = sample_proxy();
        let proxies = vec![proxy];

        assert!(find_proxy(&proxies, "tcp_123").unwrap().is_some());
        assert!(
            find_proxy(&proxies, "containers-us-west.railway.app:15432")
                .unwrap()
                .is_some()
        );
        assert!(
            find_proxy(&proxies, "containers-us-west.railway.app")
                .unwrap()
                .is_some()
        );
        assert!(find_proxy(&proxies, "15432").unwrap().is_some());
        assert!(find_proxy(&proxies, "5432").unwrap().is_some());
        assert!(find_proxy(&proxies, "missing").unwrap().is_none());
    }

    #[test]
    fn create_allows_existing_proxy_only_for_same_application_port() {
        let proxy = sample_proxy();
        let proxies = vec![proxy];

        let existing = existing_proxy_for_create(&proxies, 5432).unwrap().unwrap();
        assert_eq!(existing.id, "tcp_123");

        let err = existing_proxy_for_create(&proxies, 6380).unwrap_err();
        assert!(err.to_string().contains("Only one TCP proxy is allowed"));
        assert!(err.to_string().contains("application port 5432"));
        assert!(err.to_string().contains("application port 6380"));
    }

    #[test]
    fn id_lookup_wins_even_when_numeric_selector_would_be_ambiguous() {
        let proxies = vec![
            sample_proxy_with_ports("15432", 15432, 5432),
            sample_proxy_with_ports("tcp_456", 15433, 15432),
        ];

        let proxy = find_proxy(&proxies, "15432").unwrap().unwrap();

        assert_eq!(proxy.id, "15432");
    }

    #[test]
    fn ambiguous_domain_or_port_selector_errors() {
        let proxies = vec![
            sample_proxy_with_ports("tcp_123", 15432, 5432),
            sample_proxy_with_ports("tcp_456", 15433, 5432),
        ];

        let domain_err = find_proxy(&proxies, "containers-us-west.railway.app").unwrap_err();
        assert!(domain_err.to_string().contains("matched multiple proxies"));

        let port_err = find_proxy(&proxies, "5432").unwrap_err();
        assert!(
            port_err
                .to_string()
                .contains("Use a TCP proxy ID or full endpoint")
        );

        assert!(
            find_proxy(&proxies, "containers-us-west.railway.app:15433")
                .unwrap()
                .is_some()
        );
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
            id: "tcp_123".to_string(),
            endpoint: "containers-us-west.railway.app:15432".to_string(),
            application_port: 5432,
            staged: false,
            committed: true,
        };

        let value = serde_json::to_value(output).unwrap();

        assert_eq!(value["id"], "tcp_123");
        assert_eq!(value["endpoint"], "containers-us-west.railway.app:15432");
        assert_eq!(value["applicationPort"], 5432);
        assert_eq!(value["staged"], false);
        assert_eq!(value["committed"], true);
        assert!(value.get("proxy").is_none());
        assert!(value.get("syncStatus").is_none());
    }

    #[test]
    fn tcp_proxy_sync_status_outputs_clean_strings() {
        assert_eq!(
            tcp_proxy_sync_status(&queries::tcp_proxies::TCPProxySyncStatus::ACTIVE),
            "ACTIVE"
        );
        assert_eq!(
            tcp_proxy_sync_status(&queries::tcp_proxies::TCPProxySyncStatus::Other(
                "NEW_STATUS".to_string()
            )),
            "NEW_STATUS"
        );
    }
}
