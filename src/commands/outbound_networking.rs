use colored::ColoredString;
use serde::Serialize;

use crate::commands::output::fields::{print_field, print_service_environment_context};
use crate::controllers::{
    outbound_networking::{self, FeatureAction, Ipv6Status, StaticIpStatus},
    project::{ServiceContext, resolve_service_context},
};

use super::*;

const FIELD_LABEL_WIDTH: usize = 20;
const STAGED_IPV6_MESSAGE: &str =
    "Commit staged environment changes to trigger redeploy: railway environment edit";

/// Manage outbound networking for a service
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway outbound-network status --service api\n  railway outbound-network static-ip enable --service api\n  railway outbound-network static-ip status --service api --json\n  railway outbound-network static-ip disable --service api\n  railway outbound-network ipv6 enable --service api\n  railway outbound-network ipv6 status --service api --json\n  railway outbound-network ipv6 disable --service api\n\nAutomation notes:\n  Static Outbound IP changes require a redeploy before outbound traffic changes.\n  Outbound IPv6 changes are staged; commit them with `railway environment edit` to trigger redeploy."
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
    /// Show outbound networking status
    Status,

    /// Manage Static Outbound IPs
    #[clap(name = "static-ip")]
    StaticIp(StaticIpArgs),

    /// Manage Outbound IPv6
    Ipv6(Ipv6Args),
}

#[derive(Parser)]
struct StaticIpArgs {
    #[clap(subcommand)]
    command: StaticIpCommands,
}

#[derive(Parser)]
enum StaticIpCommands {
    /// Show Static Outbound IP status
    Status,

    /// Enable Static Outbound IPs
    Enable,

    /// Disable Static Outbound IPs
    Disable,
}

#[derive(Parser)]
struct Ipv6Args {
    #[clap(subcommand)]
    command: Ipv6Commands,
}

#[derive(Parser)]
enum Ipv6Commands {
    /// Show Outbound IPv6 status
    Status,

    /// Stage enabling Outbound IPv6
    Enable,

    /// Stage disabling Outbound IPv6
    Disable,
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
        Commands::StaticIp(args) => match args.command {
            StaticIpCommands::Status => {
                static_ip_status(project, service, environment, json).await?
            }
            StaticIpCommands::Enable => {
                static_ip_enable(project, service, environment, json).await?
            }
            StaticIpCommands::Disable => {
                static_ip_disable(project, service, environment, json).await?
            }
        },
        Commands::Ipv6(args) => match args.command {
            Ipv6Commands::Status => ipv6_status(project, service, environment, json).await?,
            Ipv6Commands::Enable => ipv6_stage(project, service, environment, true, json).await?,
            Ipv6Commands::Disable => ipv6_stage(project, service, environment, false, json).await?,
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
    let (static_ip, ipv6) = tokio::try_join!(
        outbound_networking::fetch_static_ip_status(
            &ctx.client,
            &ctx.configs,
            &ctx.environment_id,
            &ctx.service_id,
        ),
        outbound_networking::fetch_ipv6_status(
            &ctx.client,
            &ctx.configs,
            &ctx.environment_id,
            &ctx.service_id,
        )
    )?;
    let output = OutboundNetworkingOutput::new(&ctx, static_ip, ipv6);

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_outbound_networking_status(&output);
    }

    Ok(())
}

async fn static_ip_status(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let static_ip = outbound_networking::fetch_static_ip_status(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
    )
    .await?;
    let output = StaticIpStatusOutput::new(&ctx, static_ip);

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_static_ip_status(&output);
    }

    Ok(())
}

async fn static_ip_enable(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let (static_ip, action) = outbound_networking::enable_static_ips(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
    )
    .await?;
    let output = StaticIpActionOutput::new(&ctx, action, static_ip);

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_static_ip_action(&output);
    }

    Ok(())
}

async fn static_ip_disable(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let (static_ip, action) = outbound_networking::disable_static_ips(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
    )
    .await?;
    let output = StaticIpActionOutput::new(&ctx, action, static_ip);

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_static_ip_action(&output);
    }

    Ok(())
}

async fn ipv6_status(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let ipv6 = outbound_networking::fetch_ipv6_status(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
    )
    .await?;
    let output = Ipv6StatusOutput::new(&ctx, ipv6);

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_ipv6_status(&output);
    }

    Ok(())
}

async fn ipv6_stage(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    value: bool,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let (ipv6, action) = outbound_networking::stage_ipv6(
        &ctx.client,
        &ctx.configs,
        &ctx.environment_id,
        &ctx.service_id,
        value,
    )
    .await?;
    let output = Ipv6ActionOutput::new(&ctx, action, ipv6);

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_ipv6_action(&output);
    }

    Ok(())
}

fn print_outbound_networking_status(output: &OutboundNetworkingOutput) {
    println!("{}", "Outbound networking".bold());
    println!();
    print_service_environment_context(
        &output.service.name,
        &output.environment.name,
        FIELD_LABEL_WIDTH,
    );
    println!();
    print_static_ip_fields(&output.static_ip);
    println!();
    print_ipv6_fields(&output.ipv6);
}

fn print_static_ip_status(output: &StaticIpStatusOutput) {
    println!("{}", "Static Outbound IPs".bold());
    println!();
    print_service_environment_context(
        &output.service.name,
        &output.environment.name,
        FIELD_LABEL_WIDTH,
    );
    println!();
    print_static_ip_fields(&output.static_ip);
}

fn print_ipv6_status(output: &Ipv6StatusOutput) {
    println!("{}", "Outbound IPv6".bold());
    println!();
    print_service_environment_context(
        &output.service.name,
        &output.environment.name,
        FIELD_LABEL_WIDTH,
    );
    println!();
    print_ipv6_fields(&output.ipv6);
}

fn print_static_ip_action(output: &StaticIpActionOutput) {
    if !output.changed {
        println!(
            "Static Outbound IPs are already {} for service {} in environment {}.",
            if output.enabled {
                "enabled"
            } else {
                "disabled"
            },
            output.service.name.bold(),
            output.environment.name.bold()
        );
        return;
    }

    println!(
        "{} Static Outbound IPs for service {} in environment {}.",
        past_tense(output.action.action),
        output.service.name.bold(),
        output.environment.name.bold()
    );
    if output.lifecycle.redeploy_required {
        println!(
            "Redeploy required before outbound traffic {} these IPs.",
            if output.enabled {
                "uses"
            } else {
                "stops using"
            }
        );
    }
    if output.enabled {
        println!();
        print_static_ip_table(&output.static_ip);
    }
}

fn print_ipv6_action(output: &Ipv6ActionOutput) {
    if !output.changed {
        if output.ipv6.pending_value == Some(output.enabled) {
            println!(
                "Outbound IPv6 is already staged to be {} for service {} in environment {}.",
                if output.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                output.service.name.bold(),
                output.environment.name.bold()
            );
        } else {
            println!(
                "Outbound IPv6 is already {} for service {} in environment {}.",
                if output.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                output.service.name.bold(),
                output.environment.name.bold()
            );
        }
        return;
    }

    if output.lifecycle.staged {
        if output.enabled {
            println!(
                "Staged Outbound IPv6 for service {} in environment {}.",
                output.service.name.bold(),
                output.environment.name.bold()
            );
        } else {
            println!(
                "Staged disabling Outbound IPv6 for service {} in environment {}.",
                output.service.name.bold(),
                output.environment.name.bold()
            );
        }
        println!("{STAGED_IPV6_MESSAGE}");
    } else {
        println!(
            "Cleared staged Outbound IPv6 change for service {} in environment {}.",
            output.service.name.bold(),
            output.environment.name.bold()
        );
    }
}

fn print_static_ip_fields(static_ip: &StaticIpStatus) {
    print_field(
        "Static IPs:",
        &status_label(static_ip.enabled),
        FIELD_LABEL_WIDTH,
    );
    if static_ip.enabled && static_ip.high_availability {
        print_field(
            "High Availability:",
            &"enabled".green().bold(),
            FIELD_LABEL_WIDTH,
        );
    }
    if static_ip.enabled {
        println!();
        print_static_ip_table(static_ip);
    }
}

fn print_ipv6_fields(ipv6: &Ipv6Status) {
    print_field(
        "Outbound IPv6:",
        &status_label(ipv6.enabled),
        FIELD_LABEL_WIDTH,
    );
    if let Some(value) = ipv6.pending_value {
        print_field(
            "Pending:",
            &(if value { "enable" } else { "disable" })
                .to_string()
                .yellow(),
            FIELD_LABEL_WIDTH,
        );
        print_field("Message:", &STAGED_IPV6_MESSAGE, FIELD_LABEL_WIDTH);
    }
}

fn print_static_ip_table(static_ip: &StaticIpStatus) {
    if static_ip.ips.is_empty() {
        return;
    }

    let ip_width = static_ip
        .ips
        .iter()
        .map(|ip| ip.ipv4.len())
        .max()
        .unwrap_or("IP Address".len())
        .max("IP Address".len())
        + 3;
    let region_width = static_ip
        .ips
        .iter()
        .map(|ip| ip.region.len())
        .max()
        .unwrap_or("Region".len())
        .max("Region".len())
        + 3;

    println!(
        "{:<ip_width$}{:<region_width$}Type",
        "IP Address".bold(),
        "Region".bold(),
        ip_width = ip_width,
        region_width = region_width
    );
    for ip in &static_ip.ips {
        println!(
            "{:<ip_width$}{:<region_width$}Shared",
            ip.ipv4,
            ip.region,
            ip_width = ip_width,
            region_width = region_width
        );
    }
}

fn status_label(enabled: bool) -> ColoredString {
    if enabled {
        "enabled".green().bold()
    } else {
        "disabled".yellow().bold()
    }
}

fn past_tense(action: &str) -> &'static str {
    match action {
        "enable" => "Enabled",
        "disable" => "Disabled",
        _ => "Updated",
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceRef {
    id: String,
    name: String,
}

impl ResourceRef {
    fn service(ctx: &ServiceContext) -> Self {
        Self {
            id: ctx.service_id.clone(),
            name: ctx.service_name.clone(),
        }
    }

    fn environment(ctx: &ServiceContext) -> Self {
        Self {
            id: ctx.environment_id.clone(),
            name: ctx.environment_name.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutboundNetworkingOutput {
    service: ResourceRef,
    environment: ResourceRef,
    static_ip: StaticIpStatus,
    ipv6: Ipv6Status,
}

impl OutboundNetworkingOutput {
    fn new(ctx: &ServiceContext, static_ip: StaticIpStatus, ipv6: Ipv6Status) -> Self {
        Self {
            service: ResourceRef::service(ctx),
            environment: ResourceRef::environment(ctx),
            static_ip,
            ipv6,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StaticIpStatusOutput {
    service: ResourceRef,
    environment: ResourceRef,
    static_ip: StaticIpStatus,
}

impl StaticIpStatusOutput {
    fn new(ctx: &ServiceContext, static_ip: StaticIpStatus) -> Self {
        Self {
            service: ResourceRef::service(ctx),
            environment: ResourceRef::environment(ctx),
            static_ip,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Ipv6StatusOutput {
    service: ResourceRef,
    environment: ResourceRef,
    ipv6: Ipv6Status,
}

impl Ipv6StatusOutput {
    fn new(ctx: &ServiceContext, ipv6: Ipv6Status) -> Self {
        Self {
            service: ResourceRef::service(ctx),
            environment: ResourceRef::environment(ctx),
            ipv6,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StaticIpActionOutput {
    service: ResourceRef,
    environment: ResourceRef,
    #[serde(flatten)]
    action: FeatureAction,
    static_ip: StaticIpStatus,
}

impl StaticIpActionOutput {
    fn new(ctx: &ServiceContext, action: FeatureAction, static_ip: StaticIpStatus) -> Self {
        Self {
            service: ResourceRef::service(ctx),
            environment: ResourceRef::environment(ctx),
            action,
            static_ip,
        }
    }
}

impl std::ops::Deref for StaticIpActionOutput {
    type Target = FeatureAction;

    fn deref(&self) -> &Self::Target {
        &self.action
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Ipv6ActionOutput {
    service: ResourceRef,
    environment: ResourceRef,
    #[serde(flatten)]
    action: FeatureAction,
    ipv6: Ipv6Status,
}

impl Ipv6ActionOutput {
    fn new(ctx: &ServiceContext, action: FeatureAction, ipv6: Ipv6Status) -> Self {
        Self {
            service: ResourceRef::service(ctx),
            environment: ResourceRef::environment(ctx),
            action,
            ipv6,
        }
    }
}

impl std::ops::Deref for Ipv6ActionOutput {
    type Target = FeatureAction;

    fn deref(&self) -> &Self::Target {
        &self.action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controllers::outbound_networking::Lifecycle;
    use clap::Parser;

    #[test]
    fn parses_subcommands() {
        assert!(matches!(
            Args::parse_from(["outbound-network", "status"]).command,
            Commands::Status
        ));
        assert!(matches!(
            Args::parse_from(["outbound-network", "static-ip", "status"]).command,
            Commands::StaticIp(StaticIpArgs {
                command: StaticIpCommands::Status
            })
        ));
        assert!(matches!(
            Args::parse_from(["outbound-network", "static-ip", "enable"]).command,
            Commands::StaticIp(StaticIpArgs {
                command: StaticIpCommands::Enable
            })
        ));
        assert!(matches!(
            Args::parse_from(["outbound-network", "static-ip", "disable"]).command,
            Commands::StaticIp(StaticIpArgs {
                command: StaticIpCommands::Disable
            })
        ));
        assert!(matches!(
            Args::parse_from(["outbound-network", "ipv6", "enable"]).command,
            Commands::Ipv6(Ipv6Args {
                command: Ipv6Commands::Enable
            })
        ));
        assert!(matches!(
            Args::parse_from([
                "outbound-network",
                "--service",
                "api",
                "--environment",
                "production",
                "--project",
                "project-id",
                "--json",
                "ipv6",
                "disable"
            ])
            .command,
            Commands::Ipv6(Ipv6Args {
                command: Ipv6Commands::Disable
            })
        ));
    }

    #[test]
    fn action_output_has_lifecycle_shape() {
        let output = serde_json::json!({
            "feature": "staticIp",
            "action": "enable",
            "enabled": true,
            "changed": true,
            "lifecycle": Lifecycle::direct(true),
        });

        assert_eq!(output["lifecycle"]["mode"], "direct");
        assert_eq!(output["lifecycle"]["staged"], false);
        assert_eq!(output["lifecycle"]["committed"], true);
        assert_eq!(output["lifecycle"]["redeployRequired"], true);
        assert_eq!(output["lifecycle"]["redeployTriggered"], false);

        let output = serde_json::json!({
            "feature": "ipv6",
            "action": "enable",
            "enabled": true,
            "changed": true,
            "lifecycle": Lifecycle::environment_patch(true),
        });

        assert_eq!(output["lifecycle"]["mode"], "environmentPatch");
        assert_eq!(output["lifecycle"]["staged"], true);
        assert_eq!(output["lifecycle"]["committed"], false);
        assert_eq!(output["lifecycle"]["redeployRequired"], false);
        assert_eq!(output["lifecycle"]["redeployTriggered"], false);
    }
}
