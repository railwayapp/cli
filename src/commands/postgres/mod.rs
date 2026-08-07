//! `railway postgres {pitr,ha,pgbouncer}` -- CLI parity for the three biggest
//! Postgres-plugin features (continuous backups/point-in-time recovery, high
//! availability clustering, and PgBouncer connection pooling). Nested under a
//! single `postgres` command (rather than three flat top-level commands) to
//! match how customers think about these features and mirror existing
//! nesting precedent (`railway service source connect/disconnect`, `railway
//! service files ...`).
//!
//! Every environment-config fetch in this module tree uses
//! `decryptVariables: true`: the non-decrypted config masks EVERY variable
//! value as null in production (confirmed live 2026-08-07), and the
//! enabled-state detection here depends on values -- `PATRONI_ENABLED ==
//! "true"`, a non-empty `WAL_ARCHIVE_BUCKET`, PgBouncer's pool knobs. The
//! caller's own access already gates decryption server-side.

use std::collections::BTreeMap;

use is_terminal::IsTerminal;
use serde::Serialize;

use crate::controllers::{config::EnvironmentConfig, postgres_plugins, project::ServiceContext};
use crate::util::prompt::prompt_confirm_with_default;

use super::*;

/// Shared `{id, name}` output shape for the service/environment being acted on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ResourceRef {
    pub id: String,
    pub name: String,
}

pub mod ha;
pub mod ops_log;
pub mod pgbouncer;
pub mod pitr;

/// Manage Postgres plugin features: point-in-time recovery, high availability, and connection pooling
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway postgres pitr status --service postgres\n  railway postgres pitr enable --service postgres\n  railway postgres ha status --service postgres\n  railway postgres ha convert --service postgres --replicas 2\n  railway postgres pgbouncer add --service postgres --pool-mode transaction\n\nAutomation notes:\n  --service/--environment/--project/--json apply to every subcommand below `railway postgres`.\n  Actions that change config (enable/disable/convert/revert/add/remove/configure/scale) commit and deploy by default; pass --no-deploy to commit the config change without triggering deploys (it then applies on each affected service's next deploy)."
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
    /// Manage point-in-time recovery (continuous backups)
    Pitr(pitr::Args),

    /// Manage high-availability clustering
    Ha(ha::Args),

    /// Manage PgBouncer connection pooling
    Pgbouncer(pgbouncer::Args),

    /// Show the local audit trail of postgres operations
    History(HistoryArgs),
}

#[derive(Parser)]
struct HistoryArgs {
    /// Maximum entries to show (newest last)
    #[clap(long, default_value_t = 50, value_parser = clap::value_parser!(usize))]
    limit: usize,
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

    // `history` only reads the local trail -- it neither needs resolution
    // nor should it append to the very log it displays.
    if let Commands::History(history_args) = &command {
        return history(history_args, json);
    }

    let started = std::time::Instant::now();
    let result = match command {
        Commands::Pitr(sub) => {
            pitr::command(
                sub,
                project.clone(),
                service.clone(),
                environment.clone(),
                json,
            )
            .await
        }
        Commands::Ha(sub) => {
            ha::command(
                sub,
                project.clone(),
                service.clone(),
                environment.clone(),
                json,
            )
            .await
        }
        Commands::Pgbouncer(sub) => {
            pgbouncer::command(
                sub,
                project.clone(),
                service.clone(),
                environment.clone(),
                json,
            )
            .await
        }
        Commands::History(_) => unreachable!("handled above"),
    };
    let result = result.map_err(add_api_mismatch_guidance);

    // Best-effort persistent audit trail (see ops_log): PITR/HA/PgBouncer
    // compose, and reconstructing WHICH sequence of operations produced a
    // misconfigured Postgres needs more than server-side command counters.
    let (project, environment, service) =
        resolved_selectors_for_log(project, service, environment).await;
    ops_log::record(&ops_log::OpsLogEntry {
        timestamp: chrono::Utc::now(),
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
        args: std::env::args().skip(1).collect(),
        project,
        environment,
        service,
        success: result.is_ok(),
        error: result.as_ref().err().map(|e| {
            let message = format!("{e:#}");
            if message.len() > 512 {
                message[..512].to_string()
            } else {
                message
            }
        }),
        duration_ms: started.elapsed().as_millis() as u64,
    });

    result
}

/// The selectors that actually applied: explicit flags win; otherwise the
/// linked project's ids (config-file read, no network). Best-effort -- the
/// log entry still lands with whatever could be resolved.
async fn resolved_selectors_for_log(
    project: Option<String>,
    service: Option<String>,
    environment: Option<String>,
) -> (Option<String>, Option<String>, Option<String>) {
    if project.is_some() && environment.is_some() && service.is_some() {
        return (project, environment, service);
    }
    let linked = match crate::config::Configs::new() {
        Ok(configs) => configs.get_linked_project().await.ok(),
        Err(_) => None,
    };
    (
        project.or_else(|| linked.as_ref().map(|l| l.project.clone())),
        environment.or_else(|| linked.as_ref().and_then(|l| l.environment.clone())),
        service.or_else(|| linked.as_ref().and_then(|l| l.service.clone())),
    )
}

fn history(args: &HistoryArgs, json: bool) -> Result<()> {
    let entries = ops_log::read_entries();
    let start = entries.len().saturating_sub(args.limit);
    let window = &entries[start..];

    if json {
        println!("{}", serde_json::to_string_pretty(window)?);
        return Ok(());
    }

    if window.is_empty() {
        println!(
            "No postgres operations recorded yet (the trail lives at {}).",
            ops_log::log_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "~/.railway/postgres-ops.jsonl".to_string())
        );
        return Ok(());
    }

    println!(
        "{:<21} {:<7} {:<9} {:<37} COMMAND",
        "WHEN (UTC)", "OUTCOME", "DURATION", "PROJECT/SERVICE"
    );
    for entry in window {
        let outcome = if entry.success {
            "ok".green().to_string()
        } else {
            "FAIL".red().to_string()
        };
        let target = format!(
            "{}/{}",
            entry.project.as_deref().unwrap_or("-"),
            entry.service.as_deref().unwrap_or("-")
        );
        let target = if target.len() > 37 {
            format!("{}…", &target[..36])
        } else {
            target
        };
        println!(
            "{:<21} {:<7} {:<9} {:<37} railway {}",
            entry.timestamp.format("%Y-%m-%d %H:%M:%S"),
            outcome,
            format!("{}ms", entry.duration_ms),
            target,
            entry.args.join(" ")
        );
        if let Some(error) = &entry.error {
            println!("{:<40} {}", "", error.lines().next().unwrap_or("").red());
        }
    }
    Ok(())
}

/// Marker phrases the backend uses (or may use in the future) in a
/// `UserError` when an operation this CLI build depends on has been
/// removed or changed and the fix is a newer CLI. Matched
/// case-insensitively against the whole error chain.
const UPGRADE_REQUIRED_MARKERS: &[&str] = &[
    "update your railway cli",
    "upgrade your railway cli",
    "update the railway cli",
    "upgrade the railway cli",
    "newer version of the railway cli",
    "railway cli is out of date",
];

/// GraphQL validation messages that mean the running binary was built
/// against a different API schema than the server is exposing -- an
/// operation or field this command depends on no longer exists (removed,
/// renamed, or re-internalized server-side).
fn is_schema_mismatch_message(lower_chain: &str) -> bool {
    lower_chain.contains("cannot query field")
        || lower_chain.contains("is not defined by type")
        || lower_chain.contains("unknown argument")
        || lower_chain.contains("unknown field")
}

/// `railway postgres` drives API operations that the backend reserves the
/// right to evolve (they were exposed on the public subgraph specifically
/// for this CLI). When one disappears or the backend explicitly asks for a
/// newer CLI, translate the raw GraphQL error into actionable guidance
/// instead of a cryptic validation dump. Every other error passes through
/// untouched.
pub(super) fn add_api_mismatch_guidance(err: anyhow::Error) -> anyhow::Error {
    let lower_chain = format!("{err:#}").to_ascii_lowercase();

    if UPGRADE_REQUIRED_MARKERS
        .iter()
        .any(|marker| lower_chain.contains(marker))
    {
        return err.context(
            "The Railway API requires a newer CLI for this command. Update with `railway upgrade` (or your package manager) and try again.",
        );
    }

    if is_schema_mismatch_message(&lower_chain) {
        return err.context(
            "This CLI build no longer matches the Railway API -- an operation this command depends on is missing or has changed. Update with `railway upgrade` and try again; if the latest CLI still fails, the operation may have been removed (check the Railway changelog).",
        );
    }

    err
}

/// Shared confirm-before-mutating helper: `--yes` bypasses the prompt; a
/// non-TTY session without `--yes` fails loudly instead of hanging, matching
/// `tcp_proxy.rs delete`'s convention.
pub(super) fn confirm_or_bail(message: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if std::io::stdout().is_terminal() {
        prompt_confirm_with_default(message, false)
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    }
}

/// Service id -> name lookup, used to label HA cluster members (which are
/// only identified by id in `environment.config`).
pub(super) fn service_name_map(ctx: &ServiceContext) -> BTreeMap<String, String> {
    ctx.project
        .services
        .edges
        .iter()
        .map(|edge| (edge.node.id.clone(), edge.node.name.clone()))
        .collect()
}

/// The resolved cluster/standalone root for `ctx.service_id` -- if the
/// resolved service is a PgBouncer/HAProxy edge child, this follows
/// `parentServiceId` back to the actual database root (mirrors
/// `PgBouncerSection.tsx`'s `templateRootServiceId`).
pub(super) struct RootContext {
    pub root_id: String,
    pub root_name: String,
}

pub(super) const FIELD_LABEL_WIDTH: usize = 20;

/// Fixed-width field printer, matching `cdn.rs`'s status output convention.
pub(super) fn print_field(label: &str, value: &dyn std::fmt::Display) {
    let padded = format!("{label:<FIELD_LABEL_WIDTH$}");
    println!("{} {value}", padded.dimmed());
}

pub(super) fn status_label(enabled: bool) -> colored::ColoredString {
    if enabled {
        "enabled".green().bold()
    } else {
        "disabled".yellow().bold()
    }
}

pub(super) fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

pub(super) fn resolve_root(ctx: &ServiceContext, config: &EnvironmentConfig) -> RootContext {
    let root_id = postgres_plugins::resolve_root_service_id(config, &ctx.service_id);
    let root_name = if root_id == ctx.service_id {
        ctx.service_name.clone()
    } else {
        service_name_map(ctx)
            .get(&root_id)
            .cloned()
            .unwrap_or_else(|| root_id.clone())
    };
    RootContext { root_id, root_name }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_feature_subcommands() {
        assert!(matches!(
            Args::parse_from(["postgres", "pitr", "status"]).command,
            Commands::Pitr(_)
        ));
        assert!(matches!(
            Args::parse_from(["postgres", "ha", "status"]).command,
            Commands::Ha(_)
        ));
        assert!(matches!(
            Args::parse_from(["postgres", "pgbouncer", "status"]).command,
            Commands::Pgbouncer(_)
        ));
    }

    #[test]
    fn parses_history_with_limit() {
        let args = Args::parse_from(["postgres", "history"]);
        assert!(matches!(
            args.command,
            Commands::History(HistoryArgs { limit: 50 })
        ));
        let args = Args::parse_from(["postgres", "history", "--limit", "5"]);
        assert!(matches!(
            args.command,
            Commands::History(HistoryArgs { limit: 5 })
        ));
    }

    #[test]
    fn api_mismatch_guidance_translates_missing_field_validation_errors() {
        // Real message shape from the public gateway when a mutation this
        // build uses is not on the Public subgraph.
        let err = anyhow::anyhow!(
            "Cannot query field \"volumeInstanceBackupCreateForHaConversion\" on type \"Mutation\"."
        )
        .context("Failed to enable PITR");
        let wrapped = add_api_mismatch_guidance(err);
        assert!(format!("{wrapped:#}").contains("railway upgrade"));

        // Input-field removal shape ("Field X is not defined by type Y").
        let err = anyhow::anyhow!(
            "Variable \"$input\" got invalid value; Field \"stageOnly\" is not defined by type \"TemplateDeployV2Input\"."
        );
        let wrapped = add_api_mismatch_guidance(err);
        assert!(format!("{wrapped:#}").contains("no longer matches the Railway API"));
    }

    #[test]
    fn api_mismatch_guidance_surfaces_explicit_upgrade_user_errors() {
        // If the backend ever retires one of these routes it throws a
        // UserError telling the caller to update -- the CLI must lead with
        // actionable guidance, not a bare GraphQL error.
        let err = anyhow::anyhow!(
            "This operation has moved. Please update your Railway CLI to continue managing PITR."
        )
        .context("Failed to enable PITR");
        let wrapped = add_api_mismatch_guidance(err);
        let rendered = format!("{wrapped:#}");
        assert!(rendered.contains("requires a newer CLI"));
        assert!(rendered.contains("railway upgrade"));
        // The server's own message stays visible in the chain.
        assert!(rendered.contains("This operation has moved"));
    }

    #[test]
    fn api_mismatch_guidance_passes_unrelated_errors_through() {
        let err = anyhow::anyhow!("Problem processing request").context("Failed to enable PITR");
        let before = format!("{err:#}");
        let after = format!("{:#}", add_api_mismatch_guidance(err));
        assert_eq!(before, after);

        let err = anyhow::anyhow!("connection reset by peer");
        let after = format!("{:#}", add_api_mismatch_guidance(err));
        assert_eq!(after, "connection reset by peer");
    }

    #[test]
    fn global_selectors_are_accepted_before_and_after_the_subcommand() {
        let args = Args::parse_from([
            "postgres",
            "--project",
            "project-id",
            "--environment",
            "production",
            "--service",
            "web",
            "--json",
            "pitr",
            "status",
        ]);
        assert_eq!(args.project.as_deref(), Some("project-id"));
        assert_eq!(args.environment.as_deref(), Some("production"));
        assert_eq!(args.service.as_deref(), Some("web"));
        assert!(args.json);

        let args = Args::parse_from(["postgres", "ha", "status", "--service", "web", "--json"]);
        assert_eq!(args.service.as_deref(), Some("web"));
        assert!(args.json);
    }
}
