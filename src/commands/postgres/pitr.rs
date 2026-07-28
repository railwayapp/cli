//! `railway postgres pitr` -- point-in-time recovery / continuous backups.

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;
use serde::Serialize;

use crate::{
    client::post_graphql,
    controllers::{
        config::{EnvironmentConfig, fetch_environment_config},
        postgres_plugins::{self, PitrState},
        project::resolve_service_context,
        template_apply::{
            self, ApplyKind, ApplyTemplateParams, PITR_TEMPLATE_CODE, RevertTemplateParams,
        },
    },
    gql::{mutations, queries},
};

use super::{
    ResourceRef, confirm_or_bail, not_yet_implemented, print_field, resolve_root, service_name_map,
    status_label, yes_no,
};

/// Manage point-in-time recovery (continuous backups) for Postgres
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway postgres pitr status --service postgres\n  railway postgres pitr enable --service postgres\n  railway postgres pitr disable --service postgres --yes\n  railway postgres pitr restore --service postgres --at 2026-07-20T12:00:00Z\n  railway postgres pitr backup create --service postgres --name pre-migration\n  railway postgres pitr schedule set --daily --weekly\n\nAutomation notes:\n  <time> for `restore` accepts RFC3339 (2026-07-20T12:00:00Z) or `YYYY-MM-DD HH:MM:SS` (UTC).\n  `enable`/`disable` auto-detect whether the target is a standalone Postgres or the root of an HA cluster.\n  `progress`/`cancel`/`clear` only apply to HA clusters (the rolling enable/disable workflow)."
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
    /// Stage the change without deploying it immediately (standalone only; HA
    /// enable always applies live, it has no staging step)
    #[clap(long)]
    no_deploy: bool,
}

#[derive(Parser)]
struct DisableArgs {
    /// Skip the confirmation prompt
    #[clap(long, short = 'y')]
    yes: bool,

    /// Stage the change without deploying it immediately (standalone only; HA
    /// disable always applies live, it has no staging step)
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
    /// Point in time to restore to: RFC3339 (2026-07-20T12:00:00Z) or "YYYY-MM-DD HH:MM:SS" (UTC)
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

    /// Trigger an immediate run of a missed scheduled backup
    Trigger(BackupTriggerArgs),
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
struct BackupTriggerArgs {
    /// Backup schedule ID to trigger (see `backup schedule list`)
    #[clap(long = "schedule-id")]
    schedule_id: Option<String>,
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
        .args(["daily", "weekly", "monthly"])
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
        Commands::Progress(_) => not_yet_implemented("pitr progress"),
        Commands::Cancel => cancel(project, service, environment, json).await,
        Commands::Clear => clear(project, service, environment, json).await,
        Commands::Restore(_) => not_yet_implemented("pitr restore"),
        Commands::Backup(a) => match a.command {
            BackupCommands::List => not_yet_implemented("pitr backup list"),
            BackupCommands::Create(_) => not_yet_implemented("pitr backup create"),
            BackupCommands::Delete(_) => not_yet_implemented("pitr backup delete"),
            BackupCommands::Lock(_) => not_yet_implemented("pitr backup lock"),
            BackupCommands::Restore(_) => not_yet_implemented("pitr backup restore"),
            BackupCommands::Trigger(_) => not_yet_implemented("pitr backup trigger"),
        },
        Commands::Schedule(a) => match a.command {
            ScheduleCommands::Set(_) => not_yet_implemented("pitr schedule set"),
            ScheduleCommands::List => not_yet_implemented("pitr schedule list"),
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
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    print_status(&ctx, &config, json)
}

fn print_status(
    ctx: &crate::controllers::project::ServiceContext,
    config: &EnvironmentConfig,
    json: bool,
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
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
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
        return print_status(&ctx, &config, json);
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

        if let Some(workflow_id) = response.enable_pitr_for_ha_cluster.workflow_id {
            crate::controllers::workflow::wait_for_workflow(&ctx.client, &ctx.configs, workflow_id)
                .await?;
        }
        if !json {
            println!(
                "Enabled PITR for {} in environment {}.",
                root.root_name.bold(),
                ctx.environment_name.bold()
            );
        }
    } else {
        let volume_instance_id = target_service.volume_mounts.keys().next().cloned();
        let result = template_apply::apply_composable_template(
            &ctx,
            ApplyTemplateParams {
                template_code: PITR_TEMPLATE_CODE.to_string(),
                service_id: root.root_id.clone(),
                volume_instance_id,
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
                "Staged (not yet deployed)"
            };
            println!(
                "{verb} PITR for {} in environment {} (project {}).",
                root.root_name.bold(),
                ctx.environment_name.bold(),
                result.project_id
            );
        }
    }

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    print_status(&ctx, &config, json)
}

async fn disable(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
    args: DisableArgs,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
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

        if let Some(workflow_id) = response.disable_pitr_for_ha_cluster.workflow_id {
            crate::controllers::workflow::wait_for_workflow(&ctx.client, &ctx.configs, workflow_id)
                .await?;
        }
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
                "Staged the disable (not yet deployed)"
            };
            println!(
                "{verb} for PITR on {} in environment {} (project {}).",
                root.root_name.bold(),
                ctx.environment_name.bold(),
                result.project_id
            );
        }
    }

    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
        .await?
        .config;
    print_status(&ctx, &config, json)
}

async fn cancel(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
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
        println!("{}", serde_json::json!({"cancelled": true}));
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
    let config = fetch_environment_config(&ctx.client, &ctx.configs, &ctx.environment_id, false)
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
        println!("{}", serde_json::json!({"cleared": true}));
    } else {
        println!(
            "Cleared PITR workflow progress for {}.",
            root.root_name.bold()
        );
    }
    Ok(())
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
