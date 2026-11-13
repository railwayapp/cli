use std::{
    collections::BTreeMap,
    env, fmt,
    io::stdout,
    panic,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::anyhow;
use clap::{Subcommand, ValueEnum};
use crossterm::{
    cursor::{Hide, Show},
    event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::{SinkExt, StreamExt};
use is_terminal::IsTerminal;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph},
};
use reqwest_websocket::{Message, RequestBuilderExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{sync::mpsc, time::Instant};

use crate::{
    consts,
    util::prompt::{fake_select, prompt_confirm_with_default, prompt_select},
    workspace::{Workspace, workspaces_with_client},
};

use super::*;

const DOMAIN_SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);
const DOMAIN_PICKER_FRAME_INTERVAL: Duration = Duration::from_millis(33);
const DOMAIN_PICKER_RESULT_PADDING: &str = "  ";

#[derive(Clone, Copy)]
enum TerminalTheme {
    Dark,
    Light,
}

/// Manage purchased Railway domains
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway domains list\n  railway domains search example --limit 10\n  railway domains check example.com --json\n  railway domains list --workspace team --status active\n  railway domains status example.com\n  railway domains auto-renew disable example.com\n  railway domains dns create example.com --type A --host @ --answer 1.2.3.4\n  railway domains nameservers set example.com ns1.example.com ns2.example.com --yes"
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Workspace name or ID
    #[clap(long, global = true)]
    workspace: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Search for purchasable domains
    Search(SearchArgs),

    /// Check domain availability
    Check(CheckArgs),

    /// List purchased domains
    #[clap(visible_alias = "ls")]
    List(ListArgs),

    /// Show purchased domain status
    Status(DomainIdentifierArgs),

    /// Manage auto-renew for a purchased domain
    #[clap(name = "auto-renew")]
    AutoRenew {
        #[clap(subcommand)]
        command: AutoRenewCommands,
    },

    /// Manage DNS records for a purchased domain
    Dns {
        #[clap(subcommand)]
        command: DnsCommands,
    },

    /// Manage nameserver delegation for a purchased domain
    Nameservers {
        #[clap(subcommand)]
        command: NameserverCommands,
    },
}

#[derive(Parser)]
struct SearchArgs {
    /// Search query
    query: Vec<String>,

    /// Maximum number of results to print
    #[clap(long, default_value_t = 20)]
    limit: usize,
}

#[derive(Parser)]
struct CheckArgs {
    /// Domains to check
    #[clap(required = true)]
    domains: Vec<String>,
}

#[derive(Parser)]
struct ListArgs {
    /// Domain status to include
    #[clap(long, value_enum, default_value_t = StatusFilter::All)]
    status: StatusFilter,
}

#[derive(Parser)]
struct DomainIdentifierArgs {
    /// Domain name or Railway domain ID
    #[clap(value_name = "DOMAIN_OR_ID")]
    domain: String,
}

#[derive(Subcommand)]
enum AutoRenewCommands {
    /// Show auto-renew status
    Status(DomainIdentifierArgs),

    /// Enable auto-renew
    Enable(DomainIdentifierArgs),

    /// Disable auto-renew
    Disable(DomainIdentifierArgs),
}

#[derive(Subcommand)]
enum DnsCommands {
    /// List DNS records
    List(DnsDomainArgs),

    /// Create a DNS record
    Create(DnsWriteArgs),

    /// Update a DNS record
    Update(DnsUpdateArgs),

    /// Delete a DNS record
    Delete(DnsDeleteArgs),
}

#[derive(Parser)]
struct DnsDomainArgs {
    /// Domain name
    domain: String,
}

#[derive(Parser)]
struct DnsWriteArgs {
    /// Domain name
    domain: String,

    /// DNS record type
    #[clap(long = "type", value_enum)]
    record_type: DnsRecordType,

    /// DNS host/name, such as @ or www
    #[clap(long)]
    host: String,

    /// DNS answer/value
    #[clap(long)]
    answer: String,

    /// TTL in seconds
    #[clap(long)]
    ttl: Option<i64>,

    /// Priority for MX and SRV records
    #[clap(long)]
    priority: Option<i64>,
}

#[derive(Parser)]
struct DnsUpdateArgs {
    /// Domain name
    domain: String,

    /// DNS record ID
    record_id: i64,

    /// DNS record type
    #[clap(long = "type", value_enum)]
    record_type: DnsRecordType,

    /// DNS host/name, such as @ or www
    #[clap(long)]
    host: String,

    /// DNS answer/value
    #[clap(long)]
    answer: String,

    /// TTL in seconds
    #[clap(long)]
    ttl: Option<i64>,

    /// Priority for MX and SRV records
    #[clap(long)]
    priority: Option<i64>,
}

#[derive(Parser)]
struct DnsDeleteArgs {
    /// Domain name
    domain: String,

    /// DNS record ID
    record_id: i64,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    yes: bool,
}

#[derive(Subcommand)]
enum NameserverCommands {
    /// List authoritative nameservers
    List(DomainIdentifierArgs),

    /// Set custom authoritative nameservers
    Set(NameserverSetArgs),

    /// Reset to Railway-managed nameservers
    Reset(NameserverResetArgs),
}

#[derive(Parser)]
struct NameserverSetArgs {
    /// Domain name or Railway domain ID
    #[clap(value_name = "DOMAIN_OR_ID")]
    domain: String,

    /// Nameservers to delegate to
    #[clap(required = true)]
    nameservers: Vec<String>,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    yes: bool,
}

#[derive(Parser)]
struct NameserverResetArgs {
    /// Domain name or Railway domain ID
    #[clap(value_name = "DOMAIN_OR_ID")]
    domain: String,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    yes: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum StatusFilter {
    #[value(name = "active")]
    Active,
    #[value(name = "purchasing")]
    Purchasing,
    #[value(name = "expired")]
    Expired,
    #[value(name = "refunded")]
    Refunded,
    #[value(name = "all")]
    All,
}

impl fmt::Display for StatusFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Active => "active",
            Self::Purchasing => "purchasing",
            Self::Expired => "expired",
            Self::Refunded => "refunded",
            Self::All => "all",
        })
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize, PartialEq, Eq)]
enum DnsRecordType {
    #[value(name = "A")]
    A,
    #[value(name = "AAAA")]
    Aaaa,
    #[value(name = "ANAME")]
    Aname,
    #[value(name = "CNAME")]
    Cname,
    #[value(name = "MX")]
    Mx,
    #[value(name = "NS")]
    Ns,
    #[value(name = "SRV")]
    Srv,
    #[value(name = "TXT")]
    Txt,
}

impl fmt::Display for DnsRecordType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::A => "A",
            Self::Aaaa => "AAAA",
            Self::Aname => "ANAME",
            Self::Cname => "CNAME",
            Self::Mx => "MX",
            Self::Ns => "NS",
            Self::Srv => "SRV",
            Self::Txt => "TXT",
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DomainOutput {
    id: String,
    domain: String,
    status: String,
    auto_renew_enabled: bool,
    purchase_price: i64,
    renewal_price: i64,
    registration_years: i64,
    next_billing_date: Option<String>,
    workspace_id: String,
    workspace_name: Option<String>,
    created_at: String,
    nameservers: NameserversOutput,
    connected_service_instances: Vec<ConnectedServiceOutput>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NameserversOutput {
    nameservers: Vec<String>,
    is_default: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectedServiceOutput {
    project_id: String,
    service_id: String,
    environment_id: String,
    service_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DnsRecordOutput {
    id: i64,
    domain_name: String,
    host: String,
    fqdn: String,
    #[serde(rename = "type")]
    record_type: String,
    answer: String,
    ttl: i64,
    priority: Option<i64>,
}

#[derive(Debug, Serialize)]
struct DomainListOutput {
    domains: Vec<DomainOutput>,
}

#[derive(Debug, Serialize)]
struct DomainStatusOutput {
    domain: DomainOutput,
}

#[derive(Debug, Serialize)]
struct DnsRecordsOutput {
    records: Vec<DnsRecordOutput>,
}

#[derive(Debug, Serialize)]
struct DnsRecordMutationOutput {
    record: DnsRecordOutput,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DnsDeleteOutput {
    deleted: bool,
    record_id: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AutoRenewOutput {
    domain: String,
    auto_renew_enabled: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NameserverMutationOutput {
    domain: String,
    nameservers: Vec<String>,
    is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
struct DomainSearchResult {
    #[serde(default)]
    domain: String,
    zone: Option<String>,
    purchasable: Option<bool>,
    purchase_price: Option<f64>,
    renewal_price: Option<f64>,
    allowed_years: Option<Vec<i64>>,
    unavailable_reason: Option<String>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    purchase_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct SearchOutput {
    domains: Vec<DomainSearchResult>,
}

#[derive(Clone)]
struct DomainSearchRequest {
    query: String,
    limit: usize,
}

struct DomainPickerApp {
    request: DomainSearchRequest,
    results: Vec<DomainSearchResult>,
    selected: usize,
    theme: TerminalTheme,
    loading: bool,
    error: Option<String>,
    next_search_at: Option<Instant>,
    next_request_id: u64,
    active_request_id: u64,
}

struct DomainPickerMessage {
    request_id: u64,
    result: Result<Vec<DomainSearchResult>, String>,
}

struct CommandContext {
    configs: Configs,
    client: reqwest::Client,
    workspace: Option<Workspace>,
}

impl CommandContext {
    fn workspace(&self) -> Result<&Workspace> {
        self.workspace
            .as_ref()
            .ok_or_else(|| anyhow!("--workspace is required when referencing a domain name"))
    }
}

impl Commands {
    fn requires_workspace(&self, workspace_requested: bool) -> bool {
        if workspace_requested {
            return true;
        }

        match self {
            Self::List(_) | Self::Dns { .. } => true,
            Self::Status(args) => identifier_needs_workspace(&args.domain),
            Self::AutoRenew { command } => command.requires_workspace(),
            Self::Nameservers { command } => command.requires_workspace(),
            Self::Search(_) | Self::Check(_) => false,
        }
    }
}

impl AutoRenewCommands {
    fn requires_workspace(&self) -> bool {
        match self {
            Self::Status(args) | Self::Enable(args) | Self::Disable(args) => {
                identifier_needs_workspace(&args.domain)
            }
        }
    }
}

impl NameserverCommands {
    fn requires_workspace(&self) -> bool {
        match self {
            Self::List(args) => identifier_needs_workspace(&args.domain),
            Self::Set(args) => identifier_needs_workspace(&args.domain),
            Self::Reset(args) => identifier_needs_workspace(&args.domain),
        }
    }
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Commands::Search(search_args) => {
            search_domains(search_args, args.json).await?;
        }
        Commands::Check(check_args) => {
            check_domains(check_args, args.json).await?;
        }
        command => {
            let needs_workspace = command.requires_workspace(args.workspace.is_some());
            let context = resolve_context(args.workspace, needs_workspace, args.json).await?;
            match command {
                Commands::List(list_args) => list_domains(&context, list_args, args.json).await?,
                Commands::Status(status_args) => {
                    show_domain_status(&context, status_args.domain, args.json).await?
                }
                Commands::AutoRenew { command } => auto_renew(&context, command, args.json).await?,
                Commands::Dns { command } => dns(&context, command, args.json).await?,
                Commands::Nameservers { command } => {
                    nameservers(&context, command, args.json).await?
                }
                Commands::Search(_) | Commands::Check(_) => unreachable!(),
            }
        }
    }
    Ok(())
}

async fn resolve_context(
    requested_workspace: Option<String>,
    needs_workspace: bool,
    json: bool,
) -> Result<CommandContext> {
    let configs = Configs::new()?;
    let client = GQLClient::new_user_authorized(&configs)?;
    let workspace = if needs_workspace {
        Some(resolve_workspace(
            workspaces_with_client(&client, &configs).await?,
            requested_workspace,
            json,
        )?)
    } else {
        None
    };

    Ok(CommandContext {
        configs,
        client,
        workspace,
    })
}

fn resolve_workspace(
    workspaces: Vec<Workspace>,
    requested: Option<String>,
    json: bool,
) -> Result<Workspace> {
    use crate::errors::RailwayError;

    let select = |workspace: &Workspace| {
        if !json {
            fake_select("Select a workspace", workspace.name());
        }
        workspace.clone()
    };

    if let Some(input) = requested {
        return workspaces
            .iter()
            .find(|workspace| {
                workspace.id().eq_ignore_ascii_case(&input)
                    || workspace.name().eq_ignore_ascii_case(&input)
            })
            .map(select)
            .ok_or_else(|| RailwayError::WorkspaceNotFound(input).into());
    }

    if workspaces.len() == 1 {
        return Ok(select(&workspaces[0]));
    }

    if json || !std::io::stdout().is_terminal() {
        bail!("--workspace required in non-interactive mode (multiple workspaces available)");
    }

    prompt_select("Select a workspace", workspaces)
}

async fn list_domains(context: &CommandContext, args: ListArgs, json: bool) -> Result<()> {
    let workspace = context.workspace()?;
    let response = post_graphql_skip_none::<queries::RailwayDomains, _>(
        &context.client,
        context.configs.get_backboard(),
        queries::railway_domains::Variables {
            workspace_id: workspace.id().to_string(),
            status: args.status.to_gql(),
        },
    )
    .await?;

    let domains = response
        .railway_domains
        .iter()
        .map(domain_from_value)
        .collect::<Result<Vec<_>>>()?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DomainListOutput { domains })?
        );
        return Ok(());
    }

    if domains.is_empty() {
        println!(
            "No purchased domains found in workspace {}.",
            workspace.name().bold()
        );
        return Ok(());
    }

    print_domain_table(&domains);
    Ok(())
}

async fn show_domain_status(
    context: &CommandContext,
    identifier: String,
    json: bool,
) -> Result<()> {
    let domain = resolve_domain(context, &identifier).await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DomainStatusOutput { domain })?
        );
        return Ok(());
    }

    print_domain_details(&domain);
    Ok(())
}

async fn auto_renew(
    context: &CommandContext,
    command: AutoRenewCommands,
    json: bool,
) -> Result<()> {
    match command {
        AutoRenewCommands::Status(args) => {
            let domain = resolve_domain(context, &args.domain).await?;
            print_auto_renew(&domain, json)
        }
        AutoRenewCommands::Enable(args) => set_auto_renew(context, args.domain, true, json).await,
        AutoRenewCommands::Disable(args) => set_auto_renew(context, args.domain, false, json).await,
    }
}

async fn set_auto_renew(
    context: &CommandContext,
    identifier: String,
    enabled: bool,
    json: bool,
) -> Result<()> {
    let domain = resolve_domain(context, &identifier).await?;

    let response = post_graphql_skip_none::<mutations::RailwayDomainUpdate, _>(
        &context.client,
        context.configs.get_backboard(),
        mutations::railway_domain_update::Variables {
            input: mutations::railway_domain_update::RailwayDomainUpdateInput {
                id: domain.id.clone(),
                auto_renew_enabled: Some(enabled),
            },
        },
    )
    .await?;

    let updated = domain_from_value(&response.railway_domain_update)?;
    print_auto_renew(&updated, json)
}

fn print_auto_renew(domain: &DomainOutput, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&AutoRenewOutput {
                domain: domain.domain.clone(),
                auto_renew_enabled: domain.auto_renew_enabled,
            })?
        );
        return Ok(());
    }

    let status = if domain.auto_renew_enabled {
        "enabled".green()
    } else {
        "disabled".yellow()
    };
    println!("Auto-renew for {} is {}.", domain.domain.bold(), status);
    Ok(())
}

async fn dns(context: &CommandContext, command: DnsCommands, json: bool) -> Result<()> {
    match command {
        DnsCommands::List(args) => list_dns_records(context, args.domain, json).await,
        DnsCommands::Create(args) => create_dns_record(context, args, json).await,
        DnsCommands::Update(args) => update_dns_record(context, args, json).await,
        DnsCommands::Delete(args) => delete_dns_record(context, args, json).await,
    }
}

async fn list_dns_records(context: &CommandContext, domain: String, json: bool) -> Result<()> {
    let domain = normalize_domain(&domain);
    let records = fetch_dns_records(context, &domain).await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DnsRecordsOutput { records })?
        );
        return Ok(());
    }

    if records.is_empty() {
        println!("No DNS records found for {}.", domain.bold());
        return Ok(());
    }

    print_dns_table(&records);
    Ok(())
}

async fn create_dns_record(context: &CommandContext, args: DnsWriteArgs, json: bool) -> Result<()> {
    let domain = normalize_domain(&args.domain);
    let response = post_graphql_skip_none::<mutations::RailwayDomainDnsRecordCreate, _>(
        &context.client,
        context.configs.get_backboard(),
        mutations::railway_domain_dns_record_create::Variables {
            input: mutations::railway_domain_dns_record_create::RailwayDomainDnsRecordCreateInput {
                answer: args.answer,
                domain,
                host: args.host,
                priority: args.priority,
                ttl: args.ttl,
                type_: args.record_type.to_create_gql(),
                workspace_id: context.workspace()?.id().to_string(),
            },
        },
    )
    .await?;

    let record = dns_record_from_value(&response.railway_domain_dns_record_create)?;
    print_dns_record_mutation("Created", record, json)
}

async fn update_dns_record(
    context: &CommandContext,
    args: DnsUpdateArgs,
    json: bool,
) -> Result<()> {
    let domain = normalize_domain(&args.domain);
    let response = post_graphql_skip_none::<mutations::RailwayDomainDnsRecordUpdate, _>(
        &context.client,
        context.configs.get_backboard(),
        mutations::railway_domain_dns_record_update::Variables {
            input: mutations::railway_domain_dns_record_update::RailwayDomainDnsRecordUpdateInput {
                answer: args.answer,
                domain,
                host: args.host,
                priority: args.priority,
                record_id: args.record_id,
                ttl: args.ttl,
                type_: args.record_type.to_update_gql(),
                workspace_id: context.workspace()?.id().to_string(),
            },
        },
    )
    .await?;

    let record = dns_record_from_value(&response.railway_domain_dns_record_update)?;
    print_dns_record_mutation("Updated", record, json)
}

async fn delete_dns_record(
    context: &CommandContext,
    args: DnsDeleteArgs,
    json: bool,
) -> Result<()> {
    let domain = normalize_domain(&args.domain);
    confirm_action(
        args.yes,
        &format!(
            "Delete DNS record {} for {}?",
            args.record_id.to_string().red(),
            domain.bold()
        ),
        "--yes is required to delete DNS records in non-interactive mode",
    )?;

    let response = post_graphql::<mutations::RailwayDomainDnsRecordDelete, _>(
        &context.client,
        context.configs.get_backboard(),
        mutations::railway_domain_dns_record_delete::Variables {
            input: mutations::railway_domain_dns_record_delete::RailwayDomainDnsRecordDeleteInput {
                domain,
                record_id: args.record_id,
                workspace_id: context.workspace()?.id().to_string(),
            },
        },
    )
    .await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DnsDeleteOutput {
                deleted: response.railway_domain_dns_record_delete,
                record_id: args.record_id,
            })?
        );
    } else if response.railway_domain_dns_record_delete {
        println!(
            "Deleted DNS record {}.",
            args.record_id.to_string().magenta()
        );
    } else {
        println!("DNS record {} was not deleted.", args.record_id);
    }

    Ok(())
}

async fn fetch_dns_records(context: &CommandContext, domain: &str) -> Result<Vec<DnsRecordOutput>> {
    let response = post_graphql::<queries::RailwayDomainDnsRecords, _>(
        &context.client,
        context.configs.get_backboard(),
        queries::railway_domain_dns_records::Variables {
            domain: domain.to_string(),
            workspace_id: context.workspace()?.id().to_string(),
        },
    )
    .await?;

    response
        .railway_domain_dns_records
        .iter()
        .map(dns_record_from_value)
        .collect()
}

fn print_dns_record_mutation(verb: &str, record: DnsRecordOutput, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DnsRecordMutationOutput { record })?
        );
    } else {
        println!(
            "{} DNS record {} {} -> {}.",
            verb,
            record.record_type.magenta(),
            record.fqdn.bold(),
            record.answer
        );
    }
    Ok(())
}

async fn nameservers(
    context: &CommandContext,
    command: NameserverCommands,
    json: bool,
) -> Result<()> {
    match command {
        NameserverCommands::List(args) => {
            let domain = resolve_domain(context, &args.domain).await?;
            print_nameservers(&domain, json)
        }
        NameserverCommands::Set(args) => {
            let nameservers = validate_nameservers(args.nameservers)?;
            let domain = resolve_domain(context, &args.domain).await?;
            confirm_action(
                args.yes,
                &format!(
                    "Set custom nameservers for {}? DNS will be delegated away from Railway-managed defaults.",
                    domain.domain.red()
                ),
                "--yes is required to set nameservers in non-interactive mode",
            )?;
            set_nameservers(context, domain, nameservers, json).await
        }
        NameserverCommands::Reset(args) => {
            let domain = resolve_domain(context, &args.domain).await?;
            confirm_action(
                args.yes,
                &format!(
                    "Reset nameservers for {} to Railway-managed defaults?",
                    domain.domain.red()
                ),
                "--yes is required to reset nameservers in non-interactive mode",
            )?;
            set_nameservers(context, domain, Vec::new(), json).await
        }
    }
}

async fn set_nameservers(
    context: &CommandContext,
    domain: DomainOutput,
    nameservers: Vec<String>,
    json: bool,
) -> Result<()> {
    let response = post_graphql::<mutations::RailwayDomainNameserversSet, _>(
        &context.client,
        context.configs.get_backboard(),
        mutations::railway_domain_nameservers_set::Variables {
            input: mutations::railway_domain_nameservers_set::RailwayDomainNameserversSetInput {
                id: domain.id.clone(),
                nameservers,
            },
        },
    )
    .await?;

    let nameservers = nameservers_from_value(&response.railway_domain_nameservers_set)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&NameserverMutationOutput {
                domain: domain.domain,
                nameservers: nameservers.nameservers,
                is_default: nameservers.is_default,
            })?
        );
    } else {
        print_nameserver_values(&domain.domain, &nameservers);
    }
    Ok(())
}

fn print_nameservers(domain: &DomainOutput, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&NameserverMutationOutput {
                domain: domain.domain.clone(),
                nameservers: domain.nameservers.nameservers.clone(),
                is_default: domain.nameservers.is_default,
            })?
        );
    } else {
        print_nameserver_values(&domain.domain, &domain.nameservers);
    }
    Ok(())
}

fn print_nameserver_values(domain: &str, nameservers: &NameserversOutput) {
    let mode = if nameservers.is_default {
        "Railway-managed".green()
    } else {
        "custom".yellow()
    };
    println!("Nameservers for {} ({mode}):", domain.bold());
    for nameserver in &nameservers.nameservers {
        println!("  {nameserver}");
    }
}

async fn resolve_domain(context: &CommandContext, identifier: &str) -> Result<DomainOutput> {
    let identifier = normalize_domain(identifier);
    if looks_like_domain_name(&identifier) {
        let response = post_graphql::<queries::RailwayDomainByName, _>(
            &context.client,
            context.configs.get_backboard(),
            queries::railway_domain_by_name::Variables {
                domain: identifier,
                workspace_id: context.workspace()?.id().to_string(),
            },
        )
        .await?;
        domain_from_value(&response.railway_domain_by_name)
    } else {
        let response = post_graphql::<queries::RailwayDomain, _>(
            &context.client,
            context.configs.get_backboard(),
            queries::railway_domain::Variables { id: identifier },
        )
        .await?;
        domain_from_value(&response.railway_domain)
    }
}

async fn search_domains(args: SearchArgs, json: bool) -> Result<()> {
    if args.limit == 0 {
        bail!("--limit must be greater than 0");
    }

    let query = args.query.join(" ").trim().to_string();
    if std::io::stdout().is_terminal() && !json {
        if let Some(domain) = run_domain_picker(DomainSearchRequest {
            query,
            limit: args.limit,
        })
        .await?
        {
            print_selected_domain(domain)?;
        }
        return Ok(());
    }

    if query.is_empty() {
        bail!("Provide a search query");
    }

    let mut domains = run_domain_search(&query, args.limit).await?;
    domains.truncate(args.limit);
    print_search_results(domains, json)
}

async fn check_domains(args: CheckArgs, json: bool) -> Result<()> {
    let domains = args
        .domains
        .iter()
        .map(|domain| normalize_domain(domain))
        .collect::<Vec<_>>();
    let results = run_domain_check(&domains).await?;
    print_search_results(results, json)
}

async fn connect_domain_search() -> Result<reqwest_websocket::WebSocket> {
    let configs = Configs::new()?;
    let host = format!("backboard.{}", configs.get_host());
    let client = reqwest::Client::builder()
        .user_agent(consts::get_user_agent())
        .build()?;

    let mut request = client
        .get(format!("wss://{host}/domain-search"))
        .header("x-source", consts::get_user_agent())
        .timeout(Duration::from_secs(10));

    if let Some(token) = configs.get_railway_auth_token() {
        request = request.header("authorization", format!("Bearer {token}"));
    }

    let response = request.upgrade().send().await?;
    response.error_for_status_ref()?;
    Ok(response.into_websocket().await?)
}

async fn run_domain_search(query: &str, limit: usize) -> Result<Vec<DomainSearchResult>> {
    let mut socket = connect_domain_search().await?;
    let request_type = if is_phrase_like(query) {
        "suggest"
    } else {
        "search"
    };
    let payload = if request_type == "suggest" {
        json!({ "type": "suggest", "prompt": query })
    } else {
        json!({ "type": "search", "query": query })
    };
    socket.send(Message::Text(payload.to_string())).await?;

    let mut results = Vec::new();
    let mut check_sent = false;

    loop {
        let Some(message) = next_search_message(&mut socket).await? else {
            break;
        };

        match parse_search_message(&message)? {
            SearchMessage::Results { domains } => {
                results = domains;
                if !check_sent {
                    let domains = results
                        .iter()
                        .take(limit)
                        .map(|result| result.domain.clone())
                        .collect::<Vec<_>>();
                    if !domains.is_empty() {
                        socket
                            .send(Message::Text(
                                json!({
                                    "type": "check",
                                    "domains": domains,
                                    "query": query,
                                })
                                .to_string(),
                            ))
                            .await?;
                        check_sent = true;
                    }
                }
            }
            SearchMessage::Domains { domains } => {
                merge_domain_results(&mut results, domains);
                break;
            }
            SearchMessage::Error { message } => bail!("{message}"),
            SearchMessage::Ignore => {}
        }
    }

    if results.is_empty() {
        bail!("No domain search results returned");
    }

    Ok(results)
}

async fn run_domain_check(domains: &[String]) -> Result<Vec<DomainSearchResult>> {
    let mut socket = connect_domain_search().await?;
    socket
        .send(Message::Text(
            json!({
                "type": "check",
                "domains": domains,
                "query": domains.join(","),
            })
            .to_string(),
        ))
        .await?;

    loop {
        let Some(message) = next_search_message(&mut socket).await? else {
            break;
        };

        match parse_search_message(&message)? {
            SearchMessage::Domains { domains: results } => {
                let mut ordered = Vec::new();
                for domain in domains {
                    if let Some(result) = results.get(domain) {
                        ordered.push(result.clone());
                    } else {
                        ordered.push(DomainSearchResult {
                            domain: domain.clone(),
                            ..Default::default()
                        });
                    }
                }
                return Ok(ordered);
            }
            SearchMessage::Results { .. } | SearchMessage::Ignore => {}
            SearchMessage::Error { message } => bail!("{message}"),
        }
    }

    bail!("No domain availability results returned")
}

fn spawn_domain_picker_search(
    tx: mpsc::UnboundedSender<DomainPickerMessage>,
    request: DomainSearchRequest,
    request_id: u64,
) {
    tokio::spawn(async move {
        let result = fetch_domain_picker_results(&request)
            .await
            .map_err(|error| format!("{error:#}"));
        let _ = tx.send(DomainPickerMessage { request_id, result });
    });
}

async fn fetch_domain_picker_results(
    request: &DomainSearchRequest,
) -> Result<Vec<DomainSearchResult>> {
    if request.query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut results = run_domain_search(&request.query, request.limit).await?;
    results.truncate(request.limit);
    add_purchase_urls(&mut results)?;
    Ok(results)
}

async fn run_domain_picker(request: DomainSearchRequest) -> Result<Option<DomainSearchResult>> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_domain_terminal();
        original_hook(info);
    }));

    let (mut terminal, theme) = setup_domain_terminal()?;
    let _cleanup = scopeguard::guard((), |_| {
        restore_domain_terminal();
    });

    let (search_tx, mut search_rx) = mpsc::unbounded_channel();
    let initial_query = !request.query.trim().is_empty();
    let mut app = DomainPickerApp {
        request,
        results: Vec::new(),
        selected: 0,
        theme,
        loading: initial_query,
        error: None,
        next_search_at: None,
        next_request_id: 1,
        active_request_id: 1,
    };

    if initial_query {
        spawn_domain_picker_search(
            search_tx.clone(),
            app.request.clone(),
            app.active_request_id,
        );
    }

    let mut events = EventStream::new();
    let mut render_interval = tokio::time::interval(DOMAIN_PICKER_FRAME_INTERVAL);
    render_interval.tick().await;

    loop {
        tokio::select! {
            _ = render_interval.tick() => {
                terminal.draw(|frame| render_domain_picker(&app, frame))?;
            }
            Some(message) = search_rx.recv() => {
                if message.request_id == app.active_request_id {
                    app.loading = false;
                    match message.result {
                        Ok(results) => {
                            app.error = None;
                            app.results = results;
                            app.selected = app.selected.min(app.results.len().saturating_sub(1));
                        }
                        Err(error) => {
                            app.results.clear();
                            app.selected = 0;
                            app.error = Some(error);
                        }
                    }
                }
            }
            Some(Ok(event)) = events.next() => {
                if let Some(domain) = handle_domain_picker_event(event, &mut app) {
                    return Ok(domain);
                }
            }
            _ = wait_for_domain_debounce(app.next_search_at), if app.next_search_at.is_some() => {
                app.next_search_at = None;
                if app.request.query.trim().is_empty() {
                    app.loading = false;
                    app.results.clear();
                    app.error = None;
                    continue;
                }

                app.next_request_id += 1;
                app.active_request_id = app.next_request_id;
                app.loading = true;
                app.error = None;
                spawn_domain_picker_search(
                    search_tx.clone(),
                    app.request.clone(),
                    app.active_request_id,
                );
            }
            _ = tokio::signal::ctrl_c() => {
                return Ok(None);
            }
        }
    }
}

async fn wait_for_domain_debounce(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    }
}

fn handle_domain_picker_event(
    event: Event,
    app: &mut DomainPickerApp,
) -> Option<Option<DomainSearchResult>> {
    let Event::Key(key) = event else {
        return None;
    };
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return None;
    }

    match key.code {
        KeyCode::Esc => Some(None),
        KeyCode::Enter => app.results.get(app.selected).cloned().map(Some),
        KeyCode::Up => {
            app.selected = app.selected.saturating_sub(1);
            None
        }
        KeyCode::Down => {
            if !app.results.is_empty() {
                app.selected = (app.selected + 1).min(app.results.len() - 1);
            }
            None
        }
        KeyCode::PageUp => {
            app.selected = app.selected.saturating_sub(5);
            None
        }
        KeyCode::PageDown => {
            if !app.results.is_empty() {
                app.selected = (app.selected + 5).min(app.results.len() - 1);
            }
            None
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(None),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.request.query.clear();
            queue_domain_picker_search(app);
            None
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.request.query.push(ch);
            queue_domain_picker_search(app);
            None
        }
        KeyCode::Backspace => {
            app.request.query.pop();
            queue_domain_picker_search(app);
            None
        }
        KeyCode::Delete => {
            app.request.query.clear();
            queue_domain_picker_search(app);
            None
        }
        _ => None,
    }
}

fn queue_domain_picker_search(app: &mut DomainPickerApp) {
    app.selected = 0;
    app.error = None;
    app.loading = !app.request.query.trim().is_empty();
    if app.request.query.trim().is_empty() {
        app.results.clear();
        app.next_search_at = None;
    } else {
        app.next_search_at = Some(Instant::now() + DOMAIN_SEARCH_DEBOUNCE);
    }
}

fn setup_domain_terminal() -> Result<(Terminal<CrosstermBackend<std::io::Stdout>>, TerminalTheme)> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    let theme = detect_terminal_theme();
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok((terminal, theme))
}

fn restore_domain_terminal() {
    let _ = execute!(stdout(), LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}

fn render_domain_picker(app: &DomainPickerApp, frame: &mut Frame) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    if area.width < 54 || area.height < 12 {
        let warning = Paragraph::new("Terminal too small. Resize to search domains.")
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(warning, area);
        return;
    }

    let width = area.width.saturating_sub(8).min(104);
    let height = area.height.saturating_sub(2);
    let content = Rect {
        x: area.x + 4,
        y: area.y + 1,
        width,
        height,
    };
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(content);

    render_domain_search_input(app, frame, chunks[0]);
    render_domain_picker_list(app, frame, chunks[2]);
    render_domain_picker_hint(app, frame, chunks[3]);
}

fn render_domain_search_input(app: &DomainPickerApp, frame: &mut Frame, area: Rect) {
    let input = if app.request.query.is_empty() {
        Line::from(Span::styled(
            "Search domains...",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Line::from(Span::raw(app.request.query.clone()))
    };

    let input = Paragraph::new(input).block(
        Block::default()
            .borders(Borders::ALL)
            .padding(Padding::new(1, 1, 0, 0))
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(input, area);
}

fn render_domain_picker_list(app: &DomainPickerApp, frame: &mut Frame, area: Rect) {
    if let Some(error) = &app.error {
        let message = Paragraph::new(format!("Search failed: {error}"))
            .style(Style::default().fg(Color::Red));
        frame.render_widget(message, area);
        return;
    }

    if app.results.is_empty() {
        let paragraph = if app.loading {
            Paragraph::new(Line::from(vec![
                Span::styled("Searching domains ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    spinner_frame().to_string(),
                    Style::default().fg(Color::Green),
                ),
            ]))
        } else if app.request.query.trim().is_empty() {
            Paragraph::new("Type a domain or idea to search.")
                .style(Style::default().fg(Color::DarkGray))
        } else {
            Paragraph::new("No domains found.").style(Style::default().fg(Color::DarkGray))
        };
        frame.render_widget(paragraph, area);
        return;
    }

    let items = app
        .results
        .iter()
        .enumerate()
        .map(|(index, domain)| domain_list_item(domain, index == app.selected, app.theme))
        .collect::<Vec<_>>();
    let list = List::new(items);
    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_domain_picker_hint(app: &DomainPickerApp, frame: &mut Frame, area: Rect) {
    let result_count = if app.results.is_empty() {
        String::new()
    } else {
        format!("  {} results", app.results.len())
    };
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(
            "Enter select  Up/Down move  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(result_count, Style::default().fg(Color::DarkGray)),
    ]));
    frame.render_widget(footer, area);
}

fn spinner_frame() -> char {
    let frame = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| (duration.as_millis() / 100) as usize)
        .unwrap_or_default();
    let frame_count = consts::TICK_STRING.chars().count();

    if frame_count == 0 {
        return ' ';
    }

    consts::TICK_STRING
        .chars()
        .nth(frame % frame_count)
        .unwrap_or(' ')
}

async fn next_search_message(socket: &mut reqwest_websocket::WebSocket) -> Result<Option<String>> {
    let message = match tokio::time::timeout(Duration::from_secs(15), socket.next()).await {
        Ok(Some(message)) => message?,
        Ok(None) | Err(_) => return Ok(None),
    };

    match message {
        Message::Text(text) => Ok(Some(text)),
        Message::Close { .. } => Ok(None),
        Message::Binary(_) | Message::Ping(_) | Message::Pong(_) => Ok(Some(String::new())),
    }
}

enum SearchMessage {
    Results {
        domains: Vec<DomainSearchResult>,
    },
    Domains {
        domains: BTreeMap<String, DomainSearchResult>,
    },
    Error {
        message: String,
    },
    Ignore,
}

fn parse_search_message(message: &str) -> Result<SearchMessage> {
    if message.is_empty() {
        return Ok(SearchMessage::Ignore);
    }

    let value: Value = serde_json::from_str(message)?;
    match string_value(&value, &["type"]).as_deref() {
        Some("results") => {
            let domains = value
                .get("domains")
                .map(parse_result_array)
                .transpose()?
                .unwrap_or_default();
            Ok(SearchMessage::Results { domains })
        }
        Some("domains") => {
            let domains = value
                .get("domains")
                .map(parse_domain_map)
                .transpose()?
                .unwrap_or_default();
            Ok(SearchMessage::Domains { domains })
        }
        Some("error") => Ok(SearchMessage::Error {
            message: string_value(&value, &["message"])
                .unwrap_or_else(|| "Domain search failed".to_string()),
        }),
        _ => Ok(SearchMessage::Ignore),
    }
}

fn parse_result_array(value: &Value) -> Result<Vec<DomainSearchResult>> {
    let Some(items) = value.as_array() else {
        return Ok(Vec::new());
    };

    items
        .iter()
        .map(|item| parse_domain_result(item, None))
        .collect()
}

fn parse_domain_map(value: &Value) -> Result<BTreeMap<String, DomainSearchResult>> {
    let Some(items) = value.as_object() else {
        return Ok(BTreeMap::new());
    };

    let mut domains = BTreeMap::new();
    for (domain, details) in items {
        domains.insert(domain.clone(), parse_domain_result(details, Some(domain))?);
    }
    Ok(domains)
}

fn parse_domain_result(value: &Value, domain_hint: Option<&str>) -> Result<DomainSearchResult> {
    if let Some(domain) = value.as_str() {
        return Ok(DomainSearchResult {
            domain: domain.to_string(),
            ..Default::default()
        });
    }

    let Some(object) = value.as_object() else {
        return Err(anyhow!("Invalid domain search result"));
    };

    let mut result: DomainSearchResult = serde_json::from_value(Value::Object(object.clone()))?;
    if result.domain.is_empty() {
        if let Some(domain) = domain_hint {
            result.domain = domain.to_string();
        }
    }
    Ok(result)
}

fn merge_domain_results(
    results: &mut Vec<DomainSearchResult>,
    domains: BTreeMap<String, DomainSearchResult>,
) {
    if results.is_empty() {
        *results = domains.into_values().collect();
        return;
    }

    for result in results {
        if let Some(details) = domains.get(&result.domain) {
            *result = details.clone();
        }
    }
}

fn detect_terminal_theme() -> TerminalTheme {
    terminal_theme_from_colorfgbg()
        .or_else(query_terminal_background)
        .unwrap_or(TerminalTheme::Light)
}

fn terminal_theme_from_colorfgbg() -> Option<TerminalTheme> {
    let value = env::var("COLORFGBG").ok()?;
    let background = value.split(';').next_back()?.parse::<u8>().ok()?;

    if matches!(background, 7 | 9..=15) {
        Some(TerminalTheme::Light)
    } else {
        Some(TerminalTheme::Dark)
    }
}

#[cfg(unix)]
fn query_terminal_background() -> Option<TerminalTheme> {
    use nix::libc;
    use std::{
        io::{Read, Write},
        os::fd::AsRawFd,
        thread,
        time::Instant as StdInstant,
    };

    let mut output = stdout();
    output.write_all(b"\x1b]11;?\x1b\\").ok()?;
    output.flush().ok()?;

    let mut input = std::io::stdin();
    let fd = input.as_raw_fd();
    let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if original_flags < 0 {
        return None;
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } < 0 {
        return None;
    }

    let _restore_flags = scopeguard::guard((), |_| unsafe {
        libc::fcntl(fd, libc::F_SETFL, original_flags);
    });

    let started = StdInstant::now();
    let mut response = Vec::new();
    let mut buffer = [0_u8; 64];

    while started.elapsed() < Duration::from_millis(160) {
        match input.read(&mut buffer) {
            Ok(0) => thread::sleep(Duration::from_millis(2)),
            Ok(read) => {
                response.extend_from_slice(&buffer[..read]);
                if response.ends_with(b"\x07") || response.ends_with(b"\x1b\\") {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return None,
        }
    }

    let (red, green, blue) = parse_terminal_background_response(&response)?;
    if perceived_luminance(red, green, blue) > 160.0 {
        Some(TerminalTheme::Light)
    } else {
        Some(TerminalTheme::Dark)
    }
}

#[cfg(not(unix))]
fn query_terminal_background() -> Option<TerminalTheme> {
    None
}

fn parse_terminal_background_response(response: &[u8]) -> Option<(u8, u8, u8)> {
    let response = std::str::from_utf8(response).ok()?;
    let color_start = response
        .find("]11;rgba:")
        .map(|idx| idx + "]11;rgba:".len())
        .or_else(|| response.find("]11;rgb:").map(|idx| idx + "]11;rgb:".len()))?;
    let color = &response[color_start..];
    let color = color.split(['\x07', '\x1b']).next()?;
    let mut components = color.split('/');

    Some((
        parse_terminal_color_component(components.next()?)?,
        parse_terminal_color_component(components.next()?)?,
        parse_terminal_color_component(components.next()?)?,
    ))
}

fn parse_terminal_color_component(component: &str) -> Option<u8> {
    let digits: String = component
        .chars()
        .take_while(|ch| ch.is_ascii_hexdigit())
        .take(4)
        .collect();
    if digits.is_empty() {
        return None;
    }

    let value = u32::from_str_radix(&digits, 16).ok()?;
    let max = (1_u32 << (digits.len() * 4)) - 1;
    Some(((value * 255 + max / 2) / max) as u8)
}

fn perceived_luminance(red: u8, green: u8, blue: u8) -> f64 {
    (red as f64 * 0.299) + (green as f64 * 0.587) + (blue as f64 * 0.114)
}

fn domain_list_item(
    domain: &DomainSearchResult,
    selected: bool,
    theme: TerminalTheme,
) -> ListItem<'static> {
    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw(DOMAIN_PICKER_RESULT_PADDING),
            Span::styled(domain.domain.clone(), domain_name_style(selected, theme)),
        ]),
        Line::from(domain_metadata_spans(domain, selected, theme)),
        Line::raw(""),
    ];

    if selected {
        ListItem::new(lines).style(Style::default().bg(selected_background(theme)))
    } else {
        ListItem::new(lines)
    }
}

fn domain_metadata_spans(
    domain: &DomainSearchResult,
    selected: bool,
    theme: TerminalTheme,
) -> Vec<Span<'static>> {
    let muted = muted_style(selected, theme);
    let price_style = if domain.purchasable == Some(true) {
        Style::default().fg(Color::Green)
    } else {
        muted
    };

    vec![
        Span::raw(DOMAIN_PICKER_RESULT_PADDING),
        Span::styled(
            availability_label(domain.purchasable).to_string(),
            availability_style(domain.purchasable),
        ),
        Span::styled(" • purchase ", muted),
        Span::styled(domain_price_label(domain.purchase_price), price_style),
        Span::styled(" • renewal ", muted),
        Span::styled(domain_price_label(domain.renewal_price), price_style),
    ]
}

fn selected_background(theme: TerminalTheme) -> Color {
    match theme {
        TerminalTheme::Dark => Color::Indexed(236),
        TerminalTheme::Light => Color::Indexed(255),
    }
}

fn domain_name_style(selected: bool, theme: TerminalTheme) -> Style {
    let style = Style::default().add_modifier(Modifier::BOLD);
    if !selected {
        return style;
    }

    match theme {
        TerminalTheme::Dark => style.fg(Color::White),
        TerminalTheme::Light => style.fg(Color::Black),
    }
}

fn muted_style(selected: bool, theme: TerminalTheme) -> Style {
    if !selected {
        return Style::default().fg(Color::DarkGray);
    }

    match theme {
        TerminalTheme::Dark => Style::default().fg(Color::Gray),
        TerminalTheme::Light => Style::default().fg(Color::DarkGray),
    }
}

fn availability_style(value: Option<bool>) -> Style {
    match value {
        Some(true) => Style::default().fg(Color::Green),
        Some(false) => Style::default().fg(Color::Red),
        None => Style::default().fg(Color::Yellow),
    }
}

fn availability_label(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "available",
        Some(false) => "taken",
        None => "unknown",
    }
}

fn domain_price_label(price: Option<f64>) -> String {
    price
        .map(format_domain_search_price)
        .unwrap_or_else(|| "-".to_string())
}

fn print_selected_domain(mut domain: DomainSearchResult) -> Result<()> {
    add_purchase_urls(std::slice::from_mut(&mut domain))?;
    println!("{}", domain.domain.bold());
    println!(
        "Availability: {}",
        match domain.purchasable {
            Some(true) => "available".green(),
            Some(false) => "taken".red(),
            None => "unknown".yellow(),
        }
    );
    println!(
        "Purchase: {}",
        format_selected_price(domain.purchase_price, domain.purchasable == Some(true))
    );
    println!(
        "Renewal: {}",
        format_selected_price(domain.renewal_price, domain.purchasable == Some(true))
    );

    if let Some(url) = domain.purchase_url {
        println!();
        match ::open::that(&url) {
            Ok(_) => println!("Opening purchase page in your browser:"),
            Err(_) => println!("Couldn't open a browser. Purchase here:"),
        }
        println!("  {}", url.bold().underline());
    }
    Ok(())
}

fn format_selected_price(price: Option<f64>, available: bool) -> String {
    let label = domain_price_label(price);
    if available && price.is_some() {
        label.green().to_string()
    } else {
        label
    }
}

fn print_search_results(mut domains: Vec<DomainSearchResult>, json: bool) -> Result<()> {
    add_purchase_urls(&mut domains)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&SearchOutput { domains })?
        );
        return Ok(());
    }

    if domains.is_empty() {
        println!("No domains found.");
        return Ok(());
    }

    let rows = domains
        .iter()
        .map(|domain| {
            vec![
                domain.domain.clone(),
                format_availability(domain.purchasable),
                domain
                    .purchase_price
                    .map(format_domain_search_price)
                    .unwrap_or_else(|| "-".to_string()),
                domain
                    .renewal_price
                    .map(format_domain_search_price)
                    .unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect::<Vec<_>>();
    print_table(&["Domain", "Available", "Purchase", "Renewal"], rows);

    let purchase_links = domains
        .iter()
        .filter_map(|domain| {
            domain
                .purchase_url
                .as_ref()
                .map(|url| (domain.domain.as_str(), url.as_str()))
        })
        .collect::<Vec<_>>();
    if !purchase_links.is_empty() {
        println!();
        println!("Purchase links:");
        for (domain, url) in purchase_links {
            println!("  {domain}: {url}");
        }
    }

    Ok(())
}

fn print_domain_table(domains: &[DomainOutput]) {
    let rows = domains
        .iter()
        .map(|domain| {
            vec![
                domain.domain.clone(),
                domain.status.to_lowercase(),
                format_bool(domain.auto_renew_enabled),
                domain
                    .next_billing_date
                    .clone()
                    .unwrap_or_else(|| "-".into()),
                if domain.nameservers.is_default {
                    "Railway".to_string()
                } else {
                    "Custom".to_string()
                },
            ]
        })
        .collect::<Vec<_>>();
    print_table(
        &[
            "Domain",
            "Status",
            "Auto-renew",
            "Next billing",
            "Nameservers",
        ],
        rows,
    );
}

fn print_domain_details(domain: &DomainOutput) {
    println!("{}", domain.domain.magenta().bold());
    println!("  ID: {}", domain.id);
    println!("  Status: {}", domain.status.to_lowercase());
    println!("  Workspace: {}", domain.workspace_id);
    println!("  Auto-renew: {}", format_bool(domain.auto_renew_enabled));
    println!(
        "  Next billing: {}",
        domain.next_billing_date.as_deref().unwrap_or("-")
    );
    println!("  Purchase price: {}", format_cents(domain.purchase_price));
    println!("  Renewal price: {}", format_cents(domain.renewal_price));
    println!(
        "  Nameservers: {}",
        if domain.nameservers.is_default {
            "Railway-managed"
        } else {
            "custom"
        }
    );
    for nameserver in &domain.nameservers.nameservers {
        println!("    {nameserver}");
    }
    if !domain.connected_service_instances.is_empty() {
        println!("  Connected services:");
        for service in &domain.connected_service_instances {
            println!(
                "    {} ({}/{}/{})",
                service.service_name.as_deref().unwrap_or("unknown service"),
                service.project_id,
                service.environment_id,
                service.service_id
            );
        }
    }
}

fn print_dns_table(records: &[DnsRecordOutput]) {
    let rows = records
        .iter()
        .map(|record| {
            vec![
                record.id.to_string(),
                record.record_type.clone(),
                record.host.clone(),
                record.answer.clone(),
                record.ttl.to_string(),
                record
                    .priority
                    .map(|priority| priority.to_string())
                    .unwrap_or_else(|| "-".into()),
            ]
        })
        .collect::<Vec<_>>();
    print_table(&["ID", "Type", "Host", "Answer", "TTL", "Priority"], rows);
}

fn print_table(headers: &[&str], rows: Vec<Vec<String>>) {
    let widths = headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            rows.iter()
                .filter_map(|row| row.get(index))
                .map(|value| console::measure_text_width(value))
                .max()
                .unwrap_or(0)
                .max(console::measure_text_width(header))
        })
        .collect::<Vec<_>>();

    for (index, header) in headers.iter().enumerate() {
        if index > 0 {
            print!("  ");
        }
        print!("{:<width$}", header.dimmed().bold(), width = widths[index]);
    }
    println!();

    for row in rows {
        for (index, value) in row.iter().enumerate() {
            if index > 0 {
                print!("  ");
            }
            print!("{:<width$}", value, width = widths[index]);
        }
        println!();
    }
}

fn domain_from_value<T: Serialize>(domain: &T) -> Result<DomainOutput> {
    let value = serde_json::to_value(domain)?;
    Ok(DomainOutput {
        id: required_string(&value, &["id"])?,
        domain: required_string(&value, &["domain"])?,
        status: required_string(&value, &["status"])?,
        auto_renew_enabled: required_bool(&value, &["auto_renew_enabled", "autoRenewEnabled"])?,
        purchase_price: required_i64(&value, &["purchase_price", "purchasePrice"])?,
        renewal_price: required_i64(&value, &["renewal_price", "renewalPrice"])?,
        registration_years: required_i64(&value, &["registration_years", "registrationYears"])?,
        next_billing_date: string_value(&value, &["next_billing_date", "nextBillingDate"]),
        workspace_id: required_string(&value, &["workspace_id", "workspaceId"])?,
        workspace_name: string_value(&value, &["workspace_name", "workspaceName"]),
        created_at: required_string(&value, &["created_at", "createdAt"])?,
        nameservers: nameservers_from_value(
            value
                .get("nameservers")
                .ok_or_else(|| anyhow!("Missing nameservers"))?,
        )?,
        connected_service_instances: connected_services_from_value(
            value
                .get("connected_service_instances")
                .or_else(|| value.get("connectedServiceInstances"))
                .unwrap_or(&Value::Array(Vec::new())),
        )?,
    })
}

fn dns_record_from_value<T: Serialize>(record: &T) -> Result<DnsRecordOutput> {
    let value = serde_json::to_value(record)?;
    Ok(DnsRecordOutput {
        id: required_i64(&value, &["id"])?,
        domain_name: required_string(&value, &["domain_name", "domainName"])?,
        host: required_string(&value, &["host"])?,
        fqdn: required_string(&value, &["fqdn"])?,
        record_type: required_string(&value, &["type", "type_"])?,
        answer: required_string(&value, &["answer"])?,
        ttl: required_i64(&value, &["ttl"])?,
        priority: i64_value(&value, &["priority"]),
    })
}

fn nameservers_from_value<T: Serialize>(value: &T) -> Result<NameserversOutput> {
    let value = serde_json::to_value(value)?;
    let nameservers = value
        .get("nameservers")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Missing nameservers"))?
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    Ok(NameserversOutput {
        nameservers,
        is_default: required_bool(&value, &["is_default", "isDefault"])?,
    })
}

fn connected_services_from_value(value: &Value) -> Result<Vec<ConnectedServiceOutput>> {
    let Some(services) = value.as_array() else {
        return Ok(Vec::new());
    };

    services
        .iter()
        .map(|service| {
            Ok(ConnectedServiceOutput {
                project_id: required_string(service, &["project_id", "projectId"])?,
                service_id: required_string(service, &["service_id", "serviceId"])?,
                environment_id: required_string(service, &["environment_id", "environmentId"])?,
                service_name: string_value(service, &["service_name", "serviceName"]),
            })
        })
        .collect()
}

fn required_string(value: &Value, keys: &[&str]) -> Result<String> {
    string_value(value, keys).ok_or_else(|| anyhow!("Missing string field {}", keys[0]))
}

fn string_value(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn required_bool(value: &Value, keys: &[&str]) -> Result<bool> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_bool))
        .ok_or_else(|| anyhow!("Missing boolean field {}", keys[0]))
}

fn required_i64(value: &Value, keys: &[&str]) -> Result<i64> {
    i64_value(value, keys).ok_or_else(|| anyhow!("Missing integer field {}", keys[0]))
}

fn i64_value(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_i64))
}

fn confirm_action(yes: bool, prompt: &str, non_interactive_message: &str) -> Result<()> {
    if yes {
        return Ok(());
    }

    if !std::io::stdout().is_terminal() {
        bail!("{non_interactive_message}");
    }

    if prompt_confirm_with_default(prompt, false)? {
        Ok(())
    } else {
        bail!("Cancelled")
    }
}

fn normalize_domain(input: &str) -> String {
    input
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_lowercase()
}

fn looks_like_domain_name(input: &str) -> bool {
    input.contains('.')
}

fn identifier_needs_workspace(input: &str) -> bool {
    looks_like_domain_name(&normalize_domain(input))
}

fn validate_nameservers(nameservers: Vec<String>) -> Result<Vec<String>> {
    if !(2..=13).contains(&nameservers.len()) {
        bail!("Provide between 2 and 13 nameservers");
    }

    nameservers
        .into_iter()
        .map(|nameserver| {
            let nameserver = normalize_domain(&nameserver);
            if nameserver.is_empty()
                || !nameserver.contains('.')
                || nameserver.chars().any(char::is_whitespace)
            {
                bail!("Invalid nameserver: {nameserver}");
            }
            Ok(nameserver)
        })
        .collect()
}

fn is_phrase_like(query: &str) -> bool {
    query
        .split_whitespace()
        .filter(|part| part.chars().count() >= 2)
        .count()
        >= 3
}

fn format_bool(value: bool) -> String {
    if value { "on".into() } else { "off".into() }
}

fn format_availability(value: Option<bool>) -> String {
    match value {
        Some(true) => "yes".green().to_string(),
        Some(false) => "no".red().to_string(),
        None => "-".to_string(),
    }
}

fn format_cents(cents: i64) -> String {
    format!("${}.{:02}", cents / 100, cents.abs() % 100)
}

fn format_domain_search_price(price: f64) -> String {
    let cents = (price * 100.0).round() as i64;
    if cents % 100 == 0 {
        format!("${}", cents / 100)
    } else {
        format!("${}.{:02}", cents / 100, cents.abs() % 100)
    }
}

fn add_purchase_urls(domains: &mut [DomainSearchResult]) -> Result<()> {
    let configs = Configs::new()?;
    add_purchase_urls_for_host(domains, configs.get_host())
}

fn add_purchase_urls_for_host(domains: &mut [DomainSearchResult], host: &str) -> Result<()> {
    for domain in domains {
        domain.purchase_url = if domain.purchasable == Some(true) {
            Some(purchase_url(host, &domain.domain)?)
        } else {
            None
        };
    }
    Ok(())
}

fn purchase_url(host: &str, domain: &str) -> Result<String> {
    let mut url = url::Url::parse(&format!("https://{host}/domains"))?;
    url.query_pairs_mut()
        .append_pair("q", domain)
        .append_pair("purchase", "true");
    Ok(url.to_string())
}

impl StatusFilter {
    fn to_gql(self) -> Option<queries::railway_domains::RailwayDomainStatus> {
        match self {
            Self::Active => Some(queries::railway_domains::RailwayDomainStatus::ACTIVE),
            Self::Purchasing => Some(queries::railway_domains::RailwayDomainStatus::PURCHASING),
            Self::Expired => Some(queries::railway_domains::RailwayDomainStatus::EXPIRED),
            Self::Refunded => Some(queries::railway_domains::RailwayDomainStatus::REFUNDED),
            Self::All => None,
        }
    }
}

impl DnsRecordType {
    fn to_create_gql(
        self,
    ) -> mutations::railway_domain_dns_record_create::RailwayDomainDnsRecordType {
        match self {
            Self::A => mutations::railway_domain_dns_record_create::RailwayDomainDnsRecordType::A,
            Self::Aaaa => {
                mutations::railway_domain_dns_record_create::RailwayDomainDnsRecordType::AAAA
            }
            Self::Aname => {
                mutations::railway_domain_dns_record_create::RailwayDomainDnsRecordType::ANAME
            }
            Self::Cname => {
                mutations::railway_domain_dns_record_create::RailwayDomainDnsRecordType::CNAME
            }
            Self::Mx => mutations::railway_domain_dns_record_create::RailwayDomainDnsRecordType::MX,
            Self::Ns => mutations::railway_domain_dns_record_create::RailwayDomainDnsRecordType::NS,
            Self::Srv => {
                mutations::railway_domain_dns_record_create::RailwayDomainDnsRecordType::SRV
            }
            Self::Txt => {
                mutations::railway_domain_dns_record_create::RailwayDomainDnsRecordType::TXT
            }
        }
    }

    fn to_update_gql(
        self,
    ) -> mutations::railway_domain_dns_record_update::RailwayDomainDnsRecordType {
        match self {
            Self::A => mutations::railway_domain_dns_record_update::RailwayDomainDnsRecordType::A,
            Self::Aaaa => {
                mutations::railway_domain_dns_record_update::RailwayDomainDnsRecordType::AAAA
            }
            Self::Aname => {
                mutations::railway_domain_dns_record_update::RailwayDomainDnsRecordType::ANAME
            }
            Self::Cname => {
                mutations::railway_domain_dns_record_update::RailwayDomainDnsRecordType::CNAME
            }
            Self::Mx => mutations::railway_domain_dns_record_update::RailwayDomainDnsRecordType::MX,
            Self::Ns => mutations::railway_domain_dns_record_update::RailwayDomainDnsRecordType::NS,
            Self::Srv => {
                mutations::railway_domain_dns_record_update::RailwayDomainDnsRecordType::SRV
            }
            Self::Txt => {
                mutations::railway_domain_dns_record_update::RailwayDomainDnsRecordType::TXT
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_domain_inputs() {
        assert_eq!(normalize_domain("https://Example.COM/path"), "example.com");
        assert_eq!(normalize_domain("ns1.example.com."), "ns1.example.com");
    }

    #[test]
    fn validates_nameserver_count_and_shape() {
        assert!(validate_nameservers(vec!["ns1.example.com".into()]).is_err());
        assert!(
            validate_nameservers(vec!["ns1.example.com".into(), "ns2.example.com".into()]).is_ok()
        );
        assert!(
            validate_nameservers(
                (1..=13)
                    .map(|index| format!("ns{index}.example.com"))
                    .collect()
            )
            .is_ok()
        );
        assert!(
            validate_nameservers(
                (1..=14)
                    .map(|index| format!("ns{index}.example.com"))
                    .collect()
            )
            .is_err()
        );
        assert!(validate_nameservers(vec!["ns1".into(), "ns2.example.com".into()]).is_err());
    }

    #[test]
    fn parses_search_messages() {
        let message = r#"{
            "type": "domains",
            "domains": {
                "example.com": {
                    "purchasable": true,
                    "purchasePrice": 12.34,
                    "renewalPrice": 12.34,
                    "allowedYears": [1]
                }
            }
        }"#;

        match parse_search_message(message).unwrap() {
            SearchMessage::Domains { domains } => {
                let result = domains.get("example.com").unwrap();
                assert_eq!(result.domain, "example.com");
                assert_eq!(result.purchasable, Some(true));
                assert_eq!(result.purchase_price, Some(12.34));
            }
            _ => panic!("expected domains message"),
        }
    }

    #[test]
    fn builds_purchase_urls() {
        assert_eq!(
            purchase_url("railway.com", "example.com").unwrap(),
            "https://railway.com/domains?q=example.com&purchase=true"
        );
    }

    #[test]
    fn adds_purchase_urls_for_purchasable_domains_only() {
        let mut domains = vec![
            DomainSearchResult {
                domain: "available.com".into(),
                purchasable: Some(true),
                ..Default::default()
            },
            DomainSearchResult {
                domain: "taken.com".into(),
                purchasable: Some(false),
                ..Default::default()
            },
        ];

        add_purchase_urls_for_host(&mut domains, "railway.com").unwrap();

        assert_eq!(
            domains[0].purchase_url.as_deref(),
            Some("https://railway.com/domains?q=available.com&purchase=true")
        );
        assert_eq!(domains[1].purchase_url, None);
    }

    #[test]
    fn detects_phrase_like_queries() {
        assert!(is_phrase_like("small business analytics"));
        assert!(!is_phrase_like("example.com"));
    }
}
