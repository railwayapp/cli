use std::fmt;

use anyhow::{Context, bail};
use chrono::{DateTime, TimeZone, Utc};
use clap::ValueEnum;
use colored::{ColoredString, Colorize};
use serde::Serialize;

use crate::controllers::project::{ServiceContext, resolve_service_context};

use super::*;

const FIELD_LABEL_WIDTH: usize = 22;
const CLOUDFLARE_PROVIDER: &str = "DETECTED_CDN_PROVIDER_CLOUDFLARE";

type ServiceEdgeConfig = queries::service_edge_config::ServiceEdgeConfigServiceInstanceEdgeConfig;
type ServiceEdgeCaching =
    queries::service_edge_config::ServiceEdgeConfigServiceInstanceEdgeConfigCaching;

/// Manage CDN caching for a service
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway cdn status --service web\n  railway cdn enable --service web\n  railway cdn update --html-caching force --default-ttl 4h\n  railway cdn update --no-swr --purge-on-deploy all\n  railway cdn purge html\n  railway cdn purge all --json\n  railway cdn disable --service web\n\nAutomation notes:\n  CDN caching is service-scoped and requires an applied public domain. Purges apply to the whole service; per-URL purge is not supported."
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
    /// Show CDN settings for a service
    Status,

    /// Enable CDN caching
    Enable,

    /// Disable CDN caching
    Disable,

    /// Update CDN caching settings
    Update(UpdateArgs),

    /// Purge cached content
    Purge(PurgeArgs),
}

#[derive(Parser)]
#[clap(group(
    clap::ArgGroup::new("setting")
        .args(["html_caching", "default_ttl", "swr", "no_swr", "purge_on_deploy"])
        .required(true)
        .multiple(true)
))]
struct UpdateArgs {
    /// HTML caching mode
    #[clap(long = "html-caching", value_enum)]
    html_caching: Option<HtmlCachingValue>,

    /// Default TTL: 30m, 1h, 2h, 4h, 12h, 1d, or matching seconds
    #[clap(long = "default-ttl", value_parser = parse_default_ttl)]
    default_ttl: Option<DefaultTtl>,

    /// Honor stale-while-revalidate Cache-Control directives
    #[clap(long, conflicts_with = "no_swr")]
    swr: bool,

    /// Ignore stale-while-revalidate Cache-Control directives
    #[clap(long = "no-swr", conflicts_with = "swr")]
    no_swr: bool,

    /// Cache purge policy after successful deploys
    #[clap(long = "purge-on-deploy", value_enum)]
    purge_on_deploy: Option<PurgeOnDeployValue>,
}

#[derive(Parser)]
struct PurgeArgs {
    /// Content scope to purge
    #[clap(value_enum)]
    scope: PurgeScopeValue,
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
        Commands::Status => status(project, service, environment, json).await?,
        Commands::Enable => enable(project, service, environment, json).await?,
        Commands::Disable => disable(project, service, environment, json).await?,
        Commands::Update(update_args) => {
            update(project, service, environment, update_args, json).await?
        }
        Commands::Purge(purge_args) => {
            purge(project, service, environment, purge_args, json).await?
        }
    }

    Ok(())
}

async fn status(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let status = load_cdn_status(&ctx).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        print_status(&status);
    }

    Ok(())
}

async fn enable(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let domains = fetch_domains(&ctx).await?;
    ensure_available(&domains)?;

    post_graphql::<mutations::EnableServiceCdn, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::enable_service_cdn::Variables {
            input: mutations::enable_service_cdn::EnableServiceCdnInput {
                service_id: ctx.service_id.clone(),
                environment_id: ctx.environment_id.clone(),
                config: None,
            },
        },
    )
    .await?;

    let status = load_cdn_status_with_domains(&ctx, domains).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ActionOutput {
                action: "enable",
                scope: None,
                status,
            })?
        );
    } else {
        println!(
            "Enabled CDN caching for service {} in environment {}.",
            ctx.service_name.bold(),
            ctx.environment_name.bold()
        );
        print_status(&status);
    }

    Ok(())
}

async fn disable(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;

    post_graphql::<mutations::DisableServiceCdn, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::disable_service_cdn::Variables {
            input: mutations::disable_service_cdn::DisableServiceCdnInput {
                service_id: ctx.service_id.clone(),
                environment_id: ctx.environment_id.clone(),
            },
        },
    )
    .await?;

    let status = load_cdn_status(&ctx).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ActionOutput {
                action: "disable",
                scope: None,
                status,
            })?
        );
    } else {
        println!(
            "Disabled CDN caching for service {} in environment {}.",
            ctx.service_name.bold(),
            ctx.environment_name.bold()
        );
        print_status(&status);
    }

    Ok(())
}

async fn update(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    args: UpdateArgs,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let domains = fetch_domains(&ctx).await?;
    ensure_available(&domains)?;
    let edge_config = fetch_edge_config(&ctx)
        .await?
        .context("CDN caching is disabled. Run `railway cdn enable` first.")?;
    let caching = edge_caching(&edge_config)
        .context("CDN caching is disabled. Run `railway cdn enable` first.")?;

    let current_purge_on_deploy = purge_on_deploy_from_query(&caching.purge_on_deploy)?;
    let swr_enabled = if args.swr {
        true
    } else if args.no_swr {
        false
    } else {
        caching.stale_while_revalidate.enabled
    };

    post_graphql::<mutations::UpdateServiceEdgeConfig, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::update_service_edge_config::Variables {
            input: mutations::update_service_edge_config::UpdateServiceEdgeConfigInput {
                service_id: ctx.service_id.clone(),
                environment_id: ctx.environment_id.clone(),
                config: mutations::update_service_edge_config::EdgeConfigInput {
                    caching: Some(
                        mutations::update_service_edge_config::EdgeCachingConfigInput {
                            mode: Some(caching.mode.clone()),
                            default_ttl_seconds: Some(
                                args.default_ttl
                                    .map(DefaultTtl::seconds)
                                    .unwrap_or(caching.default_ttl_seconds),
                            ),
                            html_caching: Some(
                                args.html_caching
                                    .map(|mode| mode.as_api().to_string())
                                    .unwrap_or_else(|| caching.html_caching.clone()),
                            ),
                            stale_while_revalidate: Some(
                                mutations::update_service_edge_config::StaleWhileRevalidateInput {
                                    enabled: swr_enabled,
                                },
                            ),
                            purge_on_deploy: Some(
                                args.purge_on_deploy
                                    .unwrap_or(current_purge_on_deploy)
                                    .to_update_enum(),
                            ),
                        },
                    ),
                },
            },
        },
    )
    .await?;

    let status = load_cdn_status_with_domains(&ctx, domains).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ActionOutput {
                action: "update",
                scope: None,
                status,
            })?
        );
    } else {
        println!(
            "Updated CDN settings for service {} in environment {}.",
            ctx.service_name.bold(),
            ctx.environment_name.bold()
        );
        print_status(&status);
    }

    Ok(())
}

async fn purge(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    args: PurgeArgs,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let domains = fetch_domains(&ctx).await?;
    ensure_available(&domains)?;
    let edge_config = fetch_edge_config(&ctx)
        .await?
        .context("CDN caching is disabled. Run `railway cdn enable` first.")?;
    if edge_caching(&edge_config).is_none() {
        bail!("CDN caching is disabled. Run `railway cdn enable` first.");
    }

    post_graphql::<mutations::PurgeServiceCache, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::purge_service_cache::Variables {
            input: mutations::purge_service_cache::PurgeServiceCacheInput {
                service_id: ctx.service_id.clone(),
                environment_id: ctx.environment_id.clone(),
                scope: args.scope.to_purge_enum(),
            },
        },
    )
    .await?;

    let status = load_cdn_status_with_domains(&ctx, domains).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ActionOutput {
                action: "purge",
                scope: Some(args.scope),
                status,
            })?
        );
    } else {
        println!(
            "Purged {} cache for service {} in environment {}.",
            args.scope,
            ctx.service_name.bold(),
            ctx.environment_name.bold()
        );
        print_status(&status);
    }

    Ok(())
}

async fn load_cdn_status(ctx: &ServiceContext) -> Result<CdnStatusOutput> {
    let domains = fetch_domains(ctx).await?;
    load_cdn_status_with_domains(ctx, domains).await
}

async fn load_cdn_status_with_domains(
    ctx: &ServiceContext,
    domains: queries::domains::DomainsDomains,
) -> Result<CdnStatusOutput> {
    let available = has_public_domain(&domains);
    let edge_config = if available {
        fetch_edge_config(ctx).await?
    } else {
        None
    };
    Ok(build_status(ctx, available, edge_config, &domains))
}

async fn fetch_domains(ctx: &ServiceContext) -> Result<queries::domains::DomainsDomains> {
    let response = post_graphql::<queries::Domains, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::domains::Variables {
            project_id: ctx.project_id.clone(),
            environment_id: ctx.environment_id.clone(),
            service_id: ctx.service_id.clone(),
        },
    )
    .await?;
    Ok(response.domains)
}

async fn fetch_edge_config(ctx: &ServiceContext) -> Result<Option<ServiceEdgeConfig>> {
    let response = post_graphql::<queries::ServiceEdgeConfig, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::service_edge_config::Variables {
            environment_id: ctx.environment_id.clone(),
            service_id: ctx.service_id.clone(),
        },
    )
    .await?;
    Ok(response.service_instance.edge_config)
}

fn build_status(
    ctx: &ServiceContext,
    available: bool,
    edge_config: Option<ServiceEdgeConfig>,
    domains: &queries::domains::DomainsDomains,
) -> CdnStatusOutput {
    let enabled = cdn_enabled(edge_config.as_ref());
    let warnings = if enabled && has_verified_cloudflare_domain(domains) {
        vec![WarningOutput {
            kind: "cloudflareProxyDetected".to_string(),
            message: "One or more verified custom domains is proxied through Cloudflare. For best cache performance, set the DNS record to DNS only so requests reach Railway's edge directly.".to_string(),
        }]
    } else {
        Vec::new()
    };

    CdnStatusOutput {
        service: ResourceRef {
            id: ctx.service_id.clone(),
            name: ctx.service_name.clone(),
        },
        environment: ResourceRef {
            id: ctx.environment_id.clone(),
            name: ctx.environment_name.clone(),
        },
        cdn: CdnOutput {
            available,
            enabled,
            caching: edge_config
                .as_ref()
                .and_then(edge_caching)
                .map(CachingOutput::from),
            purge: edge_config
                .as_ref()
                .map(PurgeStatusOutput::from)
                .unwrap_or_default(),
            warnings,
        },
    }
}

fn cdn_enabled(edge_config: Option<&ServiceEdgeConfig>) -> bool {
    edge_config.and_then(edge_caching).is_some()
}

fn edge_caching(edge_config: &ServiceEdgeConfig) -> Option<&ServiceEdgeCaching> {
    if edge_config.enabled {
        edge_config
            .caching
            .as_ref()
            .filter(|caching| !caching.mode.eq_ignore_ascii_case("off"))
    } else {
        None
    }
}

fn ensure_available(domains: &queries::domains::DomainsDomains) -> Result<()> {
    if has_public_domain(domains) {
        Ok(())
    } else {
        bail!(
            "CDN caching requires an applied public domain. Add a domain with `railway domain` first."
        )
    }
}

fn has_public_domain(domains: &queries::domains::DomainsDomains) -> bool {
    domains
        .service_domains
        .iter()
        .any(|domain| is_applied_domain_sync_status(&domain.sync_status))
        || domains
            .custom_domains
            .iter()
            .any(|domain| is_applied_domain_sync_status(&domain.sync_status))
}

fn is_applied_domain_sync_status<T: fmt::Debug>(sync_status: &T) -> bool {
    matches!(enum_name(sync_status).as_str(), "ACTIVE" | "UNSPECIFIED")
}

fn has_verified_cloudflare_domain(domains: &queries::domains::DomainsDomains) -> bool {
    domains.custom_domains.iter().any(|domain| {
        is_verified_cloudflare_provider(
            domain.status.verified,
            domain
                .status
                .cdn_provider
                .as_ref()
                .map(enum_name)
                .as_deref(),
        )
    })
}

fn is_verified_cloudflare_provider(verified: bool, provider: Option<&str>) -> bool {
    verified && provider == Some(CLOUDFLARE_PROVIDER)
}

fn print_status(status: &CdnStatusOutput) {
    println!("{}", "CDN caching".bold());
    println!();
    print_field("Service:", &status.service.name.green().bold());
    print_field("Environment:", &status.environment.name.blue().bold());

    if !status.cdn.available {
        print_field("Status:", &"unavailable".yellow().bold());
        print_field(
            "Message:",
            &"Add a public domain with `railway domain` before enabling CDN caching.",
        );
        return;
    }

    print_field("Status:", &status_label(status.cdn.enabled));

    if let Some(caching) = &status.cdn.caching {
        print_field("HTML caching:", &caching.html_caching);
        print_field("Default TTL:", &caching.default_ttl_label);
        print_field("Honor SWR:", &yes_no(caching.stale_while_revalidate));
        print_field("Purge on deploy:", &caching.purge_on_deploy);
    }

    print_field(
        "HTML last purged:",
        &purge_time_label(status.cdn.purge.html_last_purged_epoch, Utc::now()),
    );
    print_field(
        "All last purged:",
        &purge_time_label(status.cdn.purge.all_last_purged_epoch, Utc::now()),
    );

    for warning in &status.cdn.warnings {
        println!();
        print_field("Warning:", &warning.message.yellow());
    }
}

fn print_field(label: &str, value: &dyn fmt::Display) {
    let padded = format!("{label:<FIELD_LABEL_WIDTH$}");
    println!("{} {value}", padded.dimmed());
}

fn status_label(enabled: bool) -> ColoredString {
    if enabled {
        "enabled".green().bold()
    } else {
        "disabled".yellow().bold()
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn enum_name<T: fmt::Debug>(value: &T) -> String {
    format!("{value:?}")
}

fn purge_on_deploy_from_query(
    value: &queries::service_edge_config::PurgeOnDeploy,
) -> Result<PurgeOnDeployValue> {
    match enum_name(value).as_str() {
        "OFF" => Ok(PurgeOnDeployValue::Off),
        "HTML" => Ok(PurgeOnDeployValue::Html),
        "ALL" => Ok(PurgeOnDeployValue::All),
        other => bail!("Unrecognized purge-on-deploy value returned by API: {other}"),
    }
}

fn purge_time(epoch: i64) -> Option<DateTime<Utc>> {
    if epoch <= 0 {
        return None;
    }
    Utc.timestamp_opt(epoch, 0).single()
}

fn purge_time_rfc3339(epoch: i64) -> Option<String> {
    purge_time(epoch).map(|time| time.to_rfc3339())
}

fn purge_time_label(epoch: i64, now: DateTime<Utc>) -> String {
    let Some(time) = purge_time(epoch) else {
        return "Never".to_string();
    };
    if time >= now {
        "Just now".to_string()
    } else {
        time.to_rfc3339()
    }
}

fn purge_epoch_by_kind(value: &serde_json::Value, kind: &str) -> i64 {
    value
        .get(kind)
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
                .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
        })
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DefaultTtl(i64);

impl DefaultTtl {
    fn seconds(self) -> i64 {
        self.0
    }
}

fn parse_default_ttl(value: &str) -> std::result::Result<DefaultTtl, String> {
    match value.to_ascii_lowercase().as_str() {
        "30m" | "1800" => Ok(DefaultTtl(1800)),
        "1h" | "3600" => Ok(DefaultTtl(3600)),
        "2h" | "7200" => Ok(DefaultTtl(7200)),
        "4h" | "14400" => Ok(DefaultTtl(14400)),
        "12h" | "43200" => Ok(DefaultTtl(43200)),
        "1d" | "86400" => Ok(DefaultTtl(86400)),
        _ => Err(
            "must be one of 30m, 1h, 2h, 4h, 12h, 1d, 1800, 3600, 7200, 14400, 43200, or 86400"
                .to_string(),
        ),
    }
}

fn ttl_label(seconds: i64) -> String {
    match seconds {
        1800 => "30 minutes".to_string(),
        3600 => "1 hour".to_string(),
        7200 => "2 hours".to_string(),
        14400 => "4 hours".to_string(),
        43200 => "12 hours".to_string(),
        86400 => "1 day".to_string(),
        _ => format!("{seconds} seconds"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum HtmlCachingValue {
    Auto,
    Force,
    Never,
}

impl HtmlCachingValue {
    fn as_api(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Force => "force",
            Self::Never => "never",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum PurgeOnDeployValue {
    Off,
    Html,
    All,
}

impl PurgeOnDeployValue {
    fn to_update_enum(self) -> mutations::update_service_edge_config::PurgeOnDeploy {
        match self {
            Self::Off => mutations::update_service_edge_config::PurgeOnDeploy::OFF,
            Self::Html => mutations::update_service_edge_config::PurgeOnDeploy::HTML,
            Self::All => mutations::update_service_edge_config::PurgeOnDeploy::ALL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum PurgeScopeValue {
    Html,
    All,
}

impl PurgeScopeValue {
    fn to_purge_enum(self) -> mutations::purge_service_cache::PurgeCacheScope {
        match self {
            Self::Html => mutations::purge_service_cache::PurgeCacheScope::HTML,
            Self::All => mutations::purge_service_cache::PurgeCacheScope::ALL,
        }
    }
}

impl fmt::Display for PurgeScopeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Self::Html => "HTML",
            Self::All => "all",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceRef {
    id: String,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CdnStatusOutput {
    service: ResourceRef,
    environment: ResourceRef,
    cdn: CdnOutput,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CdnOutput {
    available: bool,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    caching: Option<CachingOutput>,
    purge: PurgeStatusOutput,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<WarningOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CachingOutput {
    html_caching: String,
    default_ttl_seconds: i64,
    #[serde(skip_serializing)]
    default_ttl_label: String,
    stale_while_revalidate: bool,
    purge_on_deploy: String,
}

impl From<&queries::service_edge_config::ServiceEdgeConfigServiceInstanceEdgeConfigCaching>
    for CachingOutput
{
    fn from(
        caching: &queries::service_edge_config::ServiceEdgeConfigServiceInstanceEdgeConfigCaching,
    ) -> Self {
        Self {
            html_caching: caching.html_caching.clone(),
            default_ttl_seconds: caching.default_ttl_seconds,
            default_ttl_label: ttl_label(caching.default_ttl_seconds),
            stale_while_revalidate: caching.stale_while_revalidate.enabled,
            purge_on_deploy: enum_name(&caching.purge_on_deploy).to_ascii_lowercase(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PurgeStatusOutput {
    html_last_purged_epoch: i64,
    html_last_purged_at: Option<String>,
    all_last_purged_epoch: i64,
    all_last_purged_at: Option<String>,
}

impl From<&queries::service_edge_config::ServiceEdgeConfigServiceInstanceEdgeConfig>
    for PurgeStatusOutput
{
    fn from(
        edge_config: &queries::service_edge_config::ServiceEdgeConfigServiceInstanceEdgeConfig,
    ) -> Self {
        let html_last_purged_epoch = purge_epoch_by_kind(&edge_config.purge_epoch_by_kind, "html");
        let all_last_purged_epoch = edge_config.purge_epoch;
        Self {
            html_last_purged_epoch,
            html_last_purged_at: purge_time_rfc3339(html_last_purged_epoch),
            all_last_purged_epoch,
            all_last_purged_at: purge_time_rfc3339(all_last_purged_epoch),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WarningOutput {
    kind: String,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActionOutput {
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<PurgeScopeValue>,
    #[serde(flatten)]
    status: CdnStatusOutput,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_subcommands_and_selectors() {
        assert!(matches!(
            Args::parse_from(["cdn", "status"]).command,
            Commands::Status
        ));
        assert!(matches!(
            Args::parse_from(["cdn", "enable", "--service", "web"]).command,
            Commands::Enable
        ));
        assert!(matches!(
            Args::parse_from(["cdn", "disable", "--environment", "production"]).command,
            Commands::Disable
        ));
        let args = Args::parse_from([
            "cdn",
            "--project",
            "project-id",
            "--environment",
            "production",
            "--service",
            "web",
            "--json",
            "status",
        ]);
        assert_eq!(args.project.as_deref(), Some("project-id"));
        assert_eq!(args.environment.as_deref(), Some("production"));
        assert_eq!(args.service.as_deref(), Some("web"));
        assert!(args.json);
    }

    #[test]
    fn parses_update_settings() {
        let args = Args::parse_from([
            "cdn",
            "update",
            "--html-caching",
            "force",
            "--default-ttl",
            "4h",
            "--no-swr",
            "--purge-on-deploy",
            "all",
        ]);

        let Commands::Update(update) = args.command else {
            panic!("expected update");
        };
        assert_eq!(update.html_caching, Some(HtmlCachingValue::Force));
        assert_eq!(update.default_ttl, Some(DefaultTtl(14400)));
        assert!(update.no_swr);
        assert_eq!(update.purge_on_deploy, Some(PurgeOnDeployValue::All));
    }

    #[test]
    fn update_requires_at_least_one_setting() {
        assert!(Args::try_parse_from(["cdn", "update"]).is_err());
    }

    #[test]
    fn swr_flags_conflict() {
        assert!(Args::try_parse_from(["cdn", "update", "--swr", "--no-swr"]).is_err());
    }

    #[test]
    fn parses_purge_scope() {
        let args = Args::parse_from(["cdn", "purge", "html"]);
        assert!(matches!(
            args.command,
            Commands::Purge(PurgeArgs {
                scope: PurgeScopeValue::Html
            })
        ));
        assert!(Args::try_parse_from(["cdn", "purge", "--scope", "html"]).is_err());
    }

    #[test]
    fn validates_default_ttl_values() {
        assert_eq!(parse_default_ttl("30m").unwrap(), DefaultTtl(1800));
        assert_eq!(parse_default_ttl("1h").unwrap(), DefaultTtl(3600));
        assert_eq!(parse_default_ttl("2h").unwrap(), DefaultTtl(7200));
        assert_eq!(parse_default_ttl("4h").unwrap(), DefaultTtl(14400));
        assert_eq!(parse_default_ttl("12h").unwrap(), DefaultTtl(43200));
        assert_eq!(parse_default_ttl("1d").unwrap(), DefaultTtl(86400));
        assert_eq!(parse_default_ttl("86400").unwrap(), DefaultTtl(86400));
        assert!(parse_default_ttl("3h").is_err());
        assert!(parse_default_ttl("60").is_err());
    }

    #[test]
    fn purge_time_label_handles_never_past_and_future() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        assert_eq!(purge_time_label(0, now), "Never");
        assert_eq!(
            purge_time_label(1_600_000_000, now),
            "2020-09-13T12:26:40+00:00"
        );
        assert_eq!(purge_time_label(1_800_000_000, now), "Just now");
    }

    #[test]
    fn purge_epoch_by_kind_accepts_numbers_and_strings() {
        let value = serde_json::json!({
            "html": 123,
            "css": "456",
            "bad": "nope"
        });
        assert_eq!(purge_epoch_by_kind(&value, "html"), 123);
        assert_eq!(purge_epoch_by_kind(&value, "css"), 456);
        assert_eq!(purge_epoch_by_kind(&value, "bad"), 0);
        assert_eq!(purge_epoch_by_kind(&value, "missing"), 0);
    }

    #[test]
    fn status_output_uses_camel_case_and_null_purge_times() {
        let output = CdnStatusOutput {
            service: ResourceRef {
                id: "svc_123".to_string(),
                name: "web".to_string(),
            },
            environment: ResourceRef {
                id: "env_123".to_string(),
                name: "production".to_string(),
            },
            cdn: CdnOutput {
                available: true,
                enabled: false,
                caching: None,
                purge: PurgeStatusOutput::default(),
                warnings: Vec::new(),
            },
        };
        let value = serde_json::to_value(output).unwrap();
        assert_eq!(value["cdn"]["available"], true);
        assert_eq!(value["cdn"]["enabled"], false);
        assert!(value["cdn"].get("caching").is_none());
        assert_eq!(value["cdn"]["purge"]["htmlLastPurgedEpoch"], 0);
        assert!(value["cdn"]["purge"]["htmlLastPurgedAt"].is_null());
        assert_eq!(value["cdn"]["purge"]["allLastPurgedEpoch"], 0);
        assert!(value["cdn"]["purge"]["allLastPurgedAt"].is_null());
    }

    #[test]
    fn enabled_status_json_omits_internal_caching_fields() {
        let output = CdnStatusOutput {
            service: ResourceRef {
                id: "svc_123".to_string(),
                name: "web".to_string(),
            },
            environment: ResourceRef {
                id: "env_123".to_string(),
                name: "production".to_string(),
            },
            cdn: CdnOutput {
                available: true,
                enabled: true,
                caching: Some(CachingOutput {
                    html_caching: "force".to_string(),
                    default_ttl_seconds: 43200,
                    default_ttl_label: "12 hours".to_string(),
                    stale_while_revalidate: true,
                    purge_on_deploy: "html".to_string(),
                }),
                purge: PurgeStatusOutput::default(),
                warnings: Vec::new(),
            },
        };
        let value = serde_json::to_value(output).unwrap();
        let caching = &value["cdn"]["caching"];
        assert_eq!(caching["htmlCaching"], "force");
        assert_eq!(caching["defaultTtlSeconds"], 43200);
        assert!(caching.get("mode").is_none());
        assert!(caching.get("defaultTtlLabel").is_none());
        assert!(value["cdn"].get("edgeConfigId").is_none());
    }

    #[test]
    fn action_output_uses_action_and_status_shape() {
        let output = ActionOutput {
            action: "purge",
            scope: Some(PurgeScopeValue::Html),
            status: CdnStatusOutput {
                service: ResourceRef {
                    id: "svc_123".to_string(),
                    name: "web".to_string(),
                },
                environment: ResourceRef {
                    id: "env_123".to_string(),
                    name: "production".to_string(),
                },
                cdn: CdnOutput {
                    available: true,
                    enabled: true,
                    caching: None,
                    purge: PurgeStatusOutput::default(),
                    warnings: Vec::new(),
                },
            },
        };
        let value = serde_json::to_value(output).unwrap();
        assert_eq!(value["action"], "purge");
        assert_eq!(value["scope"], "html");
        assert!(value.get("purged").is_none());
        assert!(value.get("enabled").is_none());
        assert_eq!(value["service"]["name"], "web");
        assert_eq!(value["cdn"]["enabled"], true);
    }

    #[test]
    fn unavailable_status_is_disabled_without_edge_config() {
        let status = CdnStatusOutput {
            service: ResourceRef {
                id: "svc_123".to_string(),
                name: "web".to_string(),
            },
            environment: ResourceRef {
                id: "env_123".to_string(),
                name: "production".to_string(),
            },
            cdn: CdnOutput {
                available: false,
                enabled: false,
                caching: None,
                purge: PurgeStatusOutput::default(),
                warnings: Vec::new(),
            },
        };
        assert!(!status.cdn.available);
        assert!(!status.cdn.enabled);
    }

    #[test]
    fn disabled_edge_config_with_cached_settings_is_not_cdn_enabled() {
        let edge_config = edge_config(false);
        assert!(!cdn_enabled(Some(&edge_config)));
        assert!(edge_caching(&edge_config).is_none());
    }

    #[test]
    fn enabled_edge_config_requires_caching_settings() {
        let mut edge_config = edge_config(true);
        assert!(cdn_enabled(Some(&edge_config)));

        edge_config.caching = None;
        assert!(!cdn_enabled(Some(&edge_config)));
        assert!(edge_caching(&edge_config).is_none());
    }

    #[test]
    fn edge_config_mode_off_is_not_cdn_enabled() {
        let mut edge_config = edge_config(true);
        edge_config.caching.as_mut().unwrap().mode = "off".to_string();

        assert!(!cdn_enabled(Some(&edge_config)));
        assert!(edge_caching(&edge_config).is_none());
    }

    #[test]
    fn public_domain_availability_requires_applied_sync_status() {
        let empty = domains(Vec::new());
        assert!(!has_public_domain(&empty));

        let creating = domains(vec![queries::domains::ServiceDomainSyncStatus::CREATING]);
        assert!(!has_public_domain(&creating));

        let deleting = domains(vec![queries::domains::ServiceDomainSyncStatus::DELETING]);
        assert!(!has_public_domain(&deleting));

        let active = domains(vec![queries::domains::ServiceDomainSyncStatus::ACTIVE]);
        assert!(has_public_domain(&active));

        let unspecified = domains(vec![queries::domains::ServiceDomainSyncStatus::UNSPECIFIED]);
        assert!(has_public_domain(&unspecified));
    }

    #[test]
    fn cloudflare_warning_requires_verified_cloudflare_provider() {
        assert!(is_verified_cloudflare_provider(
            true,
            Some(CLOUDFLARE_PROVIDER)
        ));
        assert!(!is_verified_cloudflare_provider(
            false,
            Some(CLOUDFLARE_PROVIDER)
        ));
        assert!(!is_verified_cloudflare_provider(
            true,
            Some("DETECTED_CDN_PROVIDER_UNSPECIFIED")
        ));
        assert!(!is_verified_cloudflare_provider(true, None));
    }

    fn edge_config(enabled: bool) -> ServiceEdgeConfig {
        ServiceEdgeConfig {
            id: "edge_123".to_string(),
            enabled,
            caching: Some(ServiceEdgeCaching {
                mode: "auto".to_string(),
                default_ttl_seconds: 7200,
                html_caching: "auto".to_string(),
                stale_while_revalidate:
                    queries::service_edge_config::ServiceEdgeConfigServiceInstanceEdgeConfigCachingStaleWhileRevalidate {
                        enabled: true,
                    },
                purge_on_deploy: queries::service_edge_config::PurgeOnDeploy::HTML,
            }),
            purge_epoch: 0,
            purge_epoch_by_kind: serde_json::json!({}),
        }
    }

    fn domains(
        sync_statuses: Vec<queries::domains::ServiceDomainSyncStatus>,
    ) -> queries::domains::DomainsDomains {
        queries::domains::DomainsDomains {
            service_domains: sync_statuses
                .into_iter()
                .enumerate()
                .map(
                    |(index, sync_status)| queries::domains::DomainsDomainsServiceDomains {
                        id: format!("sd_{index}"),
                        domain: format!("web-{index}.up.railway.app"),
                        suffix: Some("up.railway.app".to_string()),
                        environment_id: "env_123".to_string(),
                        service_id: "svc_123".to_string(),
                        target_port: None,
                        sync_status,
                        created_at: None,
                        updated_at: None,
                    },
                )
                .collect(),
            custom_domains: Vec::new(),
        }
    }
}
