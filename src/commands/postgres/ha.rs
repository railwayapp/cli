//! `railway postgres ha` -- high-availability Postgres clustering.

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;
use serde::Serialize;

use crate::controllers::{
    config::{EnvironmentConfig, fetch_environment_config},
    postgres_plugins::{self, PitrState},
    project::{ServiceContext, resolve_service_context},
    template_apply::{
        self, ApplyKind, ApplyTemplateParams, HA_TEMPLATE_CODE, RevertTemplateParams,
    },
};

use super::{
    ResourceRef, confirm_or_bail, not_yet_implemented, print_field, resolve_root, service_name_map,
    status_label,
};

/// Manage high-availability clustering for Postgres
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway postgres ha status --service postgres\n  railway postgres ha convert --service postgres --replicas 2\n  railway postgres ha convert --service postgres --replicas 2 --coordinators 3 --edge 1\n  railway postgres ha revert --service postgres --yes\n  railway postgres ha scale --service postgres --replicas 3\n  railway postgres ha switchover --service postgres --to postgres-replica-1\n\nAutomation notes:\n  Omitted --replicas/--coordinators/--edge on `convert` leave the template's authored count untouched.\n  --coordinators must be an odd number (consensus quorum)."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser)]
enum Commands {
    /// Show HA cluster status
    Status,

    /// Convert a standalone Postgres service into an HA cluster
    Convert(ConvertArgs),

    /// Revert an HA cluster back to standalone Postgres
    Revert(RevertArgs),

    /// Scale cluster replicas, coordinators, or edge nodes
    Scale(ScaleArgs),

    /// Promote a replica to leader (brief downtime)
    #[clap(visible_alias = "promote")]
    Switchover(SwitchoverArgs),
}

#[derive(Parser)]
struct ConvertArgs {
    /// Number of replicas (excluding the primary); omit to keep the template default
    #[clap(long)]
    replicas: Option<i64>,

    /// Number of coordinator/consensus nodes (e.g. etcd); must be odd; omit to keep the template default
    #[clap(long)]
    coordinators: Option<i64>,

    /// Number of edge/load-balancer replicas (e.g. HAProxy); omit to keep the template default
    #[clap(long)]
    edge: Option<i64>,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Stage the change without deploying it immediately
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct RevertArgs {
    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Stage the change without deploying it immediately
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
#[clap(group(
    clap::ArgGroup::new("target")
        .args(["replicas", "coordinators", "edge"])
        .required(true)
        .multiple(true)
))]
struct ScaleArgs {
    /// Target replica count
    #[clap(long)]
    replicas: Option<i64>,

    /// Target coordinator/consensus node count (must stay odd)
    #[clap(long)]
    coordinators: Option<i64>,

    /// Target edge/load-balancer replica count
    #[clap(long)]
    edge: Option<i64>,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Stage the change without deploying it immediately
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct SwitchoverArgs {
    /// Service name or ID of the replica to promote
    #[clap(long)]
    to: String,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,
}

pub async fn command(
    args: Args,
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    match args.command {
        Commands::Status => status(project, service, environment, json).await,
        Commands::Convert(a) => convert(project, service, environment, json, a).await,
        Commands::Revert(a) => revert(project, service, environment, json, a).await,
        Commands::Scale(_) => not_yet_implemented("ha scale"),
        Commands::Switchover(_) => not_yet_implemented("ha switchover"),
    }
}

async fn status(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    print_status(&ctx, &config, json)
}

fn print_status(ctx: &ServiceContext, config: &EnvironmentConfig, json: bool) -> Result<()> {
    let root = resolve_root(ctx, config);
    let names = service_name_map(ctx);
    let ha_state = postgres_plugins::compute_ha_state(config, &root.root_id, &names);

    let members: Vec<HaMemberOutput> = ha_state
        .members
        .iter()
        .map(|m| HaMemberOutput {
            service: ResourceRef {
                id: m.service_id.clone(),
                name: m.service_name.clone(),
            },
            cluster_role: m.cluster_role.clone(),
        })
        .collect();

    let output = HaStatusOutput {
        service: ResourceRef {
            id: ctx.service_id.clone(),
            name: ctx.service_name.clone(),
        },
        environment: ResourceRef {
            id: ctx.environment_id.clone(),
            name: ctx.environment_name.clone(),
        },
        root: ResourceRef {
            id: root.root_id.clone(),
            name: root.root_name.clone(),
        },
        is_cluster: ha_state.is_cluster,
        members,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_ha_status(&output);
    }
    Ok(())
}

fn print_ha_status(output: &HaStatusOutput) {
    println!("{}", "High availability".bold());
    println!();
    print_field("Service:", &output.service.name.green().bold());
    print_field("Environment:", &output.environment.name.blue().bold());
    if output.root.id != output.service.id {
        print_field("Cluster root:", &output.root.name);
    }
    print_field("Status:", &status_label(output.is_cluster));

    if output.is_cluster {
        println!();
        println!(
            "{}",
            "Members (live role/lag not yet available in this CLI version):".bold()
        );
        for member in &output.members {
            println!(
                "  {:<28} {}",
                member.service.name,
                member.cluster_role.as_deref().unwrap_or("-")
            );
        }
    }
}

fn guardrail_blockers(state: &PitrState) -> Vec<String> {
    let mut blockers = Vec::new();
    if state.unsupported_image {
        blockers.push(
            "Image is not an official Railway Postgres image -- HA conversion is not supported."
                .to_string(),
        );
    }
    if state.minor_pinned {
        blockers.push(
            "Image is pinned to a minor version -- unpin to the major tag (e.g. `:16`) before converting to HA."
                .to_string(),
        );
    }
    if state.has_start_command {
        blockers.push(
            "A custom start command overrides the Postgres entrypoint -- clear it before converting to HA."
                .to_string(),
        );
    }
    blockers
}

async fn convert(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: ConvertArgs,
) -> Result<()> {
    if let Some(coordinators) = args.coordinators
        && coordinators % 2 == 0
    {
        bail!("--coordinators must be an odd number for consensus quorum (got {coordinators})");
    }

    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = postgres_plugins::compute_ha_state(&config, &root.root_id, &names);

    if ha_state.is_cluster {
        bail!("{} is already an HA cluster.", root.root_name);
    }

    let target_service = config.services.get(&root.root_id).with_context(|| {
        format!(
            "Service \"{}\" not found in environment config",
            root.root_name
        )
    })?;
    let blockers = guardrail_blockers(&postgres_plugins::compute_pitr_state(target_service));
    if !blockers.is_empty() {
        bail!(
            "Cannot convert {} to HA:\n  - {}",
            root.root_name,
            blockers.join("\n  - ")
        );
    }

    if !confirm_or_bail(
        &format!(
            "Convert {} to an HA cluster? Connection endpoints will change and active connections will drop.",
            root.root_name.yellow()
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    let volume_instance_id = target_service.volume_mounts.keys().next().cloned();
    let result = template_apply::apply_composable_template(
        &ctx,
        ApplyTemplateParams {
            template_code: HA_TEMPLATE_CODE.to_string(),
            service_id: root.root_id.clone(),
            volume_instance_id,
            replica_count: args.replicas,
            internal_count: args.coordinators,
            edge_count: args.edge,
            edge_variables: None,
            kind: ApplyKind::Conversion,
            auto_deploy: !args.no_deploy,
        },
    )
    .await
    .context("Failed to convert to HA")?;

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    if !json {
        let verb = if result.deployed {
            "Converted and deployed"
        } else {
            "Staged the conversion of"
        };
        println!(
            "{verb} {} to an HA cluster in environment {} (project {}).",
            root.root_name.bold(),
            ctx.environment_name.bold(),
            result.project_id
        );
    }
    print_status(&ctx, &config, json)
}

async fn revert(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: RevertArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = postgres_plugins::compute_ha_state(&config, &root.root_id, &names);

    if !ha_state.is_cluster {
        bail!("{} is not an HA cluster.", root.root_name);
    }

    if !confirm_or_bail(
        &format!(
            "Revert {} to standalone Postgres? Connection endpoints will change and active connections will drop.",
            root.root_name.red()
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    let result = template_apply::revert_template(
        &ctx,
        RevertTemplateParams {
            template_code: HA_TEMPLATE_CODE.to_string(),
            root_service_id: root.root_id.clone(),
            auto_deploy: !args.no_deploy,
        },
    )
    .await
    .context("Failed to revert HA cluster")?;

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    if !json {
        let verb = if result.deployed {
            "Reverted and deployed"
        } else {
            "Staged the revert of"
        };
        println!(
            "{verb} {} to standalone Postgres in environment {} (project {}).",
            root.root_name.bold(),
            ctx.environment_name.bold(),
            result.project_id
        );
    }
    print_status(&ctx, &config, json)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HaMemberOutput {
    service: ResourceRef,
    cluster_role: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HaStatusOutput {
    service: ResourceRef,
    environment: ResourceRef,
    root: ResourceRef,
    is_cluster: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    members: Vec<HaMemberOutput>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_top_level_verbs() {
        assert!(matches!(
            Args::parse_from(["ha", "status"]).command,
            Commands::Status
        ));
        assert!(matches!(
            Args::parse_from(["ha", "convert"]).command,
            Commands::Convert(_)
        ));
        assert!(matches!(
            Args::parse_from(["ha", "revert", "--yes"]).command,
            Commands::Revert(RevertArgs {
                yes: true,
                no_deploy: false
            })
        ));
    }

    #[test]
    fn parses_convert_counts() {
        let args = Args::parse_from([
            "ha",
            "convert",
            "--replicas",
            "2",
            "--coordinators",
            "3",
            "--edge",
            "1",
        ]);
        let Commands::Convert(convert) = args.command else {
            panic!("expected convert");
        };
        assert_eq!(convert.replicas, Some(2));
        assert_eq!(convert.coordinators, Some(3));
        assert_eq!(convert.edge, Some(1));
    }

    #[test]
    fn scale_requires_at_least_one_target() {
        assert!(Args::try_parse_from(["ha", "scale"]).is_err());
        let args = Args::parse_from(["ha", "scale", "--replicas", "3"]);
        assert!(matches!(
            args.command,
            Commands::Scale(ScaleArgs {
                replicas: Some(3),
                ..
            })
        ));
    }

    #[test]
    fn switchover_accepts_promote_alias_and_requires_to() {
        assert!(Args::try_parse_from(["ha", "switchover"]).is_err());
        let args = Args::parse_from(["ha", "switchover", "--to", "postgres-replica-1"]);
        let Commands::Switchover(switchover) = args.command else {
            panic!("expected switchover");
        };
        assert_eq!(switchover.to, "postgres-replica-1");

        let args = Args::parse_from(["ha", "promote", "--to", "postgres-replica-1"]);
        assert!(matches!(args.command, Commands::Switchover(_)));
    }

    #[test]
    fn guardrail_blockers_lists_every_failing_check() {
        let state = PitrState {
            enabled: false,
            bucket_wired: false,
            minor_pinned: false,
            unsupported_image: true,
            has_start_command: true,
        };
        let blockers = guardrail_blockers(&state);
        assert_eq!(blockers.len(), 2);
    }
}
