//! `railway postgres pitr` -- point-in-time recovery / continuous backups.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::Parser;
use colored::Colorize;
use serde::Serialize;
use tokio::time::{sleep, timeout};

use crate::{
    client::post_graphql,
    commands::ssh::get_service_instance_id,
    controllers::{
        config::{EnvironmentConfig, fetch_environment_config},
        database::DatabaseType,
        db_stats::{diagnose_db_stats_failure, preflight_db_stats_ssh},
        exec::exec_in_container,
        postgres_plugins::{self, PitrState},
        project::{ServiceContext, resolve_service_context},
        template_apply::{
            self, ApplyKind, ApplyTemplateParams, PITR_TEMPLATE_CODE, RevertTemplateParams,
        },
    },
    gql::{mutations, queries},
    util::time::parse_time,
};

use super::{
    ResourceRef, confirm_or_bail, print_field, resolve_root, service_name_map, status_label, yes_no,
};

/// Manage point-in-time recovery (continuous backups) for Postgres
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway postgres pitr status --service postgres\n  railway postgres pitr enable --service postgres\n  railway postgres pitr disable --service postgres --yes\n  railway postgres pitr restore --service postgres --at 2026-07-20T12:00:00Z\n  railway postgres pitr backup create --service postgres --name pre-migration\n  railway postgres pitr schedule set --daily --weekly\n\nAutomation notes:\n  <time> for `restore` accepts RFC3339 (2026-07-20T12:00:00Z), `YYYY-MM-DD HH:MM:SS`/`YYYY-MM-DD HH:MM` (interpreted in your local timezone), or a relative offset back from now (30m, 2h, 1d, 1w).\n  `enable`/`disable` auto-detect whether the target is a standalone Postgres or the root of an HA cluster.\n  `progress`/`cancel`/`clear` only apply to HA clusters (the rolling enable/disable workflow).\n  `status`'s coverage/archiver section is a best-effort live probe over SSH into the running container; it degrades to \"unavailable\" instead of failing the command if the service isn't reachable."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser)]
enum Commands {
    /// Show PITR status
    Status,

    /// Enable point-in-time recovery
    Enable(EnableArgs),

    /// Disable point-in-time recovery
    Disable(DisableArgs),

    /// Show HA PITR enable/disable workflow progress (HA clusters only)
    Progress(ProgressArgs),

    /// Cancel a stuck HA PITR enable/disable workflow (HA clusters only)
    Cancel,

    /// Clear a completed HA PITR workflow's progress snapshot (HA clusters only)
    Clear,

    /// Restore to a point in time into a new service
    Restore(RestoreArgs),

    /// Manage on-demand backups
    Backup(BackupArgs),

    /// Manage the automatic backup schedule
    Schedule(ScheduleArgs),
}

#[derive(Parser)]
struct EnableArgs {
    /// Commit the config change without triggering deploys -- it applies on
    /// the next deploy (standalone only; HA enable always applies live)
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct DisableArgs {
    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Commit the config change without triggering deploys -- it applies on
    /// the next deploy (standalone only; HA disable always applies live)
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct ProgressArgs {
    /// Poll until the workflow reaches a terminal phase
    #[clap(long)]
    watch: bool,
}

#[derive(Parser)]
struct RestoreArgs {
    /// Point in time to restore to: RFC3339 (2026-07-20T12:00:00Z), "YYYY-MM-DD HH:MM:SS"
    /// (local timezone), or a relative offset back from now (30m, 2h, 1d)
    #[clap(long = "at")]
    at: String,

    /// Name for the new restored service (defaults to "<source>-restored-YYYYMMDD-HHMM")
    #[clap(long = "new-service-name")]
    new_service_name: Option<String>,

    /// Restore from a specific archive sub-prefix (multi-history picker)
    #[clap(long = "source-repo-path")]
    source_repo_path: Option<String>,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,
}

#[derive(Parser)]
struct BackupArgs {
    #[clap(subcommand)]
    command: BackupCommands,
}

#[derive(Parser)]
enum BackupCommands {
    /// List backups
    List,

    /// Create an on-demand backup
    Create(BackupCreateArgs),

    /// Delete one or more backups
    Delete(BackupDeleteArgs),

    /// Remove a backup's expiration (keep it indefinitely)
    Lock(BackupIdArgs),

    /// Restore from a backup
    Restore(BackupRestoreArgs),
}

#[derive(Parser)]
struct BackupCreateArgs {
    /// Optional name/label for the backup (defaults to "Manual")
    #[clap(long)]
    name: Option<String>,
}

#[derive(Parser)]
struct BackupDeleteArgs {
    /// Backup ID(s) to delete
    #[clap(required = true)]
    ids: Vec<String>,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,
}

#[derive(Parser)]
struct BackupIdArgs {
    /// Backup ID
    id: String,
}

#[derive(Parser)]
struct BackupRestoreArgs {
    /// Backup ID to restore from
    id: String,

    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,
}

#[derive(Parser)]
struct ScheduleArgs {
    #[clap(subcommand)]
    command: ScheduleCommands,
}

#[derive(Parser)]
enum ScheduleCommands {
    /// Set the automatic backup schedule (any combination)
    Set(ScheduleSetArgs),

    /// List the configured backup schedule(s)
    List,
}

#[derive(Parser)]
#[clap(group(
    clap::ArgGroup::new("kinds")
        .args(["daily", "weekly", "monthly", "none"])
        .required(true)
        .multiple(true)
))]
struct ScheduleSetArgs {
    /// Keep a daily backup
    #[clap(long)]
    daily: bool,

    /// Keep a weekly backup
    #[clap(long)]
    weekly: bool,

    /// Keep a monthly backup
    #[clap(long)]
    monthly: bool,

    /// Remove every automatic backup schedule (existing backups are kept)
    #[clap(long, conflicts_with_all = ["daily", "weekly", "monthly"])]
    none: bool,
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
        Commands::Enable(a) => enable(project, service, environment, json, a).await,
        Commands::Disable(a) => disable(project, service, environment, json, a).await,
        Commands::Progress(a) => progress(project, service, environment, json, a).await,
        Commands::Cancel => cancel(project, service, environment, json).await,
        Commands::Clear => clear(project, service, environment, json).await,
        Commands::Restore(a) => restore(project, service, environment, json, a).await,
        Commands::Backup(a) => match a.command {
            BackupCommands::List => backup_list(project, service, environment, json).await,
            BackupCommands::Create(a) => {
                backup_create(project, service, environment, json, a).await
            }
            BackupCommands::Delete(a) => {
                backup_delete(project, service, environment, json, a).await
            }
            BackupCommands::Lock(a) => backup_lock(project, service, environment, json, a).await,
            BackupCommands::Restore(a) => {
                backup_restore(project, service, environment, json, a).await
            }
        },
        Commands::Schedule(a) => match a.command {
            ScheduleCommands::Set(a) => schedule_set(project, service, environment, json, a).await,
            ScheduleCommands::List => schedule_list(project, service, environment, json).await,
        },
    }
}

async fn status(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    print_status(&ctx, &config, json, true).await
}

/// `include_live == false` skips the SSH coverage/archiver probe -- used
/// right after `enable`/`disable`, where the deployment triggered by that
/// change hasn't rolled out yet, so a live probe would only report stale or
/// "unavailable" noise (mirrors `pgbouncer`'s post-mutation status print).
async fn print_status(
    ctx: &ServiceContext,
    config: &EnvironmentConfig,
    json: bool,
    include_live: bool,
) -> Result<()> {
    let root = resolve_root(ctx, config);
    let names = service_name_map(ctx);
    let ha_state = postgres_plugins::compute_ha_state(config, &root.root_id, &names);

    let root_pitr = config
        .services
        .get(&root.root_id)
        .map(postgres_plugins::compute_pitr_state)
        .unwrap_or_default();

    let members: Vec<PitrMemberStatus> = if ha_state.is_cluster {
        ha_state
            .members
            .iter()
            .filter(|m| matches!(m.cluster_role.as_deref(), Some("root") | Some("replica")))
            .map(|m| {
                let state = config
                    .services
                    .get(&m.service_id)
                    .map(postgres_plugins::compute_pitr_state)
                    .unwrap_or_default();
                PitrMemberStatus {
                    service: ResourceRef {
                        id: m.service_id.clone(),
                        name: m.service_name.clone(),
                    },
                    cluster_role: m.cluster_role.clone(),
                    enabled: state.enabled,
                    bucket_wired: state.bucket_wired,
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    // Live coverage/archiver probe is best-effort and only meaningful once the
    // overlay is actually applied -- skip it entirely for a service that never
    // had PITR enabled rather than spending a ~5s SSH round trip to learn
    // nothing.
    let live = if include_live && root_pitr.enabled {
        Some(probe_pitr_live(ctx, &root.root_id).await)
    } else {
        None
    };

    let output = PitrStatusOutput {
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
        is_ha_cluster: ha_state.is_cluster,
        enabled: root_pitr.enabled,
        bucket_wired: root_pitr.bucket_wired,
        blockers: guardrail_blockers(&root_pitr),
        members,
        live,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_pitr_status(&output);
    }
    Ok(())
}

fn print_pitr_status(output: &PitrStatusOutput) {
    println!("{}", "Point-in-time recovery (PITR)".bold());
    println!();
    print_field("Service:", &output.service.name.green().bold());
    print_field("Environment:", &output.environment.name.blue().bold());
    if output.root.id != output.service.id {
        print_field("Cluster root:", &output.root.name);
    }
    print_field("HA cluster:", &yes_no(output.is_ha_cluster));
    print_field("Status:", &status_label(output.enabled));
    print_field("Bucket wired:", &yes_no(output.bucket_wired));

    for blocker in &output.blockers {
        println!();
        print_field("Blocker:", &blocker.yellow());
    }

    if !output.members.is_empty() {
        println!();
        println!("{}", "Members:".bold());
        for member in &output.members {
            println!(
                "  {:<24} {:<10} {}",
                member.service.name,
                member.cluster_role.as_deref().unwrap_or("-"),
                status_label(member.enabled)
            );
        }
    }

    if let Some(live) = &output.live {
        println!();
        println!("{}", "Live coverage (best effort):".bold());
        if !live.available {
            print_field(
                "Probe:",
                &live
                    .unavailable_reason
                    .clone()
                    .unwrap_or_else(|| "unavailable".to_string())
                    .dimmed(),
            );
            return;
        }
        match &live.backup_coverage_error {
            Some(err) => print_field("Backup coverage:", &format!("unavailable ({err})").dimmed()),
            None => {
                print_field(
                    "Backup sets:",
                    &live
                        .backup_set_count
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                );
                print_field(
                    "Latest backup:",
                    &live.latest_backup_at.as_deref().unwrap_or("-"),
                );
                print_field(
                    "WAL coverage:",
                    &format!(
                        "{} .. {}",
                        live.wal_min.as_deref().unwrap_or("-"),
                        live.wal_max.as_deref().unwrap_or("-")
                    ),
                );
            }
        }
        match &live.archiver_error {
            Some(err) => print_field("Archiver:", &format!("unavailable ({err})").dimmed()),
            None => {
                let healthy = live.archiver_healthy.unwrap_or(false);
                print_field(
                    "Archiver:",
                    &if live.archiver_healthy.is_some() {
                        status_label(healthy)
                    } else {
                        "unknown".dimmed().bold()
                    },
                );
                print_field(
                    "Last archived at:",
                    &live.archiver_last_archived_at.as_deref().unwrap_or("-"),
                );
                print_field(
                    "Restorable up to:",
                    &live.max_restore_time.as_deref().unwrap_or("-"),
                );
            }
        }
    }
}

fn guardrail_blockers(state: &PitrState) -> Vec<String> {
    let mut blockers = Vec::new();
    if state.unsupported_image {
        blockers.push(
            "Image is not an official Railway Postgres image -- PITR is not supported.".to_string(),
        );
    }
    if state.minor_pinned {
        blockers.push(
            "Image is pinned to a minor version -- unpin to the major tag (e.g. `:16`) before enabling PITR."
                .to_string(),
        );
    }
    if state.has_start_command {
        blockers.push(
            "A custom start command overrides the entrypoint that turns on WAL archiving -- clear it before enabling PITR."
                .to_string(),
        );
    }
    blockers
}

async fn enable(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: EnableArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = postgres_plugins::compute_ha_state(&config, &root.root_id, &names);

    let target_service = config.services.get(&root.root_id).with_context(|| {
        format!(
            "Service \"{}\" not found in environment config",
            root.root_name
        )
    })?;
    let pitr_state = postgres_plugins::compute_pitr_state(target_service);

    if pitr_state.enabled {
        println!("PITR is already enabled for {}.", root.root_name.bold());
        return print_status(&ctx, &config, json, true).await;
    }

    let blockers = guardrail_blockers(&pitr_state);
    if !blockers.is_empty() {
        bail!(
            "Cannot enable PITR for {}:\n  - {}",
            root.root_name,
            blockers.join("\n  - ")
        );
    }

    if ha_state.is_cluster {
        if args.no_deploy {
            eprintln!(
                "Note: --no-deploy has no effect here -- enabling PITR on an HA cluster runs a live rolling restart with no staging step."
            );
        }
        let response = post_graphql::<mutations::EnablePitrForHaCluster, _>(
            &ctx.client,
            ctx.configs.get_backboard(),
            mutations::enable_pitr_for_ha_cluster::Variables {
                input: mutations::enable_pitr_for_ha_cluster::EnablePitrForHaClusterInput {
                    environment_id: ctx.environment_id.clone(),
                    project_id: ctx.project_id.clone(),
                    root_service_id: root.root_id.clone(),
                },
            },
        )
        .await
        .context("Failed to enable PITR on the HA cluster")?;

        follow_started_ha_workflow(
            &ctx,
            &root,
            json,
            response.enable_pitr_for_ha_cluster.workflow_id,
            "enable",
        )
        .await?;
        if !json {
            println!(
                "Enabled PITR for {} in environment {}.",
                root.root_name.bold(),
                ctx.environment_name.bold()
            );
        }
    } else {
        let result = template_apply::apply_composable_template(
            &ctx,
            ApplyTemplateParams {
                template_code: PITR_TEMPLATE_CODE.to_string(),
                service_id: root.root_id.clone(),
                // Overlay applies never take the pre-conversion safety
                // backup, so no volume-instance resolution is needed.
                volume_instance_id: None,
                replica_count: None,
                internal_count: None,
                edge_count: None,
                edge_variables: None,
                kind: ApplyKind::Overlay,
                auto_deploy: !args.no_deploy,
            },
        )
        .await
        .context("Failed to enable PITR")?;

        if !json {
            let verb = if result.deployed {
                "Enabled and deployed"
            } else {
                "Enabled (deploys skipped -- applies on the next deploy)"
            };
            println!(
                "{verb} PITR for {} in environment {} (project {}).",
                root.root_name.bold(),
                ctx.environment_name.bold(),
                result.project_id
            );
        }
    }

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    print_status(&ctx, &config, json, false).await
}

async fn disable(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: DisableArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = postgres_plugins::compute_ha_state(&config, &root.root_id, &names);

    if !confirm_or_bail(
        &format!(
            "Disable PITR for {}? This stops WAL archiving; existing backups are kept.",
            root.root_name.red()
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    if ha_state.is_cluster {
        match post_graphql::<queries::GetPitrHaClusterReplicationHealth, _>(
            &ctx.client,
            ctx.configs.get_backboard(),
            queries::get_pitr_ha_cluster_replication_health::Variables {
                environment_id: ctx.environment_id.clone(),
                root_service_id: root.root_id.clone(),
            },
        )
        .await
        {
            Ok(response) => {
                if let Some(health) = response.pitr_ha_cluster_replication_health
                    && health.reachable
                    && !health.all_healthy
                {
                    let unhealthy: Vec<String> = health
                        .members
                        .iter()
                        .filter(|m| !m.healthy)
                        .map(|m| m.service_name.clone())
                        .collect();
                    bail!(
                        "Cannot disable PITR: replication is not caught up on {}. Wait for replicas to catch up and try again.",
                        unhealthy.join(", ")
                    );
                }
            }
            Err(err) => {
                eprintln!(
                    "Warning: could not check replication health before disabling PITR: {err:#}"
                );
            }
        }

        if args.no_deploy {
            eprintln!(
                "Note: --no-deploy has no effect here -- disabling PITR on an HA cluster runs a live rolling restart with no staging step."
            );
        }
        let response = post_graphql::<mutations::DisablePitrForHaCluster, _>(
            &ctx.client,
            ctx.configs.get_backboard(),
            mutations::disable_pitr_for_ha_cluster::Variables {
                input: mutations::disable_pitr_for_ha_cluster::DisablePitrForHaClusterInput {
                    environment_id: ctx.environment_id.clone(),
                    project_id: ctx.project_id.clone(),
                    root_service_id: root.root_id.clone(),
                },
            },
        )
        .await
        .context("Failed to disable PITR on the HA cluster")?;

        follow_started_ha_workflow(
            &ctx,
            &root,
            json,
            response.disable_pitr_for_ha_cluster.workflow_id,
            "disable",
        )
        .await?;
        if !json {
            println!(
                "Disabled PITR for {} in environment {}.",
                root.root_name.bold(),
                ctx.environment_name.bold()
            );
        }
    } else {
        let result = template_apply::revert_template(
            &ctx,
            RevertTemplateParams {
                template_code: PITR_TEMPLATE_CODE.to_string(),
                root_service_id: root.root_id.clone(),
                auto_deploy: !args.no_deploy,
            },
        )
        .await
        .context("Failed to disable PITR")?;

        if !json {
            let verb = if result.deployed {
                "Disabled and deployed"
            } else {
                "Disabled (deploys skipped -- applies on the next deploy)"
            };
            println!(
                "{verb} PITR on {} in environment {} (project {}).",
                root.root_name.bold(),
                ctx.environment_name.bold(),
                result.project_id
            );
        }
    }

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    print_status(&ctx, &config, json, false).await
}

async fn cancel(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = postgres_plugins::compute_ha_state(&config, &root.root_id, &names);
    if !ha_state.is_cluster {
        bail!(
            "{} is not an HA cluster; there is no PITR workflow to cancel.",
            root.root_name
        );
    }

    post_graphql::<mutations::CancelPitrHaWorkflow, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::cancel_pitr_ha_workflow::Variables {
            environment_id: ctx.environment_id.clone(),
            root_service_id: root.root_id.clone(),
        },
    )
    .await
    .context("Failed to cancel the PITR workflow")?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"cancelled": true}))?
        );
    } else {
        println!("Cancelled the PITR workflow for {}.", root.root_name.bold());
    }
    Ok(())
}

async fn clear(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = postgres_plugins::compute_ha_state(&config, &root.root_id, &names);
    if !ha_state.is_cluster {
        bail!(
            "{} is not an HA cluster; there is no PITR workflow progress to clear.",
            root.root_name
        );
    }

    post_graphql::<mutations::ClearPitrHaWorkflowProgress, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::clear_pitr_ha_workflow_progress::Variables {
            environment_id: ctx.environment_id.clone(),
            root_service_id: root.root_id.clone(),
        },
    )
    .await
    .context("Failed to clear the PITR workflow progress")?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"cleared": true}))?
        );
    } else {
        println!(
            "Cleared PITR workflow progress for {}.",
            root.root_name.bold()
        );
    }
    Ok(())
}

/// Max time `progress --watch` polls before giving up (the workflow itself
/// may still finish later -- this only bounds the CLI's own wait).
const PROGRESS_WATCH_TIMEOUT_SECS: u64 = 600;
const PROGRESS_POLL_INTERVAL_SECS: u64 = 2;
/// Follow deadline for a workflow this command just started (`pitr
/// enable`/`disable` on an HA cluster). Deliberately much longer than the
/// generic ~2-minute `wait_for_workflow` cap: the rolling enable/disable
/// restarts every member one at a time and its server-side activities allow
/// up to 20 minutes each.
const HA_WORKFLOW_FOLLOW_TIMEOUT_SECS: u64 = 1800;
/// How long a just-started workflow gets to make its progress record
/// visible before the follower falls back to the generic workflow wait.
const PROGRESS_VISIBILITY_GRACE_SECS: u64 = 60;

async fn progress(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: ProgressArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let names = service_name_map(&ctx);
    let ha_state = postgres_plugins::compute_ha_state(&config, &root.root_id, &names);
    if !ha_state.is_cluster {
        bail!(
            "{} is not an HA cluster; there is no PITR workflow progress to show.",
            root.root_name
        );
    }

    let observed = follow_progress(
        &ctx,
        &root,
        &ProgressFollow {
            watch: args.watch,
            expected_workflow_id: None,
            deadline: Duration::from_secs(PROGRESS_WATCH_TIMEOUT_SECS),
            fail_on_failed: false,
            render: true,
            json,
        },
    )
    .await?;

    if observed.is_none() {
        if json {
            println!("{}", serde_json::json!({"active": false}));
        } else {
            println!(
                "No PITR enable/disable workflow found for {}.",
                root.root_name.bold()
            );
        }
    }
    Ok(())
}

/// Options for [`follow_progress`].
struct ProgressFollow {
    /// Keep polling until a terminal phase; `false` renders at most one
    /// snapshot and returns.
    watch: bool,
    /// Only accept the progress record for this specific workflow id.
    /// Protects a just-started follow from latching onto a stale (already
    /// terminal) record from a previous run that was never `clear`ed.
    expected_workflow_id: Option<String>,
    deadline: Duration,
    /// Treat a FAILED terminal phase as a command failure (`enable`/
    /// `disable`) instead of a state to display (`progress`).
    fail_on_failed: bool,
    /// Render snapshots as they change. Disabled for `enable`/`disable`
    /// under `--json`, where only the final status document may reach
    /// stdout.
    render: bool,
    json: bool,
}

/// Polls `pitrHaWorkflowProgress` and renders snapshots as they change.
/// Returns `Ok(Some(<final phase>))` once a (matching) progress record was
/// observed, or `Ok(None)` if none became visible in time.
async fn follow_progress(
    ctx: &ServiceContext,
    root: &super::RootContext,
    opts: &ProgressFollow,
) -> Result<Option<String>> {
    let start = std::time::Instant::now();
    let deadline = start + opts.deadline;
    let visibility_deadline = start + Duration::from_secs(PROGRESS_VISIBILITY_GRACE_SECS);
    let mut last_printed: Option<String> = None;

    loop {
        let response = post_graphql::<queries::GetPitrHaWorkflowProgress, _>(
            &ctx.client,
            ctx.configs.get_backboard(),
            queries::get_pitr_ha_workflow_progress::Variables {
                environment_id: ctx.environment_id.clone(),
                root_service_id: root.root_id.clone(),
            },
        )
        .await
        .context("Failed to fetch the PITR workflow progress")?;

        let progress = response.pitr_ha_workflow_progress.filter(|p| {
            opts.expected_workflow_id
                .as_deref()
                .is_none_or(|expected| p.workflow_id == expected)
        });

        let Some(progress) = progress else {
            if opts.expected_workflow_id.is_some()
                && std::time::Instant::now() < visibility_deadline
            {
                sleep(Duration::from_secs(PROGRESS_POLL_INTERVAL_SECS)).await;
                continue;
            }
            return Ok(None);
        };

        let output = build_progress_output(root, &progress);
        if opts.render {
            let rendered = serde_json::to_string(&output)?;
            if last_printed.as_deref() != Some(rendered.as_str()) {
                if opts.json {
                    println!("{}", serde_json::to_string_pretty(&output)?);
                } else {
                    print_progress(&output);
                }
                last_printed = Some(rendered);
            }
        }

        if output.phase == "failed" {
            if opts.fail_on_failed {
                let detail = output
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "no error detail reported".to_string());
                bail!(
                    "The PITR workflow failed{}: {detail}",
                    output
                        .failed_at_phase
                        .as_deref()
                        .map(|phase| format!(" during {phase}"))
                        .unwrap_or_default()
                );
            }
            return Ok(Some(output.phase));
        }
        if output.phase == "done" || !opts.watch {
            return Ok(Some(output.phase));
        }

        if std::time::Instant::now() >= deadline {
            bail!(
                "Timed out after {}s waiting for the workflow to reach a terminal phase. It keeps running server-side -- follow it with `railway postgres pitr progress --watch`.",
                opts.deadline.as_secs()
            );
        }

        sleep(Duration::from_secs(PROGRESS_POLL_INTERVAL_SECS)).await;
    }
}

/// Follows a rolling HA PITR enable/disable workflow this command just
/// started. Prefers the dedicated progress record (rich phase/member
/// rendering, a deadline sized for a full rolling restart); falls back to
/// the generic workflow-status wait if the progress record never becomes
/// visible, rather than reporting success blind.
async fn follow_started_ha_workflow(
    ctx: &ServiceContext,
    root: &super::RootContext,
    json: bool,
    workflow_id: Option<String>,
    direction: &str,
) -> Result<()> {
    let Some(workflow_id) = workflow_id else {
        return Ok(());
    };

    if !json {
        println!(
            "Rolling the cluster to {direction} PITR -- members restart one at a time; this can take several minutes. Following progress (the workflow keeps running server-side if you interrupt)..."
        );
    }

    let observed = follow_progress(
        ctx,
        root,
        &ProgressFollow {
            watch: true,
            expected_workflow_id: Some(workflow_id.clone()),
            deadline: Duration::from_secs(HA_WORKFLOW_FOLLOW_TIMEOUT_SECS),
            fail_on_failed: true,
            render: !json,
            json,
        },
    )
    .await?;

    if observed.is_none() {
        use crate::controllers::workflow::{WorkflowError, wait_for_workflow};
        wait_for_workflow(&ctx.client, &ctx.configs, workflow_id)
            .await
            .map_err(|err| match err {
                WorkflowError::Timeout => anyhow::anyhow!(
                    "The PITR workflow is still running (the CLI stopped waiting). Follow it with `railway postgres pitr progress --watch`."
                ),
                other => other.into(),
            })?;
    }
    Ok(())
}

fn build_progress_output(
    root: &super::RootContext,
    progress: &queries::get_pitr_ha_workflow_progress::GetPitrHaWorkflowProgressPitrHaWorkflowProgress,
) -> PitrProgressOutput {
    use queries::get_pitr_ha_workflow_progress::{
        PitrHaWorkflowDirection, PitrHaWorkflowMemberStatus, PitrHaWorkflowPhase,
    };

    let phase_str = |phase: &PitrHaWorkflowPhase| -> String {
        match phase {
            PitrHaWorkflowPhase::PLANNING => "planning".to_string(),
            PitrHaWorkflowPhase::CREATING_BUCKET => "creating_bucket".to_string(),
            PitrHaWorkflowPhase::WRITING_VARIABLES => "writing_variables".to_string(),
            PitrHaWorkflowPhase::PATCHING_DCS => "patching_dcs".to_string(),
            PitrHaWorkflowPhase::ROLLING_REPLICAS => "rolling_replicas".to_string(),
            PitrHaWorkflowPhase::SWITCHING_OVER => "switching_over".to_string(),
            PitrHaWorkflowPhase::ROLLING_EX_LEADER => "rolling_ex_leader".to_string(),
            PitrHaWorkflowPhase::REMOVING_VARIABLES => "removing_variables".to_string(),
            PitrHaWorkflowPhase::VERIFYING => "verifying".to_string(),
            PitrHaWorkflowPhase::DONE => "done".to_string(),
            PitrHaWorkflowPhase::FAILED => "failed".to_string(),
            PitrHaWorkflowPhase::Other(other) => other.to_ascii_lowercase(),
        }
    };
    let direction_str = |direction: &PitrHaWorkflowDirection| -> String {
        match direction {
            PitrHaWorkflowDirection::ENABLE => "enable".to_string(),
            PitrHaWorkflowDirection::DISABLE => "disable".to_string(),
            PitrHaWorkflowDirection::Other(other) => other.to_ascii_lowercase(),
        }
    };
    let member_status_str = |status: &PitrHaWorkflowMemberStatus| -> String {
        match status {
            PitrHaWorkflowMemberStatus::HEALTHY => "healthy".to_string(),
            PitrHaWorkflowMemberStatus::PENDING => "pending".to_string(),
            PitrHaWorkflowMemberStatus::RESTARTING => "restarting".to_string(),
            PitrHaWorkflowMemberStatus::SKIPPED => "skipped".to_string(),
            PitrHaWorkflowMemberStatus::Other(other) => other.to_ascii_lowercase(),
        }
    };

    PitrProgressOutput {
        root: ResourceRef {
            id: root.root_id.clone(),
            name: root.root_name.clone(),
        },
        workflow_id: progress.workflow_id.clone(),
        direction: direction_str(&progress.direction),
        phase: phase_str(&progress.phase),
        started_at: progress.started_at.clone(),
        updated_at: progress.updated_at.clone(),
        completed_at: progress.completed_at.clone(),
        error_message: progress.error_message.clone(),
        failed_at_phase: progress.failed_at_phase.as_ref().map(phase_str),
        current_member_service_id: progress.current_member_service_id.clone(),
        new_leader_service_id: progress.new_leader_service_id.clone(),
        cluster_mutated: progress.cluster_mutated,
        members: progress
            .members
            .iter()
            .map(|m| PitrProgressMemberOutput {
                service_id: m.service_id.clone(),
                service_name: m.service_name.clone(),
                is_leader: m.is_leader,
                status: member_status_str(&m.status),
            })
            .collect(),
    }
}

fn print_progress(output: &PitrProgressOutput) {
    println!("{}", "PITR HA workflow progress".bold());
    println!();
    print_field("Root:", &output.root.name);
    print_field("Direction:", &output.direction);
    print_field("Phase:", &output.phase.bold());
    print_field("Started:", &output.started_at);
    print_field("Updated:", &output.updated_at);
    if let Some(completed_at) = &output.completed_at {
        print_field("Completed:", completed_at);
    }
    if let Some(error) = &output.error_message {
        print_field("Error:", &error.red());
    }
    if let Some(failed_at) = &output.failed_at_phase {
        print_field("Failed at phase:", &failed_at.red());
    }

    if !output.members.is_empty() {
        println!();
        println!("{}", "Members:".bold());
        for member in &output.members {
            println!(
                "  {:<28} {:<6} {}",
                member.service_name,
                if member.is_leader { "leader" } else { "-" },
                member.status
            );
        }
    }
}

async fn restore(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: RestoreArgs,
) -> Result<()> {
    let target_timestamp =
        parse_time(&args.at).with_context(|| format!("Invalid --at value \"{}\"", args.at))?;

    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let volume_instance_id = resolve_volume_instance_id(&ctx, &root).await?;

    let new_service_note = args
        .new_service_name
        .clone()
        .unwrap_or_else(|| format!("{}-restored-<timestamp>", root.root_name));

    if !confirm_or_bail(
        &format!(
            "Restore {} to {}? This creates a brand-new service ({new_service_note}) from the point-in-time snapshot -- {} keeps running untouched.",
            root.root_name.yellow(),
            target_timestamp.to_rfc3339(),
            root.root_name
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    let response = post_graphql::<mutations::VolumeInstancePitrRestore, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::volume_instance_pitr_restore::Variables {
            volume_instance_id,
            target_timestamp,
            new_service_name: args.new_service_name.clone(),
            source_repo_path: args.source_repo_path.clone(),
        },
    )
    .await
    .context("Failed to start the point-in-time restore")?;

    let workflow_id = response.volume_instance_pitr_restore.workflow_id;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "root": { "id": root.root_id, "name": root.root_name },
                "targetTimestamp": target_timestamp.to_rfc3339(),
                "workflowId": workflow_id,
            }))?
        );
    } else {
        println!(
            "Started a point-in-time restore of {} to {}.",
            root.root_name.bold(),
            target_timestamp.to_rfc3339()
        );
        if let Some(id) = &workflow_id {
            print_field("Workflow:", id);
        }
        println!(
            "This runs in the background; the new service will appear in the dashboard once provisioning completes."
        );
    }
    Ok(())
}

/// Shared by every `backup`/`schedule` subcommand: resolves the target root
/// service's live VOLUME-INSTANCE id via the environment's volumeInstances.
/// Deliberately NOT the environment config's `volumeMounts` key -- that map
/// is keyed by VOLUME id, which the backup/restore mutations reject
/// (confirmed live: the auth scope can't resolve a volume id as an instance
/// and the call reads back as Not Authorized).
async fn resolve_volume_instance_id(
    ctx: &ServiceContext,
    root: &super::RootContext,
) -> Result<String> {
    let instances = crate::controllers::project::get_environment_instances(
        &ctx.client,
        &ctx.configs,
        &ctx.project_id,
        &ctx.environment_id,
    )
    .await?;

    instances
        .volume_instances
        .iter()
        .find(|edge| edge.node.service_id.as_deref() == Some(root.root_id.as_str()))
        .map(|edge| edge.node.id.clone())
        .with_context(|| {
            format!(
                "{} has no volume attached -- PITR backups require a volume.",
                root.root_name
            )
        })
}

async fn backup_list(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let volume_instance_id = resolve_volume_instance_id(&ctx, &root).await?;

    let response = post_graphql::<queries::VolumeInstanceBackupList, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::volume_instance_backup_list::Variables { volume_instance_id },
    )
    .await
    .context("Failed to list backups")?;
    let backups = response.volume_instance_backup_list;

    if json {
        println!("{}", serde_json::to_string_pretty(&backups)?);
        return Ok(());
    }

    if backups.is_empty() {
        println!("No backups found for {}.", root.root_name.bold());
        return Ok(());
    }

    println!(
        "{:<26} {:<20} {:<26} {:>10} {:<12} EXPIRES",
        "ID", "NAME", "CREATED", "SIZE (MB)", "SCHEDULE"
    );
    for backup in &backups {
        println!(
            "{:<26} {:<20} {:<26} {:>10} {:<12} {}",
            backup.id,
            backup.name.as_deref().unwrap_or("-"),
            backup.created_at.to_rfc3339(),
            backup
                .used_mb
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string()),
            backup.schedule_id.as_deref().unwrap_or("manual"),
            backup
                .expires_at
                .map(|v| v.to_rfc3339())
                .unwrap_or_else(|| "never".to_string()),
        );
    }
    Ok(())
}

async fn backup_create(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: BackupCreateArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let volume_instance_id = resolve_volume_instance_id(&ctx, &root).await?;

    let response = post_graphql::<mutations::VolumeInstanceBackupCreate, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::volume_instance_backup_create::Variables {
            volume_instance_id,
            name: args.name.clone(),
        },
    )
    .await
    .context("Failed to create a backup")?;
    let workflow_id = response.volume_instance_backup_create.workflow_id;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"root": {"id": root.root_id, "name": root.root_name}, "workflowId": workflow_id})
            )?
        );
    } else {
        println!("Started an on-demand backup for {}.", root.root_name.bold());
        if let Some(id) = &workflow_id {
            print_field("Workflow:", id);
        }
        println!("Check `railway postgres pitr backup list` once it completes.");
    }
    Ok(())
}

async fn backup_delete(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: BackupDeleteArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let volume_instance_id = resolve_volume_instance_id(&ctx, &root).await?;

    if !confirm_or_bail(
        &format!(
            "Delete {} backup(s) forever ({})? This cannot be undone.",
            args.ids.len(),
            args.ids.join(", ").red()
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    // One delete mutation per id -- the batch mutation is Internal-subgraph
    // only. Sequential on purpose: each returns its own deletion workflow,
    // and a mid-list failure reports exactly which ids already started.
    let mut workflow_ids: Vec<Option<String>> = Vec::with_capacity(args.ids.len());
    for (index, backup_id) in args.ids.iter().enumerate() {
        let response = post_graphql::<mutations::VolumeInstanceBackupDelete, _>(
            &ctx.client,
            ctx.configs.get_backboard(),
            mutations::volume_instance_backup_delete::Variables {
                volume_instance_id: volume_instance_id.clone(),
                volume_instance_backup_id: backup_id.clone(),
            },
        )
        .await
        .with_context(|| {
            format!(
                "Failed to delete backup {backup_id}{}",
                if index > 0 {
                    format!(
                        " (deletion already started for: {})",
                        args.ids[..index].join(", ")
                    )
                } else {
                    String::new()
                }
            )
        })?;
        workflow_ids.push(response.volume_instance_backup_delete.workflow_id);
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"deletedIds": args.ids, "workflowIds": workflow_ids})
            )?
        );
    } else {
        println!(
            "Started deleting {} backup(s) -- deletion runs in the background.",
            args.ids.len()
        );
        for id in workflow_ids.iter().flatten() {
            print_field("Workflow:", id);
        }
    }
    Ok(())
}

async fn backup_lock(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: BackupIdArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let volume_instance_id = resolve_volume_instance_id(&ctx, &root).await?;

    let response = post_graphql::<mutations::VolumeInstanceBackupLock, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::volume_instance_backup_lock::Variables {
            volume_instance_id,
            volume_instance_backup_id: args.id.clone(),
        },
    )
    .await
    .context("Failed to lock the backup")?;
    let locked = response.volume_instance_backup_lock;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"id": args.id, "locked": locked}))?
        );
    } else if locked {
        println!(
            "Backup {} will now be kept indefinitely (expiration removed).",
            args.id.bold()
        );
    } else {
        println!("Could not lock backup {}.", args.id.bold());
    }
    Ok(())
}

async fn backup_restore(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: BackupRestoreArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let volume_instance_id = resolve_volume_instance_id(&ctx, &root).await?;

    if !confirm_or_bail(
        &format!(
            "Restore {} from backup {}? This overwrites the current data with the backup's contents.",
            root.root_name.red(),
            args.id
        ),
        args.yes,
    )? {
        println!("Cancelled.");
        return Ok(());
    }

    let response = post_graphql::<mutations::VolumeInstanceBackupRestore, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::volume_instance_backup_restore::Variables {
            volume_instance_id,
            volume_instance_backup_id: args.id.clone(),
            replica_service_ids: None,
            wipe_service_ids: None,
        },
    )
    .await
    .context("Failed to restore from the backup")?;
    let workflow_id = response.volume_instance_backup_restore.workflow_id;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"root": {"id": root.root_id, "name": root.root_name}, "backupId": args.id, "workflowId": workflow_id})
            )?
        );
    } else {
        println!(
            "Started restoring {} from backup {}.",
            root.root_name.bold(),
            args.id
        );
        if let Some(id) = &workflow_id {
            print_field("Workflow:", id);
        }
    }
    Ok(())
}

async fn schedule_set(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: ScheduleSetArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let volume_instance_id = resolve_volume_instance_id(&ctx, &root).await?;

    use mutations::volume_instance_backup_schedule_update::VolumeInstanceBackupScheduleKind as ScheduleKind;
    let mut kinds = Vec::new();
    if args.daily {
        kinds.push(ScheduleKind::DAILY);
    }
    if args.weekly {
        kinds.push(ScheduleKind::WEEKLY);
    }
    if args.monthly {
        kinds.push(ScheduleKind::MONTHLY);
    }

    post_graphql::<mutations::VolumeInstanceBackupScheduleUpdate, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::volume_instance_backup_schedule_update::Variables {
            volume_instance_id,
            kinds: Some(kinds),
        },
    )
    .await
    .context("Failed to update the backup schedule")?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "daily": args.daily,
                "weekly": args.weekly,
                "monthly": args.monthly,
                "cleared": args.none,
            }))?
        );
    } else if args.none {
        println!(
            "Removed every automatic backup schedule for {} (existing backups are kept).",
            root.root_name.bold()
        );
    } else {
        let mut labels = Vec::new();
        if args.daily {
            labels.push("daily");
        }
        if args.weekly {
            labels.push("weekly");
        }
        if args.monthly {
            labels.push("monthly");
        }
        println!(
            "Updated the backup schedule for {}: {}.",
            root.root_name.bold(),
            labels.join(", ")
        );
    }
    Ok(())
}

async fn schedule_list(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, true)
        .await?
        .config;
    let root = resolve_root(&ctx, &config);
    let volume_instance_id = resolve_volume_instance_id(&ctx, &root).await?;

    let response = post_graphql::<queries::VolumeInstanceBackupScheduleList, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::volume_instance_backup_schedule_list::Variables { volume_instance_id },
    )
    .await
    .context("Failed to list backup schedules")?;
    let schedules = response.volume_instance_backup_schedule_list;

    if json {
        println!("{}", serde_json::to_string_pretty(&schedules)?);
        return Ok(());
    }

    if schedules.is_empty() {
        println!(
            "No backup schedule configured for {}.",
            root.root_name.bold()
        );
        return Ok(());
    }

    println!("{:<10} {:<24} {:<26} RETENTION", "KIND", "NAME", "CREATED");
    for s in &schedules {
        println!(
            "{:<10} {:<24} {:<26} {}",
            format!("{:?}", s.kind),
            s.name,
            s.created_at.to_rfc3339(),
            s.retention_seconds
                .map(|r| format!("{r}s"))
                .unwrap_or_else(|| "-".to_string()),
        );
    }
    Ok(())
}

/// Total time budget for `status`'s live coverage/archiver probe. Kept short
/// -- this is a best-effort addition to `status`, never worth making the
/// whole command feel slow (or hang) when the service isn't reachable.
const LIVE_PROBE_TIMEOUT_SECS: u64 = 5;

async fn probe_pitr_live(ctx: &ServiceContext, root_service_id: &str) -> PitrLiveProbe {
    let attempt = async {
        let instance_id = get_service_instance_id(
            &ctx.client,
            &ctx.configs,
            &ctx.environment_id,
            root_service_id,
        )
        .await
        .context("No live deployment found for this service")?;

        let (pgbackrest_result, archiver_result) = tokio::join!(
            exec_in_container(&instance_id, "pgbackrest info --output=json"),
            exec_in_container(&instance_id, ARCHIVER_PROBE_QUERY),
        );

        let mut probe = PitrLiveProbe {
            available: true,
            ..PitrLiveProbe::default()
        };

        match pgbackrest_result {
            Ok(output) => apply_pgbackrest_info(&mut probe, &output),
            Err(err) => {
                probe.backup_coverage_error =
                    Some(diagnose_db_stats_failure(&err, &DatabaseType::PostgreSQL))
            }
        }
        match archiver_result {
            Ok(output) => apply_archiver_output(&mut probe, &output),
            Err(err) => {
                probe.archiver_error =
                    Some(diagnose_db_stats_failure(&err, &DatabaseType::PostgreSQL))
            }
        }

        Ok::<_, anyhow::Error>(probe)
    };

    // A missing local SSH key is by far the most common reason this probe
    // can't run at all -- check for it up front (no network call) so the
    // failure reason is specific instead of a generic SSH timeout/refusal.
    if let Err(reason) = preflight_db_stats_ssh().await {
        return PitrLiveProbe {
            available: false,
            unavailable_reason: Some(reason),
            ..PitrLiveProbe::default()
        };
    }

    match timeout(Duration::from_secs(LIVE_PROBE_TIMEOUT_SECS), attempt).await {
        Ok(Ok(probe)) => probe,
        Ok(Err(err)) => PitrLiveProbe {
            available: false,
            unavailable_reason: Some(format!("{err:#}")),
            ..PitrLiveProbe::default()
        },
        Err(_) => PitrLiveProbe {
            available: false,
            unavailable_reason: Some(format!("probe timed out after {LIVE_PROBE_TIMEOUT_SECS}s")),
            ..PitrLiveProbe::default()
        },
    }
}

/// Loosely parses `pgbackrest info --output=json`'s shape (an array of
/// stanzas, each with a `backup` list and an `archive` list) -- deliberately
/// tolerant of missing fields since this runs against whatever pgBackRest
/// version/config the image ships, not a pinned schema.
fn apply_pgbackrest_info(probe: &mut PitrLiveProbe, output: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output.trim()) else {
        probe.backup_coverage_error = Some("could not parse pgbackrest output".to_string());
        return;
    };
    let Some(stanza) = value.as_array().and_then(|arr| arr.first()) else {
        probe.backup_coverage_error = Some("pgbackrest reported no stanzas".to_string());
        return;
    };

    if let Some(backups) = stanza.get("backup").and_then(|b| b.as_array()) {
        probe.backup_set_count = Some(backups.len());
        probe.latest_backup_at = backups
            .last()
            .and_then(|b| b.get("timestamp"))
            .and_then(|t| t.get("stop"))
            .and_then(|v| v.as_i64())
            .and_then(|epoch| chrono::DateTime::<Utc>::from_timestamp(epoch, 0))
            .map(|dt| dt.to_rfc3339());
    }
    if let Some(archive) = stanza
        .get("archive")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
    {
        probe.wal_min = archive
            .get("min")
            .and_then(|v| v.as_str())
            .map(String::from);
        probe.wal_max = archive
            .get("max")
            .and_then(|v| v.as_str())
            .map(String::from);
    }
}

const ARCHIVER_PROBE_QUERY: &str = concat!(
    "PGHOST=localhost PGPORT=5432 PGSSLMODE=disable psql -t -A -F',' -q -c \"",
    "SELECT archived_count, coalesce(last_archived_time::text, ''), failed_count, ",
    "coalesce(last_failed_time::text, ''), coalesce(((pg_last_committed_xact()).timestamp)::text, '') ",
    "FROM pg_stat_archiver\"",
);

/// Parses the single CSV-ish line `ARCHIVER_PROBE_QUERY` prints and applies
/// the sticky-field gate from prior PITR incident work: `pg_stat_archiver`'s
/// `last_failed_*` columns never clear on their own (only a Postgres restart
/// resets them), so a failure from days ago would otherwise permanently read
/// as "unhealthy" -- only treat it as current if it's newer than the last
/// successful archive.
fn apply_archiver_output(probe: &mut PitrLiveProbe, output: &str) {
    let line = output.lines().next().unwrap_or("").trim();
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() < 5 {
        probe.archiver_error = Some("unexpected archiver probe output".to_string());
        return;
    }

    let last_archived_time = non_empty(fields[1]);
    let last_failed_time = non_empty(fields[3]);
    let last_committed_at = non_empty(fields[4]);

    probe.archiver_last_archived_at = last_archived_time.clone();
    probe.max_restore_time = last_committed_at;

    probe.archiver_healthy = match (
        last_archived_time.as_deref().and_then(parse_pg_timestamp),
        last_failed_time.as_deref().and_then(parse_pg_timestamp),
    ) {
        (_, None) => Some(true), // no failure recorded at all
        (Some(archived), Some(failed)) => Some(failed <= archived),
        (None, Some(_)) => Some(false), // failed, and never successfully archived
    };
}

fn non_empty(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Parses Postgres's `timestamptz::text` output (e.g.
/// `2026-07-28 10:00:00.123456+00`), which isn't quite RFC3339 (space instead
/// of `T`, no colon in the offset).
fn parse_pg_timestamp(s: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%#z")
        .ok()
        .or_else(|| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PitrProgressMemberOutput {
    service_id: String,
    service_name: String,
    is_leader: bool,
    status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PitrProgressOutput {
    root: ResourceRef,
    workflow_id: String,
    direction: String,
    phase: String,
    started_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failed_at_phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_member_service_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_leader_service_id: Option<String>,
    cluster_mutated: bool,
    members: Vec<PitrProgressMemberOutput>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PitrMemberStatus {
    service: ResourceRef,
    cluster_role: Option<String>,
    enabled: bool,
    bucket_wired: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PitrStatusOutput {
    service: ResourceRef,
    environment: ResourceRef,
    root: ResourceRef,
    is_ha_cluster: bool,
    enabled: bool,
    bucket_wired: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    blockers: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    members: Vec<PitrMemberStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live: Option<PitrLiveProbe>,
}

/// Best-effort live coverage/archiver probe (`pgbackrest info` + `pg_stat_archiver`
/// over SSH into the root service's running container). `available == false`
/// means the probe itself couldn't run at all (no live deployment, no SSH key,
/// unreachable, timed out); `backup_coverage_error`/`archiver_error` mean the
/// probe connected but one half of the two independent sub-probes failed
/// (e.g. `pgbackrest` not installed on a non-official image, or the Postgres
/// user lacks `pg_monitor`) while the other still reports.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct PitrLiveProbe {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_coverage_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_set_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_backup_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wal_min: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wal_max: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archiver_error: Option<String>,
    /// `None` when the archiver query ran but the sticky-failure gate
    /// couldn't be evaluated (one or both timestamps failed to parse) --
    /// printed as "unknown" rather than a false "healthy"/"unhealthy".
    #[serde(skip_serializing_if = "Option::is_none")]
    archiver_healthy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archiver_last_archived_at: Option<String>,
    /// Approximate PITR restore ceiling (`(pg_last_committed_xact()).timestamp`).
    /// Deliberately simpler than the backend's authoritative
    /// `GREATEST(pg_last_committed_xact, pg_xact_commit_timestamp)` -- this is
    /// a best-effort CLI probe, not a replacement for the admin fleet monitor.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_restore_time: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_top_level_verbs() {
        assert!(matches!(
            Args::parse_from(["pitr", "status"]).command,
            Commands::Status
        ));
        assert!(matches!(
            Args::parse_from(["pitr", "enable"]).command,
            Commands::Enable(_)
        ));
        assert!(matches!(
            Args::parse_from(["pitr", "disable", "--yes"]).command,
            Commands::Disable(DisableArgs {
                yes: true,
                no_deploy: false
            })
        ));
        assert!(matches!(
            Args::parse_from(["pitr", "progress"]).command,
            Commands::Progress(_)
        ));
        assert!(matches!(
            Args::parse_from(["pitr", "cancel"]).command,
            Commands::Cancel
        ));
        assert!(matches!(
            Args::parse_from(["pitr", "clear"]).command,
            Commands::Clear
        ));
    }

    #[test]
    fn parses_restore_flags() {
        let args = Args::parse_from([
            "pitr",
            "restore",
            "--at",
            "2026-07-20T12:00:00Z",
            "--new-service-name",
            "restored-db",
            "--yes",
        ]);
        let Commands::Restore(restore) = args.command else {
            panic!("expected restore");
        };
        assert_eq!(restore.at, "2026-07-20T12:00:00Z");
        assert_eq!(restore.new_service_name.as_deref(), Some("restored-db"));
        assert!(restore.yes);
    }

    #[test]
    fn parses_backup_subcommands() {
        assert!(matches!(
            Args::parse_from(["pitr", "backup", "list"]).command,
            Commands::Backup(BackupArgs {
                command: BackupCommands::List
            })
        ));
        let args = Args::parse_from(["pitr", "backup", "create", "--name", "pre-migration"]);
        let Commands::Backup(BackupArgs {
            command: BackupCommands::Create(create),
        }) = args.command
        else {
            panic!("expected backup create");
        };
        assert_eq!(create.name.as_deref(), Some("pre-migration"));

        let args = Args::parse_from(["pitr", "backup", "delete", "id1", "id2", "--yes"]);
        let Commands::Backup(BackupArgs {
            command: BackupCommands::Delete(delete),
        }) = args.command
        else {
            panic!("expected backup delete");
        };
        assert_eq!(delete.ids, vec!["id1".to_string(), "id2".to_string()]);
        assert!(delete.yes);
    }

    #[test]
    fn schedule_set_requires_at_least_one_kind() {
        assert!(Args::try_parse_from(["pitr", "schedule", "set"]).is_err());
        let args = Args::parse_from(["pitr", "schedule", "set", "--daily", "--weekly"]);
        let Commands::Schedule(ScheduleArgs {
            command: ScheduleCommands::Set(set),
        }) = args.command
        else {
            panic!("expected schedule set");
        };
        assert!(set.daily);
        assert!(set.weekly);
        assert!(!set.monthly);
        assert!(!set.none);
    }

    #[test]
    fn schedule_set_none_clears_and_conflicts_with_kinds() {
        let args = Args::parse_from(["pitr", "schedule", "set", "--none"]);
        let Commands::Schedule(ScheduleArgs {
            command: ScheduleCommands::Set(set),
        }) = args.command
        else {
            panic!("expected schedule set");
        };
        assert!(set.none);
        assert!(!set.daily && !set.weekly && !set.monthly);

        assert!(Args::try_parse_from(["pitr", "schedule", "set", "--none", "--daily"]).is_err());
        assert!(Args::try_parse_from(["pitr", "schedule", "set", "--none", "--monthly"]).is_err());
    }

    #[test]
    fn parses_schedule_list() {
        assert!(matches!(
            Args::parse_from(["pitr", "schedule", "list"]).command,
            Commands::Schedule(ScheduleArgs {
                command: ScheduleCommands::List
            })
        ));
    }

    #[test]
    fn parses_progress_watch_flag() {
        let args = Args::parse_from(["pitr", "progress", "--watch"]);
        let Commands::Progress(progress) = args.command else {
            panic!("expected progress");
        };
        assert!(progress.watch);

        let args = Args::parse_from(["pitr", "progress"]);
        let Commands::Progress(progress) = args.command else {
            panic!("expected progress");
        };
        assert!(!progress.watch);
    }

    #[test]
    fn parses_backup_lock_and_restore() {
        let args = Args::parse_from(["pitr", "backup", "lock", "backup-1"]);
        let Commands::Backup(BackupArgs {
            command: BackupCommands::Lock(lock),
        }) = args.command
        else {
            panic!("expected backup lock");
        };
        assert_eq!(lock.id, "backup-1");

        let args = Args::parse_from(["pitr", "backup", "restore", "backup-1", "--yes"]);
        let Commands::Backup(BackupArgs {
            command: BackupCommands::Restore(restore),
        }) = args.command
        else {
            panic!("expected backup restore");
        };
        assert_eq!(restore.id, "backup-1");
        assert!(restore.yes);
    }

    #[test]
    fn parse_pg_timestamp_handles_postgres_text_format() {
        let parsed = parse_pg_timestamp("2026-07-28 10:00:00.123456+00").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-07-28T10:00:00.123456+00:00");
        assert!(parse_pg_timestamp("not-a-timestamp").is_none());
        // RFC3339 fallback.
        assert!(parse_pg_timestamp("2026-07-28T10:00:00+00:00").is_some());
    }

    #[test]
    fn apply_archiver_output_flags_malformed_line() {
        let mut probe = PitrLiveProbe::default();
        apply_archiver_output(&mut probe, "garbage-with-no-commas");
        assert!(probe.archiver_error.is_some());
        assert!(probe.archiver_healthy.is_none());
    }

    fn progress_fixture(
        phase: queries::get_pitr_ha_workflow_progress::PitrHaWorkflowPhase,
    ) -> queries::get_pitr_ha_workflow_progress::GetPitrHaWorkflowProgressPitrHaWorkflowProgress
    {
        use queries::get_pitr_ha_workflow_progress::*;
        GetPitrHaWorkflowProgressPitrHaWorkflowProgress {
            workflow_id: "wf-1".to_string(),
            phase,
            direction: PitrHaWorkflowDirection::ENABLE,
            started_at: "2026-08-07T00:00:00Z".to_string(),
            updated_at: "2026-08-07T00:01:00Z".to_string(),
            completed_at: None,
            error_message: None,
            failed_at_phase: None,
            current_member_service_id: Some("replica-1".to_string()),
            new_leader_service_id: None,
            cluster_mutated: true,
            members: vec![
                GetPitrHaWorkflowProgressPitrHaWorkflowProgressMembers {
                    service_id: "root".to_string(),
                    service_name: "postgres".to_string(),
                    is_leader: true,
                    status: PitrHaWorkflowMemberStatus::HEALTHY,
                },
                GetPitrHaWorkflowProgressPitrHaWorkflowProgressMembers {
                    service_id: "replica-1".to_string(),
                    service_name: "postgres-replica-1".to_string(),
                    is_leader: false,
                    status: PitrHaWorkflowMemberStatus::RESTARTING,
                },
            ],
        }
    }

    #[test]
    fn build_progress_output_maps_phases_directions_and_members() {
        use queries::get_pitr_ha_workflow_progress::PitrHaWorkflowPhase;

        let root = super::super::RootContext {
            root_id: "root".to_string(),
            root_name: "postgres".to_string(),
        };
        let output = build_progress_output(
            &root,
            &progress_fixture(PitrHaWorkflowPhase::ROLLING_REPLICAS),
        );
        assert_eq!(output.phase, "rolling_replicas");
        assert_eq!(output.direction, "enable");
        assert_eq!(output.workflow_id, "wf-1");
        assert_eq!(output.members.len(), 2);
        assert!(output.members[0].is_leader);
        assert_eq!(output.members[0].status, "healthy");
        assert_eq!(output.members[1].status, "restarting");
    }

    #[test]
    fn build_progress_output_lowercases_unknown_enum_values() {
        use queries::get_pitr_ha_workflow_progress::PitrHaWorkflowPhase;

        let root = super::super::RootContext {
            root_id: "root".to_string(),
            root_name: "postgres".to_string(),
        };
        let mut progress = progress_fixture(PitrHaWorkflowPhase::Other("NEW_PHASE".to_string()));
        progress.failed_at_phase = Some(PitrHaWorkflowPhase::Other("ODD_PHASE".to_string()));
        let output = build_progress_output(&root, &progress);
        assert_eq!(output.phase, "new_phase");
        assert_eq!(output.failed_at_phase.as_deref(), Some("odd_phase"));
    }

    #[test]
    fn build_progress_output_terminal_phases_map_to_done_and_failed() {
        use queries::get_pitr_ha_workflow_progress::PitrHaWorkflowPhase;

        let root = super::super::RootContext {
            root_id: "root".to_string(),
            root_name: "postgres".to_string(),
        };
        assert_eq!(
            build_progress_output(&root, &progress_fixture(PitrHaWorkflowPhase::DONE)).phase,
            "done"
        );
        assert_eq!(
            build_progress_output(&root, &progress_fixture(PitrHaWorkflowPhase::FAILED)).phase,
            "failed"
        );
    }

    #[test]
    fn non_empty_treats_blank_and_whitespace_as_none() {
        assert_eq!(non_empty("  "), None);
        assert_eq!(non_empty(""), None);
        assert_eq!(non_empty(" value "), Some("value".to_string()));
    }

    #[test]
    fn apply_archiver_output_gates_sticky_failure_on_recency() {
        // A failure strictly before the last successful archive is stale
        // (pg_stat_archiver never clears last_failed_* on its own) -- healthy.
        let mut probe = PitrLiveProbe::default();
        apply_archiver_output(
            &mut probe,
            "5,2026-07-28 10:00:00+00,1,2026-07-27 09:00:00+00,2026-07-28 10:00:05+00",
        );
        assert_eq!(probe.archiver_healthy, Some(true));
        assert_eq!(
            probe.archiver_last_archived_at.as_deref(),
            Some("2026-07-28 10:00:00+00")
        );

        // A failure after the last successful archive is current -- unhealthy.
        let mut probe = PitrLiveProbe::default();
        apply_archiver_output(
            &mut probe,
            "5,2026-07-28 10:00:00+00,2,2026-07-28 11:00:00+00,",
        );
        assert_eq!(probe.archiver_healthy, Some(false));

        // No failure recorded at all -- healthy regardless of archive state.
        let mut probe = PitrLiveProbe::default();
        apply_archiver_output(&mut probe, "5,2026-07-28 10:00:00+00,0,,");
        assert_eq!(probe.archiver_healthy, Some(true));
    }

    #[test]
    fn apply_pgbackrest_info_extracts_backup_count_and_wal_range() {
        let output = serde_json::json!([
            {
                "backup": [
                    { "timestamp": { "start": 1700000000, "stop": 1700000100 } },
                    { "timestamp": { "start": 1700003600, "stop": 1700003700 } }
                ],
                "archive": [
                    { "min": "000000010000000000000001", "max": "000000010000000000000005" }
                ]
            }
        ])
        .to_string();

        let mut probe = PitrLiveProbe::default();
        apply_pgbackrest_info(&mut probe, &output);
        assert_eq!(probe.backup_set_count, Some(2));
        assert_eq!(probe.wal_min.as_deref(), Some("000000010000000000000001"));
        assert_eq!(probe.wal_max.as_deref(), Some("000000010000000000000005"));
        assert!(probe.latest_backup_at.is_some());
    }

    #[test]
    fn apply_pgbackrest_info_degrades_on_unparseable_output() {
        let mut probe = PitrLiveProbe::default();
        apply_pgbackrest_info(&mut probe, "not json");
        assert!(probe.backup_coverage_error.is_some());
        assert_eq!(probe.backup_set_count, None);
    }

    #[test]
    fn guardrail_blockers_lists_every_failing_check() {
        let state = PitrState {
            enabled: false,
            bucket_wired: false,
            minor_pinned: true,
            unsupported_image: false,
            has_start_command: true,
        };
        let blockers = guardrail_blockers(&state);
        assert_eq!(blockers.len(), 2);
    }
}
