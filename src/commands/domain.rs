use std::{cmp::max, fmt, time::Duration};

use anyhow::{anyhow, bail};
use clap::Subcommand;
use colored::Colorize;
use is_terminal::IsTerminal;
use queries::domains::DomainsDomains;
use serde::Serialize;
use serde_json::json;

use crate::{
    consts::TICK_STRING,
    controllers::project::{ServiceContext, resolve_service_context},
    util::prompt::prompt_confirm_with_default,
};

use super::*;

/// Add, list, inspect, update, or delete domains for a service.
///
/// Running without a subcommand preserves the original create behavior:
/// - `railway domain` generates a Railway-provided service domain
/// - `railway domain example.com` creates a custom domain
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway domain\n  railway domain example.com --port 3000\n  railway domain list --service api --json\n  railway domain status example.com\n  railway domain update example.com --port 8080\n  railway domain certificate retry example.com\n  railway domain delete example.com --yes"
)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,

    /// The port to connect to the domain when creating a domain
    #[clap(short, long, value_parser = parse_port)]
    port: Option<u16>,

    /// The name of the service to manage domains for
    #[clap(short, long, global = true)]
    service: Option<String>,

    /// Environment to use (defaults to linked environment)
    #[clap(short, long, global = true)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(long, value_name = "PROJECT_ID", global = true)]
    project: Option<String>,

    /// Optionally, specify a custom domain to use. If not specified, a domain will be generated.
    ///
    /// Specifying a custom domain will also return the required DNS records
    /// to add to your DNS settings.
    domain: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// List domains for a service
    #[clap(visible_alias = "ls")]
    List,

    /// Show status and DNS details for a domain
    Status {
        /// Domain name, URL, or domain ID
        #[clap(value_name = "DOMAIN_OR_ID")]
        domain: String,
    },

    /// Delete a custom or service domain
    #[clap(visible_alias = "remove", visible_alias = "rm")]
    Delete {
        /// Domain name, URL, or domain ID
        #[clap(value_name = "DOMAIN_OR_ID")]
        domain: String,

        /// Skip confirmation dialog
        #[clap(short = 'y', long = "yes")]
        yes: bool,
    },

    /// Update a domain
    #[clap(visible_alias = "edit")]
    Update {
        /// Domain name, URL, or domain ID
        #[clap(value_name = "DOMAIN_OR_ID")]
        identifier: String,

        /// The target port to route HTTP traffic to
        #[clap(long, value_parser = parse_port)]
        port: Option<u16>,

        /// Rename a Railway-provided service domain. Accepts a full service domain or host label.
        #[clap(long = "domain", value_name = "DOMAIN")]
        new_domain: Option<String>,
    },

    /// Manage custom domain certificates
    Certificate {
        #[clap(subcommand)]
        command: CertificateCommands,
    },
}

#[derive(Subcommand)]
enum CertificateCommands {
    /// Retry certificate issuance for a custom domain
    Retry {
        /// Domain name, URL, or domain ID
        #[clap(value_name = "DOMAIN_OR_ID")]
        domain: String,
    },
}

pub async fn command(args: Args) -> Result<()> {
    let Args {
        command,
        port,
        service,
        environment,
        project,
        domain,
        json,
    } = args;

    match command {
        Some(Commands::List) => list_domains(project, service, environment, json).await?,
        Some(Commands::Status { domain }) => {
            show_domain_status(domain, project, service, environment, json).await?
        }
        Some(Commands::Delete { domain, yes }) => {
            delete_domain(domain, project, service, environment, yes, json).await?
        }
        Some(Commands::Update {
            identifier,
            port,
            new_domain,
        }) => {
            update_domain(
                identifier,
                port,
                new_domain,
                project,
                service,
                environment,
                json,
            )
            .await?
        }
        Some(Commands::Certificate { command }) => match command {
            CertificateCommands::Retry { domain } => {
                retry_domain_certificate(domain, project, service, environment, json).await?
            }
        },
        None => {
            if let Some(domain) = domain {
                create_custom_domain(domain, port, project, service, environment, json).await?;
            } else {
                create_service_domain(project, service, environment, port, json).await?;
            }
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

async fn list_domains(
    project: Option<String>,
    service_name: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service_name, environment).await?;
    let domains = fetch_domains(&ctx).await?;
    let items = domain_items(&domains);
    let summaries = summaries(&items);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ListOutput { domains: summaries })?
        );
        return Ok(());
    }

    if items.is_empty() {
        println!(
            "No domains found for service {} in environment {}.",
            ctx.service_name.bold(),
            ctx.environment_name.bold()
        );
        return Ok(());
    }

    println!(
        "Domains for service {} in environment {}:",
        ctx.service_name.bold(),
        ctx.environment_name.bold()
    );
    print_domain_table(&items);

    Ok(())
}

async fn show_domain_status(
    identifier: String,
    project: Option<String>,
    service_name: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service_name, environment).await?;
    let domain = resolve_domain(&ctx, &identifier).await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DomainOutput { domain })?
        );
        return Ok(());
    }

    print_domain_details(&domain, "Domain status", false);

    Ok(())
}

async fn delete_domain(
    identifier: String,
    project: Option<String>,
    service_name: Option<String>,
    environment: Option<String>,
    yes: bool,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service_name, environment).await?;
    let domain = resolve_domain(&ctx, &identifier).await?;

    let confirmed = confirm_delete(yes, std::io::stdout().is_terminal(), || {
        prompt_confirm_with_default(
            &format!(
                "Delete {} domain {}? This action cannot be undone.",
                domain.summary.kind,
                domain.summary.domain.red()
            ),
            false,
        )
    })?;

    if !confirmed {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "deleted": false,
                    "domain": domain.summary,
                }))?
            );
        } else {
            println!("Deletion cancelled.");
        }
        return Ok(());
    }

    match domain.summary.kind {
        DomainKind::Custom => {
            post_graphql::<mutations::CustomDomainDelete, _>(
                &ctx.client,
                ctx.configs.get_backboard(),
                mutations::custom_domain_delete::Variables {
                    id: domain.summary.id.clone(),
                },
            )
            .await?;
        }
        DomainKind::Service => {
            post_graphql::<mutations::ServiceDomainDelete, _>(
                &ctx.client,
                ctx.configs.get_backboard(),
                mutations::service_domain_delete::Variables {
                    id: domain.summary.id.clone(),
                },
            )
            .await?;
        }
    }

    let domains = fetch_domains(&ctx).await?;
    let remaining = domain_items(&domains);
    if find_domain_details(&remaining, &domain.summary.id).is_some() {
        bail!(
            "Domain deletion was requested, but {} still exists after verification.",
            domain.summary.id
        );
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "deleted": true,
                "domain": domain.summary,
            }))?
        );
    } else {
        println!(
            "Deleted {} domain {}.",
            domain.summary.kind,
            domain.summary.domain.magenta().bold()
        );
    }

    Ok(())
}

async fn update_domain(
    identifier: String,
    port: Option<u16>,
    new_domain: Option<String>,
    project: Option<String>,
    service_name: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    if port.is_none() && new_domain.is_none() {
        bail!("Specify --port, --domain, or both.");
    }

    let ctx = resolve_service_context(project, service_name, environment).await?;
    let domain = resolve_domain(&ctx, &identifier).await?;
    let target_port = port.map(|port| port as i64);

    match domain.summary.kind {
        DomainKind::Custom => {
            if new_domain.is_some() {
                bail!(
                    "Custom domains cannot be renamed. Create the new custom domain, then delete the old one."
                );
            }

            post_graphql::<mutations::CustomDomainUpdate, _>(
                &ctx.client,
                ctx.configs.get_backboard(),
                mutations::custom_domain_update::Variables {
                    environment_id: domain.summary.environment_id.clone(),
                    id: domain.summary.id.clone(),
                    target_port,
                },
            )
            .await?;
        }
        DomainKind::Service => {
            let service_domain = new_domain
                .as_deref()
                .map(|new_domain| service_domain_input(&domain, new_domain))
                .transpose()?
                .unwrap_or_else(|| domain.summary.domain.clone());

            post_graphql::<mutations::ServiceDomainUpdate, _>(
                &ctx.client,
                ctx.configs.get_backboard(),
                mutations::service_domain_update::Variables {
                    input: mutations::service_domain_update::ServiceDomainUpdateInput {
                        domain: service_domain,
                        environment_id: domain.summary.environment_id.clone(),
                        service_domain_id: domain.summary.id.clone(),
                        service_id: domain.summary.service_id.clone(),
                        target_port,
                    },
                },
            )
            .await?;
        }
    }

    let updated = resolve_domain(&ctx, &domain.summary.id).await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DomainOutput { domain: updated })?
        );
    } else {
        let changes = update_change_summary(port, new_domain.as_deref());
        println!("Updated {}.", changes);
        print_domain_details(&updated, "Updated domain status", false);
    }

    Ok(())
}

async fn retry_domain_certificate(
    identifier: String,
    project: Option<String>,
    service_name: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service_name, environment).await?;
    let domain = resolve_domain(&ctx, &identifier).await?;

    if domain.summary.kind != DomainKind::Custom {
        bail!("Certificate retry is only supported for custom domains.");
    }
    if let Some(reason) = certificate_retry_unavailable_reason(&domain) {
        bail!("{reason}");
    }

    post_graphql::<mutations::CustomDomainIssueCertificate, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::custom_domain_issue_certificate::Variables {
            id: domain.summary.id.clone(),
        },
    )
    .await?;

    let updated = resolve_domain(&ctx, &domain.summary.id).await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DomainOutput { domain: updated })?
        );
    } else {
        println!("Certificate retry requested.");
        print_domain_details(&updated, "Updated domain status", false);
    }

    Ok(())
}

async fn create_service_domain(
    project: Option<String>,
    service_name: Option<String>,
    environment: Option<String>,
    port: Option<u16>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service_name, environment).await?;

    let domains = fetch_domains(&ctx).await?;

    let domain_count = domains.service_domains.len() + domains.custom_domains.len();
    if domain_count > 0 {
        return print_existing_domains(&domains, json);
    }

    let spinner = (std::io::stdout().is_terminal() && !json)
        .then(|| creating_domain_spiner(None))
        .and_then(|s| s.ok());

    let vars = mutations::service_domain_create::Variables {
        environment_id: ctx.environment_id.clone(),
        service_id: ctx.service_id.clone(),
        target_port: port.map(|port| port as i64),
    };
    let domain = post_graphql::<mutations::ServiceDomainCreate, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        vars,
    )
    .await?
    .service_domain_create;

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    let details = details_from_created_service(&domain);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&legacy_service_domain_output(&details))?
        );
    } else {
        print_domain_details(&details, "Service domain created", true);
    }

    Ok(())
}

fn print_existing_domains(domains: &DomainsDomains, json: bool) -> Result<()> {
    let items = domain_items(domains);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&legacy_domain_list_output(&items))?
        );
        return Ok(());
    }

    println!("Domains already exist on the service:");

    if items.len() == 1 {
        print_domain_details(&items[0], "Existing domain", false);
        return Ok(());
    }

    print_domain_table(&items);

    Ok(())
}

pub fn creating_domain_spiner(message: Option<String>) -> anyhow::Result<indicatif::ProgressBar> {
    let spinner = indicatif::ProgressBar::new_spinner()
        .with_style(
            indicatif::ProgressStyle::default_spinner()
                .tick_chars(TICK_STRING)
                .template("{spinner:.green} {msg}")?,
        )
        .with_message(message.unwrap_or_else(|| "Creating domain...".to_string()));
    spinner.enable_steady_tick(Duration::from_millis(100));

    Ok(spinner)
}

async fn create_custom_domain(
    domain: String,
    port: Option<u16>,
    project: Option<String>,
    service_name: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service_name, environment).await?;

    let spinner = (std::io::stdout().is_terminal() && !json)
        .then(|| {
            creating_domain_spiner(Some(format!(
                "Creating custom domain for service {}{}...",
                ctx.service_name,
                port.map(|p| format!(" on port {p}")).unwrap_or_default()
            )))
        })
        .and_then(|s| s.ok());

    let vars = mutations::custom_domain_create::Variables {
        input: mutations::custom_domain_create::CustomDomainCreateInput {
            domain: domain.clone(),
            environment_id: ctx.environment_id.clone(),
            project_id: ctx.project_id.clone(),
            service_id: ctx.service_id.clone(),
            target_port: port.map(|p| p as i64),
        },
    };

    let response = post_graphql::<mutations::CustomDomainCreate, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        vars,
    )
    .await?;

    if let Some(s) = spinner {
        s.finish_and_clear()
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    let details = details_from_created_custom(&response.custom_domain_create);
    print_domain_details(&details, "Custom domain created", true);

    Ok(())
}

async fn fetch_domains(ctx: &ServiceContext) -> Result<DomainsDomains> {
    let vars = queries::domains::Variables {
        project_id: ctx.project_id.clone(),
        environment_id: ctx.environment_id.clone(),
        service_id: ctx.service_id.clone(),
    };

    Ok(
        post_graphql::<queries::Domains, _>(&ctx.client, ctx.configs.get_backboard(), vars)
            .await?
            .domains,
    )
}

async fn resolve_domain(ctx: &ServiceContext, identifier: &str) -> Result<DomainDetails> {
    let domains = fetch_domains(ctx).await?;
    let items = domain_items(&domains);

    find_domain_details(&items, identifier)
        .cloned()
        .ok_or_else(|| anyhow!("Domain '{}' not found on the selected service", identifier))
}

fn domain_items(domains: &DomainsDomains) -> Vec<DomainDetails> {
    domains
        .service_domains
        .iter()
        .map(details_from_service)
        .chain(domains.custom_domains.iter().map(details_from_custom))
        .collect()
}

fn summaries(items: &[DomainDetails]) -> Vec<DomainSummary> {
    items.iter().map(|domain| domain.summary.clone()).collect()
}

fn find_domain_details<'a>(
    domains: &'a [DomainDetails],
    identifier: &str,
) -> Option<&'a DomainDetails> {
    let normalized = normalize_domain_identifier(identifier);

    domains.iter().find(|domain| {
        domain.summary.id.eq_ignore_ascii_case(identifier)
            || domain.summary.domain.eq_ignore_ascii_case(&normalized)
    })
}

fn normalize_domain_identifier(identifier: &str) -> String {
    let trimmed = identifier.trim();
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);

    without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .trim_end_matches('.')
        .to_string()
}

fn details_from_service(domain: &queries::domains::DomainsDomainsServiceDomains) -> DomainDetails {
    DomainDetails {
        summary: DomainSummary {
            id: domain.id.clone(),
            domain: domain.domain.clone(),
            kind: DomainKind::Service,
            target_port: domain.target_port,
            sync_status: enum_name(&domain.sync_status),
            created_at: domain.created_at.as_ref().map(chrono::DateTime::to_rfc3339),
            updated_at: domain.updated_at.as_ref().map(chrono::DateTime::to_rfc3339),
            environment_id: domain.environment_id.clone(),
            service_id: domain.service_id.clone(),
            service_domain_suffix: domain.suffix.clone(),
        },
        dns_records: Vec::new(),
        verification: None,
        certificate: None,
        certificates: Vec::new(),
    }
}

fn details_from_created_service(
    domain: &mutations::service_domain_create::ServiceDomainCreateServiceDomainCreate,
) -> DomainDetails {
    DomainDetails {
        summary: DomainSummary {
            id: domain.id.clone(),
            domain: domain.domain.clone(),
            kind: DomainKind::Service,
            target_port: domain.target_port,
            sync_status: enum_name(&domain.sync_status),
            created_at: domain.created_at.as_ref().map(chrono::DateTime::to_rfc3339),
            updated_at: domain.updated_at.as_ref().map(chrono::DateTime::to_rfc3339),
            environment_id: domain.environment_id.clone(),
            service_id: domain.service_id.clone(),
            service_domain_suffix: domain.suffix.clone(),
        },
        dns_records: Vec::new(),
        verification: None,
        certificate: None,
        certificates: Vec::new(),
    }
}

fn details_from_custom(domain: &queries::domains::DomainsDomainsCustomDomains) -> DomainDetails {
    DomainDetails {
        summary: DomainSummary {
            id: domain.id.clone(),
            domain: domain.domain.clone(),
            kind: DomainKind::Custom,
            target_port: domain.target_port,
            sync_status: enum_name(&domain.sync_status),
            created_at: domain.created_at.as_ref().map(chrono::DateTime::to_rfc3339),
            updated_at: domain.updated_at.as_ref().map(chrono::DateTime::to_rfc3339),
            environment_id: domain.environment_id.clone(),
            service_id: domain.service_id.clone(),
            service_domain_suffix: None,
        },
        dns_records: domain
            .status
            .dns_records
            .iter()
            .map(dns_from_query)
            .collect(),
        verification: Some(VerificationOutput {
            verified: domain.status.verified,
            dns_host: domain.status.verification_dns_host.clone(),
            token: domain.status.verification_token.clone(),
        }),
        certificate: Some(CertificateStatusOutput {
            status: enum_name(&domain.status.certificate_status),
            detailed_status: enum_name_option(&domain.status.certificate_status_detailed),
            error_message: domain.status.certificate_error_message.clone(),
            error_type: enum_name_option(&domain.status.certificate_error_type),
            retryable: domain.status.certificate_retryable,
            cdn_provider: enum_name_option(&domain.status.cdn_provider),
        }),
        certificates: domain
            .status
            .certificates
            .as_ref()
            .map(|certificates| certificates.iter().map(certificate_from_query).collect())
            .unwrap_or_default(),
    }
}

fn details_from_created_custom(
    domain: &mutations::custom_domain_create::CustomDomainCreateCustomDomainCreate,
) -> DomainDetails {
    DomainDetails {
        summary: DomainSummary {
            id: domain.id.clone(),
            domain: domain.domain.clone(),
            kind: DomainKind::Custom,
            target_port: domain.target_port,
            sync_status: enum_name(&domain.sync_status),
            created_at: domain.created_at.as_ref().map(chrono::DateTime::to_rfc3339),
            updated_at: domain.updated_at.as_ref().map(chrono::DateTime::to_rfc3339),
            environment_id: domain.environment_id.clone(),
            service_id: domain.service_id.clone(),
            service_domain_suffix: None,
        },
        dns_records: domain
            .status
            .dns_records
            .iter()
            .map(dns_from_create)
            .collect(),
        verification: Some(VerificationOutput {
            verified: domain.status.verified,
            dns_host: domain.status.verification_dns_host.clone(),
            token: domain.status.verification_token.clone(),
        }),
        certificate: Some(CertificateStatusOutput {
            status: enum_name(&domain.status.certificate_status),
            detailed_status: enum_name_option(&domain.status.certificate_status_detailed),
            error_message: domain.status.certificate_error_message.clone(),
            error_type: enum_name_option(&domain.status.certificate_error_type),
            retryable: domain.status.certificate_retryable,
            cdn_provider: enum_name_option(&domain.status.cdn_provider),
        }),
        certificates: domain
            .status
            .certificates
            .as_ref()
            .map(|certificates| certificates.iter().map(certificate_from_create).collect())
            .unwrap_or_default(),
    }
}

fn dns_from_query(
    record: &queries::domains::DomainsDomainsCustomDomainsStatusDnsRecords,
) -> DnsRecordOutput {
    DnsRecordOutput {
        record_type: enum_name(&record.record_type),
        name: if record.hostlabel.is_empty() {
            "@".to_string()
        } else {
            record.hostlabel.clone()
        },
        fqdn: record.fqdn.clone(),
        required_value: record.required_value.clone(),
        current_value: record.current_value.clone(),
        status: enum_name(&record.status),
        zone: record.zone.clone(),
        purpose: enum_name(&record.purpose),
    }
}

fn dns_from_create(
    record: &mutations::custom_domain_create::CustomDomainCreateCustomDomainCreateStatusDnsRecords,
) -> DnsRecordOutput {
    DnsRecordOutput {
        record_type: enum_name(&record.record_type),
        name: if record.hostlabel.is_empty() {
            "@".to_string()
        } else {
            record.hostlabel.clone()
        },
        fqdn: record.fqdn.clone(),
        required_value: record.required_value.clone(),
        current_value: record.current_value.clone(),
        status: enum_name(&record.status),
        zone: record.zone.clone(),
        purpose: enum_name(&record.purpose),
    }
}

fn certificate_from_query(
    certificate: &queries::domains::DomainsDomainsCustomDomainsStatusCertificates,
) -> CertificateOutput {
    CertificateOutput {
        issued_at: certificate
            .issued_at
            .as_ref()
            .map(chrono::DateTime::to_rfc3339),
        expires_at: certificate
            .expires_at
            .as_ref()
            .map(chrono::DateTime::to_rfc3339),
        domain_names: certificate.domain_names.clone(),
        fingerprint_sha256: certificate.fingerprint_sha256.clone(),
        key_type: enum_name(&certificate.key_type),
    }
}

fn certificate_from_create(
    certificate: &mutations::custom_domain_create::CustomDomainCreateCustomDomainCreateStatusCertificates,
) -> CertificateOutput {
    CertificateOutput {
        issued_at: certificate
            .issued_at
            .as_ref()
            .map(chrono::DateTime::to_rfc3339),
        expires_at: certificate
            .expires_at
            .as_ref()
            .map(chrono::DateTime::to_rfc3339),
        domain_names: certificate.domain_names.clone(),
        fingerprint_sha256: certificate.fingerprint_sha256.clone(),
        key_type: enum_name(&certificate.key_type),
    }
}

fn print_domain_table(domains: &[DomainDetails]) {
    let domain_width = domains
        .iter()
        .map(|domain| domain.summary.domain.len())
        .max()
        .unwrap_or("Domain".len())
        .max("Domain".len())
        + 3;
    let type_width = "service".len() + 3;
    let id_width = domains
        .iter()
        .map(|domain| domain.summary.id.len())
        .max()
        .unwrap_or("ID".len())
        .max("ID".len())
        + 3;
    let port_width = domains
        .iter()
        .map(|domain| format_target_port(domain.summary.target_port).len())
        .max()
        .unwrap_or("Port".len())
        .max("Port".len())
        + 3;

    println!(
        "{:<domain_width$}{:<type_width$}{:<id_width$}{:<port_width$}Sync",
        "Domain".bold(),
        "Type".bold(),
        "ID".bold(),
        "Port".bold(),
        domain_width = domain_width,
        type_width = type_width,
        id_width = id_width,
        port_width = port_width,
    );

    for domain in domains {
        println!(
            "{:<domain_width$}{:<type_width$}{:<id_width$}{:<port_width$}{}",
            domain.summary.domain,
            domain.summary.kind,
            domain.summary.id,
            format_target_port(domain.summary.target_port),
            domain.summary.sync_status,
            domain_width = domain_width,
            type_width = type_width,
            id_width = id_width,
            port_width = port_width,
        );
    }
}

fn print_domain_details(domain: &DomainDetails, title: &str, show_next_step: bool) {
    println!("{}:", title.bold());
    println!(
        "  URL: {}",
        format!("https://{}", domain.summary.domain)
            .magenta()
            .bold()
    );
    println!("  ID: {}", domain.summary.id);
    println!("  Type: {}", domain.summary.kind);
    println!(
        "  Target port: {}",
        format_target_port(domain.summary.target_port)
    );
    println!("  Sync status: {}", domain.summary.sync_status);

    if let Some(created_at) = &domain.summary.created_at {
        println!("  Created: {}", created_at);
    }
    if let Some(updated_at) = &domain.summary.updated_at {
        println!("  Updated: {}", updated_at);
    }

    if let Some(verification) = &domain.verification {
        println!(
            "  Verified: {}",
            if verification.verified { "yes" } else { "no" }
        );
    }

    if let Some(certificate) = &domain.certificate {
        println!("  Certificate status: {}", certificate.status);
        if let Some(detailed_status) = &certificate.detailed_status {
            println!("  Certificate detail: {}", detailed_status);
        }
        if let Some(error_message) = &certificate.error_message {
            println!("  Certificate error: {}", error_message);
        }
        if let Some(error_type) = &certificate.error_type {
            println!("  Certificate error type: {}", error_type);
        }
        if let Some(retryable) = certificate.retryable {
            println!("  Certificate retryable: {}", retryable);
        }
    }

    if !domain.dns_records.is_empty() {
        println!("\nDNS records:");
        print_dns(&domain.dns_records, domain.verification.as_ref());
        println!(
            "\nNote: if the Name is \"@\", the DNS record should be created for the root of the domain."
        );
        println!("DNS changes can take up to 72 hours to propagate worldwide.");
    }

    if show_next_step {
        println!(
            "\nNext: {}",
            format!("railway domain status {}", domain.summary.id).bold()
        );
    }
}

fn print_dns(domains: &[DnsRecordOutput], verification: Option<&VerificationOutput>) {
    let zone = domains.first().map(|record| record.zone.as_str());

    let txt_verification = verification.and_then(|verification| {
        if verification.verified {
            return None;
        }

        match (&verification.dns_host, &verification.token) {
            (Some(host), Some(token)) => {
                let host_label = zone
                    .and_then(|zone| host.strip_suffix(&format!(".{}", zone)))
                    .unwrap_or(host);
                Some((host_label.to_string(), verification_txt_value(token)))
            }
            _ => None,
        }
    });

    let (padding_type, padding_hostlabel, padding_value) = domains
        .iter()
        // Minimum length should be 8, but we add 3 for extra padding so 8-3 = 5
        .fold((5, 5, 5), |(max_type, max_hostlabel, max_value), d| {
            (
                max(max_type, d.record_type.len()),
                max(max_hostlabel, d.name.len()),
                max(max_value, d.required_value.len()),
            )
        });

    let (padding_type, padding_hostlabel, padding_value) =
        if let Some((host, value)) = &txt_verification {
            (
                max(padding_type, 3),
                max(padding_hostlabel, host.len()),
                max(padding_value, value.len()),
            )
        } else {
            (padding_type, padding_hostlabel, padding_value)
        };

    let [padding_type, padding_hostlabel, padding_value] =
        [padding_type + 3, padding_hostlabel + 3, padding_value + 3];

    println!(
        "\t{:<width_type$}{:<width_host$}{:<width_value$}",
        "Type",
        "Name",
        "Value",
        width_type = padding_type,
        width_host = padding_hostlabel,
        width_value = padding_value
    );

    for domain in domains {
        println!(
            "\t{:<width_type$}{:<width_host$}{:<width_value$}",
            domain.record_type,
            domain.name,
            domain.required_value,
            width_type = padding_type,
            width_host = padding_hostlabel,
            width_value = padding_value
        );
    }

    if let Some((host, value)) = txt_verification {
        println!(
            "\t{:<width_type$}{:<width_host$}{:<width_value$}",
            "TXT",
            host,
            value,
            width_type = padding_type,
            width_host = padding_hostlabel,
            width_value = padding_value
        );
    }
}

fn format_target_port(port: Option<i64>) -> String {
    port.map(|port| port.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn domain_url(domain: &str) -> String {
    format!("https://{domain}")
}

fn legacy_service_domain_output(domain: &DomainDetails) -> serde_json::Value {
    json!({
        "domain": domain_url(&domain.summary.domain),
    })
}

fn legacy_domain_list_output(domains: &[DomainDetails]) -> serde_json::Value {
    json!({
        "domains": domains
            .iter()
            .map(|domain| domain_url(&domain.summary.domain))
            .collect::<Vec<_>>()
    })
}

fn service_domain_input(domain: &DomainDetails, new_domain: &str) -> Result<String> {
    let normalized = normalize_domain_identifier(new_domain);

    if normalized.is_empty() {
        bail!("--domain must not be empty.");
    }

    if normalized.contains('.') {
        return Ok(normalized);
    }

    let Some(suffix) = &domain.summary.service_domain_suffix else {
        bail!("Pass the full service domain because the current suffix could not be resolved.");
    };

    Ok(format!("{normalized}.{suffix}"))
}

fn update_change_summary(port: Option<u16>, new_domain: Option<&str>) -> String {
    let mut changes = Vec::new();
    if let Some(port) = port {
        changes.push(format!("target port to {port}"));
    }
    if let Some(new_domain) = new_domain {
        changes.push(format!(
            "domain to {}",
            normalize_domain_identifier(new_domain)
        ));
    }

    changes.join(" and ")
}

fn enum_name<T: fmt::Debug>(value: &T) -> String {
    format!("{value:?}")
}

fn enum_name_option<T: fmt::Debug>(value: &Option<T>) -> Option<String> {
    value.as_ref().map(enum_name)
}

const CERTIFICATE_STATUS_ISSUE_FAILED: &str = "CERTIFICATE_STATUS_TYPE_ISSUE_FAILED";

fn certificate_retry_unavailable_reason(domain: &DomainDetails) -> Option<String> {
    let Some(certificate) = &domain.certificate else {
        return Some(
            "Certificate retry is only available after certificate issuance fails. Current status is unknown."
                .to_string(),
        );
    };

    if certificate.status != CERTIFICATE_STATUS_ISSUE_FAILED {
        return Some(format!(
            "Certificate retry is only available after certificate issuance fails. Current status: {}.",
            certificate.status
        ));
    }

    if certificate.retryable == Some(false) {
        return Some(
            certificate
                .error_message
                .as_ref()
                .map(|message| format!("Certificate retry is not available for this failure. {message}"))
                .unwrap_or_else(|| {
                    "Certificate retry is not available for this failure. Check your DNS configuration or contact support."
                        .to_string()
                }),
        );
    }

    None
}

fn verification_txt_value(token: &str) -> String {
    let mut token = token;
    while let Some(stripped) = token.strip_prefix("railway-verify=") {
        token = stripped;
    }
    format!("railway-verify={token}")
}

fn confirm_delete<F>(yes: bool, is_terminal: bool, prompt: F) -> Result<bool>
where
    F: FnOnce() -> Result<bool>,
{
    if yes {
        Ok(true)
    } else if is_terminal {
        prompt()
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum DomainKind {
    Custom,
    Service,
}

impl fmt::Display for DomainKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            DomainKind::Custom => "custom",
            DomainKind::Service => "service",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DomainSummary {
    id: String,
    domain: String,
    #[serde(rename = "type")]
    kind: DomainKind,
    target_port: Option<i64>,
    sync_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    #[serde(skip_serializing)]
    environment_id: String,
    #[serde(skip_serializing)]
    service_id: String,
    #[serde(skip_serializing)]
    service_domain_suffix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DomainDetails {
    #[serde(flatten)]
    summary: DomainSummary,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    dns_records: Vec<DnsRecordOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verification: Option<VerificationOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    certificate: Option<CertificateStatusOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    certificates: Vec<CertificateOutput>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DnsRecordOutput {
    record_type: String,
    name: String,
    fqdn: String,
    required_value: String,
    current_value: String,
    status: String,
    zone: String,
    purpose: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct VerificationOutput {
    verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    dns_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CertificateStatusOutput {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detailed_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retryable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cdn_provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CertificateOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    issued_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
    domain_names: Vec<String>,
    fingerprint_sha256: String,
    key_type: String,
}

#[derive(Serialize)]
struct ListOutput {
    domains: Vec<DomainSummary>,
}

#[derive(Serialize)]
struct DomainOutput {
    domain: DomainDetails,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn sample_domain(id: &str, domain: &str) -> DomainDetails {
        DomainDetails {
            summary: DomainSummary {
                id: id.to_string(),
                domain: domain.to_string(),
                kind: DomainKind::Custom,
                target_port: Some(3000),
                sync_status: "ACTIVE".to_string(),
                created_at: None,
                updated_at: None,
                environment_id: "env_123".to_string(),
                service_id: "svc_123".to_string(),
                service_domain_suffix: None,
            },
            dns_records: Vec::new(),
            verification: None,
            certificate: None,
            certificates: Vec::new(),
        }
    }

    fn sample_service_domain(id: &str, domain: &str) -> DomainDetails {
        let mut domain = sample_domain(id, domain);
        domain.summary.kind = DomainKind::Service;
        domain.summary.service_domain_suffix = Some("up.railway.app".to_string());
        domain
    }

    fn sample_certificate(status: &str, retryable: Option<bool>) -> CertificateStatusOutput {
        CertificateStatusOutput {
            status: status.to_string(),
            detailed_status: None,
            error_message: Some("Certificate failed.".to_string()),
            error_type: None,
            retryable,
            cdn_provider: None,
        }
    }

    #[test]
    fn parses_legacy_create_and_subcommands() {
        let args = Args::parse_from(["domain", "example.com", "--port", "3000"]);
        assert!(args.command.is_none());
        assert_eq!(args.domain.as_deref(), Some("example.com"));
        assert_eq!(args.port, Some(3000));

        let args = Args::parse_from(["domain", "list", "--service", "api", "--json"]);
        assert!(matches!(args.command, Some(Commands::List)));
        assert_eq!(args.service.as_deref(), Some("api"));
        assert!(args.json);

        let args = Args::parse_from([
            "domain",
            "update",
            "example.com",
            "--port",
            "8080",
            "--domain",
            "api-new",
        ]);
        assert!(matches!(
            args.command,
            Some(Commands::Update {
                identifier,
                port: Some(8080),
                new_domain: Some(new_domain),
            }) if identifier == "example.com" && new_domain == "api-new"
        ));

        let args = Args::parse_from(["domain", "certificate", "retry", "example.com"]);
        assert!(matches!(
            args.command,
            Some(Commands::Certificate {
                command: CertificateCommands::Retry { domain },
            }) if domain == "example.com"
        ));
    }

    #[test]
    fn validates_port_range() {
        assert_eq!(parse_port("1").unwrap(), 1);
        assert_eq!(parse_port("65535").unwrap(), 65535);
        assert!(parse_port("0").is_err());
        assert!(parse_port("65536").is_err());
        assert!(Args::try_parse_from(["domain", "update", "example.com", "--port", "0"]).is_err());
    }

    #[test]
    fn domain_kind_display_respects_padding() {
        assert_eq!(format!("{:<10}", DomainKind::Service), "service   ");
        assert_eq!(format!("{:<9}", DomainKind::Custom), "custom   ");
    }

    #[test]
    fn verification_txt_value_has_one_prefix() {
        assert_eq!(verification_txt_value("abc123"), "railway-verify=abc123");
        assert_eq!(
            verification_txt_value("railway-verify=abc123"),
            "railway-verify=abc123"
        );
        assert_eq!(
            verification_txt_value("railway-verify=railway-verify=abc123"),
            "railway-verify=abc123"
        );
    }

    #[test]
    fn finds_domain_by_id_name_or_url() {
        let domains = vec![sample_domain("dom_123", "api.example.com")];

        assert!(find_domain_details(&domains, "dom_123").is_some());
        assert!(find_domain_details(&domains, "API.EXAMPLE.COM").is_some());
        assert!(find_domain_details(&domains, "https://api.example.com/").is_some());
        assert!(find_domain_details(&domains, "missing.example.com").is_none());
    }

    #[test]
    fn service_domain_input_accepts_full_domain_or_host_label() {
        let domain = sample_service_domain("dom_123", "api.up.railway.app");

        assert_eq!(
            service_domain_input(&domain, "web.up.railway.app").unwrap(),
            "web.up.railway.app"
        );
        assert_eq!(
            service_domain_input(&domain, "web").unwrap(),
            "web.up.railway.app"
        );
        assert!(service_domain_input(&domain, "").is_err());
    }

    #[test]
    fn certificate_retry_matches_dashboard_gate() {
        let mut domain = sample_domain("dom_123", "api.example.com");
        assert!(certificate_retry_unavailable_reason(&domain).is_some());

        domain.certificate = Some(sample_certificate(CERTIFICATE_STATUS_ISSUE_FAILED, None));
        assert_eq!(certificate_retry_unavailable_reason(&domain), None);

        domain.certificate = Some(sample_certificate(
            CERTIFICATE_STATUS_ISSUE_FAILED,
            Some(true),
        ));
        assert_eq!(certificate_retry_unavailable_reason(&domain), None);

        domain.certificate = Some(sample_certificate(
            CERTIFICATE_STATUS_ISSUE_FAILED,
            Some(false),
        ));
        assert!(certificate_retry_unavailable_reason(&domain).is_some());

        domain.certificate = Some(sample_certificate("CERTIFICATE_STATUS_TYPE_VALID", None));
        assert!(certificate_retry_unavailable_reason(&domain).is_some());
    }

    #[test]
    fn legacy_json_keeps_domain_string_contract() {
        let domain = sample_domain("dom_123", "api.example.com");

        assert_eq!(
            legacy_service_domain_output(&domain),
            serde_json::json!({ "domain": "https://api.example.com" })
        );
        assert_eq!(
            legacy_domain_list_output(&[domain]),
            serde_json::json!({ "domains": ["https://api.example.com"] })
        );
    }
}
