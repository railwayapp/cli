use std::fmt;

use chrono::{DateTime, TimeZone, Utc};
use colored::ColoredString;
use serde::Serialize;

use crate::controllers::project::{ServiceContext, resolve_service_context};

use super::*;

const FIELD_LABEL_WIDTH: usize = 22;

type ServiceWafConfig = queries::service_waf_config::ServiceWafConfigServiceInstanceEdgeConfig;

/// Manage WAF settings for a service
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway waf under-attack status --service web\n  railway waf under-attack enable --service web\n  railway waf under-attack enable --duration 1h\n  railway waf under-attack enable --duration 24h --json\n  railway waf under-attack disable --service web\n\nAutomation notes:\n  Under Attack Mode is service-scoped and requires an applied public domain. While active, visitors must pass a browser check and non-browser clients without clearance may receive 429 responses."
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
    /// Manage Under Attack Mode
    UnderAttack(UnderAttackArgs),
}

#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway waf under-attack status --service web\n  railway waf under-attack enable --service web\n  railway waf under-attack enable --duration 1h\n  railway waf under-attack enable --duration 24h --json\n  railway waf under-attack disable --service web\n\nAutomation notes:\n  Under Attack Mode is service-scoped and requires an applied public domain. While active, visitors must pass a browser check and non-browser clients without clearance may receive 429 responses."
)]
struct UnderAttackArgs {
    #[clap(subcommand)]
    command: UnderAttackCommands,
}

#[derive(Parser)]
enum UnderAttackCommands {
    /// Show Under Attack Mode status
    Status,

    /// Enable Under Attack Mode
    Enable(EnableArgs),

    /// Disable Under Attack Mode
    Disable,
}

#[derive(Parser)]
struct EnableArgs {
    /// Duration: forever, 1h, 3h, 12h, 24h, or matching seconds
    #[clap(long, default_value = "forever", value_parser = parse_duration)]
    duration: UnderAttackDuration,
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
        Commands::UnderAttack(args) => match args.command {
            UnderAttackCommands::Status => status(project, service, environment, json).await?,
            UnderAttackCommands::Enable(enable_args) => {
                enable(project, service, environment, enable_args.duration, json).await?
            }
            UnderAttackCommands::Disable => disable(project, service, environment, json).await?,
        },
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
    let status = load_waf_status(&ctx).await?;

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
    duration: UnderAttackDuration,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let domains = fetch_domains(&ctx).await?;
    ensure_available(&domains)?;
    let duration_seconds = duration.seconds();

    set_under_attack_mode(&ctx, true, duration_seconds).await?;

    let status = load_waf_status_with_domains(&ctx, domains).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ActionOutput {
                action: "enable",
                duration_seconds,
                status,
            })?
        );
    } else {
        println!(
            "Enabled Under Attack Mode for service {} in environment {}.",
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
    let domains = fetch_domains(&ctx).await?;

    if !has_public_domain(&domains) {
        let status = load_waf_status_with_domains(&ctx, domains).await?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&ActionOutput {
                    action: "disable",
                    duration_seconds: None,
                    status,
                })?
            );
        } else {
            print_status(&status);
        }
        return Ok(());
    }

    set_under_attack_mode(&ctx, false, None).await?;

    let status = load_waf_status_with_domains(&ctx, domains).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ActionOutput {
                action: "disable",
                duration_seconds: None,
                status,
            })?
        );
    } else {
        println!(
            "Disabled Under Attack Mode for service {} in environment {}.",
            ctx.service_name.bold(),
            ctx.environment_name.bold()
        );
        print_status(&status);
    }

    Ok(())
}

async fn set_under_attack_mode(
    ctx: &ServiceContext,
    enabled: bool,
    duration_seconds: Option<i64>,
) -> Result<()> {
    post_graphql::<mutations::SetServiceUnderAttackMode, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::set_service_under_attack_mode::Variables {
            input: mutations::set_service_under_attack_mode::SetServiceUnderAttackModeInput {
                service_id: ctx.service_id.clone(),
                environment_id: ctx.environment_id.clone(),
                enabled,
                duration_seconds,
            },
        },
    )
    .await?;

    Ok(())
}

async fn load_waf_status(ctx: &ServiceContext) -> Result<WafStatusOutput> {
    let domains = fetch_domains(ctx).await?;
    load_waf_status_with_domains(ctx, domains).await
}

async fn load_waf_status_with_domains(
    ctx: &ServiceContext,
    domains: queries::domains::DomainsDomains,
) -> Result<WafStatusOutput> {
    let available = has_public_domain(&domains);
    let edge_config = if available {
        fetch_waf_config(ctx).await?
    } else {
        None
    };
    Ok(build_status(ctx, available, edge_config, Utc::now()))
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

async fn fetch_waf_config(ctx: &ServiceContext) -> Result<Option<ServiceWafConfig>> {
    let response = post_graphql::<queries::ServiceWafConfig, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::service_waf_config::Variables {
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
    edge_config: Option<ServiceWafConfig>,
    now: DateTime<Utc>,
) -> WafStatusOutput {
    let under_attack_mode = under_attack_mode_output(
        edge_config.and_then(|config| config.under_attack_mode_until),
        now,
    );
    let warnings = if under_attack_mode.enabled {
        vec![WarningOutput {
            kind: "nonBrowserTrafficBlocked".to_string(),
            message: "API clients and webhooks without a verified browser session may receive 429 responses while Under Attack Mode is active.".to_string(),
        }]
    } else {
        Vec::new()
    };

    WafStatusOutput {
        service: ResourceRef {
            id: ctx.service_id.clone(),
            name: ctx.service_name.clone(),
        },
        environment: ResourceRef {
            id: ctx.environment_id.clone(),
            name: ctx.environment_name.clone(),
        },
        waf: WafOutput {
            available,
            under_attack_mode,
            warnings,
        },
    }
}

fn under_attack_mode_output(until_epoch: Option<i64>, now: DateTime<Utc>) -> UnderAttackModeOutput {
    match until_epoch {
        None => UnderAttackModeOutput {
            enabled: false,
            state: UnderAttackState::Disabled,
            until_epoch: None,
            until: None,
            remaining_seconds: None,
        },
        Some(0) => UnderAttackModeOutput {
            enabled: true,
            state: UnderAttackState::ActiveForever,
            until_epoch: Some(0),
            until: None,
            remaining_seconds: None,
        },
        Some(epoch) if epoch > now.timestamp() => UnderAttackModeOutput {
            enabled: true,
            state: UnderAttackState::ActiveTimed,
            until_epoch: Some(epoch),
            until: epoch_time_rfc3339(epoch),
            remaining_seconds: Some(epoch - now.timestamp()),
        },
        Some(epoch) => UnderAttackModeOutput {
            enabled: false,
            state: UnderAttackState::Expired,
            until_epoch: Some(epoch),
            until: epoch_time_rfc3339(epoch),
            remaining_seconds: None,
        },
    }
}

fn ensure_available(domains: &queries::domains::DomainsDomains) -> Result<()> {
    if has_public_domain(domains) {
        Ok(())
    } else {
        bail!(
            "Under Attack Mode requires an applied public domain. Add a domain with `railway domain` first."
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

fn print_status(status: &WafStatusOutput) {
    println!("{}", "WAF".bold());
    println!();
    print_field("Service:", &status.service.name.green().bold());
    print_field("Environment:", &status.environment.name.blue().bold());

    if !status.waf.available {
        print_field("Status:", &"unavailable".yellow().bold());
        print_field(
            "Message:",
            &"Add a public domain with `railway domain` before enabling Under Attack Mode.",
        );
        return;
    }

    let under_attack_mode = &status.waf.under_attack_mode;
    print_field("Status:", &status_label(under_attack_mode.enabled));
    print_field("Under Attack Mode:", &state_label(under_attack_mode.state));

    match under_attack_mode.state {
        UnderAttackState::ActiveForever => {
            print_field("Duration:", &"until turned off");
        }
        UnderAttackState::ActiveTimed => {
            if let Some(until) = &under_attack_mode.until {
                print_field("Expires:", until);
            }
            if let Some(remaining_seconds) = under_attack_mode.remaining_seconds {
                let remaining = remaining_label(remaining_seconds);
                print_field("Time remaining:", &remaining);
            }
        }
        UnderAttackState::Expired => {
            if let Some(until) = &under_attack_mode.until {
                print_field("Expired:", until);
            }
        }
        UnderAttackState::Disabled => {}
    }

    for warning in &status.waf.warnings {
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

fn state_label(state: UnderAttackState) -> ColoredString {
    match state {
        UnderAttackState::Disabled => "disabled".yellow(),
        UnderAttackState::ActiveForever => "active until turned off".green(),
        UnderAttackState::ActiveTimed => "active".green(),
        UnderAttackState::Expired => "expired".yellow(),
    }
}

fn enum_name<T: fmt::Debug>(value: &T) -> String {
    format!("{value:?}")
}

fn epoch_time(epoch: i64) -> Option<DateTime<Utc>> {
    if epoch <= 0 {
        return None;
    }
    Utc.timestamp_opt(epoch, 0).single()
}

fn epoch_time_rfc3339(epoch: i64) -> Option<String> {
    epoch_time(epoch).map(|time| time.to_rfc3339())
}

fn remaining_label(seconds: i64) -> String {
    let total_minutes = std::cmp::max(1, (seconds + 30) / 60);
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;

    if hours > 0 && minutes > 0 {
        format!("{hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h")
    } else {
        format!("{minutes}m")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnderAttackDuration {
    Forever,
    OneHour,
    ThreeHours,
    TwelveHours,
    TwentyFourHours,
}

impl UnderAttackDuration {
    fn seconds(self) -> Option<i64> {
        match self {
            Self::Forever => None,
            Self::OneHour => Some(3_600),
            Self::ThreeHours => Some(10_800),
            Self::TwelveHours => Some(43_200),
            Self::TwentyFourHours => Some(86_400),
        }
    }
}

fn parse_duration(value: &str) -> std::result::Result<UnderAttackDuration, String> {
    match value.to_ascii_lowercase().as_str() {
        "forever" | "until-turned-off" | "0" => Ok(UnderAttackDuration::Forever),
        "1h" | "3600" => Ok(UnderAttackDuration::OneHour),
        "3h" | "10800" => Ok(UnderAttackDuration::ThreeHours),
        "12h" | "43200" => Ok(UnderAttackDuration::TwelveHours),
        "24h" | "86400" => Ok(UnderAttackDuration::TwentyFourHours),
        _ => Err(
            "must be one of forever, 1h, 3h, 12h, 24h, 0, 3600, 10800, 43200, or 86400".to_string(),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceRef {
    id: String,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WafStatusOutput {
    service: ResourceRef,
    environment: ResourceRef,
    waf: WafOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WafOutput {
    available: bool,
    under_attack_mode: UnderAttackModeOutput,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<WarningOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct UnderAttackModeOutput {
    enabled: bool,
    state: UnderAttackState,
    until_epoch: Option<i64>,
    until: Option<String>,
    remaining_seconds: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum UnderAttackState {
    Disabled,
    ActiveForever,
    ActiveTimed,
    Expired,
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
    duration_seconds: Option<i64>,
    #[serde(flatten)]
    status: WafStatusOutput,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_under_attack_subcommands_and_selectors() {
        assert!(matches!(
            Args::parse_from(["waf", "under-attack", "status"]).command,
            Commands::UnderAttack(_)
        ));
        let args = Args::parse_from([
            "waf",
            "--project",
            "project-id",
            "--environment",
            "production",
            "--service",
            "web",
            "--json",
            "under-attack",
            "status",
        ]);
        assert_eq!(args.project.as_deref(), Some("project-id"));
        assert_eq!(args.environment.as_deref(), Some("production"));
        assert_eq!(args.service.as_deref(), Some("web"));
        assert!(args.json);
    }

    #[test]
    fn parses_duration_values() {
        let args = Args::parse_from(["waf", "under-attack", "enable"]);
        let Commands::UnderAttack(under_attack) = args.command;
        let UnderAttackCommands::Enable(enable) = under_attack.command else {
            panic!("expected enable");
        };
        assert_eq!(enable.duration, UnderAttackDuration::Forever);

        assert_eq!(
            parse_duration("forever").unwrap(),
            UnderAttackDuration::Forever
        );
        assert_eq!(
            parse_duration("until-turned-off").unwrap(),
            UnderAttackDuration::Forever
        );
        assert_eq!(parse_duration("1h").unwrap(), UnderAttackDuration::OneHour);
        assert_eq!(
            parse_duration("3h").unwrap(),
            UnderAttackDuration::ThreeHours
        );
        assert_eq!(
            parse_duration("12h").unwrap(),
            UnderAttackDuration::TwelveHours
        );
        assert_eq!(
            parse_duration("24h").unwrap(),
            UnderAttackDuration::TwentyFourHours
        );
        assert_eq!(parse_duration("86400").unwrap().seconds(), Some(86_400));
        assert!(parse_duration("2h").is_err());
    }

    #[test]
    fn under_attack_mode_output_handles_states() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();

        let disabled = under_attack_mode_output(None, now);
        assert!(!disabled.enabled);
        assert_eq!(disabled.state, UnderAttackState::Disabled);

        let forever = under_attack_mode_output(Some(0), now);
        assert!(forever.enabled);
        assert_eq!(forever.state, UnderAttackState::ActiveForever);
        assert_eq!(forever.until_epoch, Some(0));

        let timed = under_attack_mode_output(Some(1_700_003_600), now);
        assert!(timed.enabled);
        assert_eq!(timed.state, UnderAttackState::ActiveTimed);
        assert_eq!(timed.remaining_seconds, Some(3_600));

        let expired = under_attack_mode_output(Some(1_699_999_999), now);
        assert!(!expired.enabled);
        assert_eq!(expired.state, UnderAttackState::Expired);
    }

    #[test]
    fn status_output_uses_camel_case_and_null_times() {
        let output = WafStatusOutput {
            service: ResourceRef {
                id: "svc_123".to_string(),
                name: "web".to_string(),
            },
            environment: ResourceRef {
                id: "env_123".to_string(),
                name: "production".to_string(),
            },
            waf: WafOutput {
                available: true,
                under_attack_mode: UnderAttackModeOutput {
                    enabled: false,
                    state: UnderAttackState::Disabled,
                    until_epoch: None,
                    until: None,
                    remaining_seconds: None,
                },
                warnings: Vec::new(),
            },
        };
        let value = serde_json::to_value(output).unwrap();
        assert_eq!(value["waf"]["available"], true);
        assert_eq!(value["waf"]["underAttackMode"]["enabled"], false);
        assert_eq!(value["waf"]["underAttackMode"]["state"], "disabled");
        assert!(value["waf"]["underAttackMode"]["untilEpoch"].is_null());
        assert!(value["waf"].get("warnings").is_none());
    }

    #[test]
    fn action_output_flattens_status() {
        let output = ActionOutput {
            action: "enable",
            duration_seconds: Some(3_600),
            status: WafStatusOutput {
                service: ResourceRef {
                    id: "svc_123".to_string(),
                    name: "web".to_string(),
                },
                environment: ResourceRef {
                    id: "env_123".to_string(),
                    name: "production".to_string(),
                },
                waf: WafOutput {
                    available: true,
                    under_attack_mode: UnderAttackModeOutput {
                        enabled: true,
                        state: UnderAttackState::ActiveTimed,
                        until_epoch: Some(1_700_003_600),
                        until: Some("2023-11-14T23:13:20+00:00".to_string()),
                        remaining_seconds: Some(3_600),
                    },
                    warnings: Vec::new(),
                },
            },
        };
        let value = serde_json::to_value(output).unwrap();
        assert_eq!(value["action"], "enable");
        assert_eq!(value["durationSeconds"], 3_600);
        assert_eq!(value["service"]["name"], "web");
        assert_eq!(value["waf"]["underAttackMode"]["enabled"], true);
    }

    #[test]
    fn disable_action_output_can_report_unavailable_waf() {
        let output = ActionOutput {
            action: "disable",
            duration_seconds: None,
            status: WafStatusOutput {
                service: ResourceRef {
                    id: "svc_123".to_string(),
                    name: "web".to_string(),
                },
                environment: ResourceRef {
                    id: "env_123".to_string(),
                    name: "production".to_string(),
                },
                waf: WafOutput {
                    available: false,
                    under_attack_mode: UnderAttackModeOutput {
                        enabled: false,
                        state: UnderAttackState::Disabled,
                        until_epoch: None,
                        until: None,
                        remaining_seconds: None,
                    },
                    warnings: Vec::new(),
                },
            },
        };
        let value = serde_json::to_value(output).unwrap();
        assert_eq!(value["action"], "disable");
        assert!(value.get("durationSeconds").is_none());
        assert_eq!(value["waf"]["available"], false);
        assert_eq!(value["waf"]["underAttackMode"]["state"], "disabled");
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
    fn remaining_label_formats_like_dashboard_countdown() {
        assert_eq!(remaining_label(60), "1m");
        assert_eq!(remaining_label(3_600), "1h");
        assert_eq!(remaining_label(9_660), "2h 41m");
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
