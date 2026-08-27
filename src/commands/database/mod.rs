//! The shared implementation behind `railway postgres`, `railway mysql` and
//! `railway redis` -- the managed database features (point-in-time recovery,
//! high-availability clustering, connection pooling) that compose on top of an
//! existing database service.
//!
//! Each engine gets its own top-level command rather than a single
//! `railway database` with an `--engine` flag: the features an engine actually
//! has differ (Redis ships no archiver, only Postgres ships a pooler), and a
//! per-engine command is the only way `--help` can tell the truth about that.
//! The subcommand bodies live here, once, and take the engine as a parameter;
//! the per-engine files (`commands/{postgres,mysql,redis}.rs`) are just the
//! capability declarations wired to them.
//!
//! Every environment-config fetch in this module tree uses
//! `decryptVariables: true`: the non-decrypted config masks EVERY variable
//! value as null in production, and the enabled-state detection here depends
//! on values -- the HA-active variable reading "true", a non-empty archive
//! bucket, the pooler's knobs. The caller's own access already gates
//! decryption server-side.

use std::collections::BTreeMap;

use is_terminal::IsTerminal;
use serde::Serialize;

use crate::controllers::database_engines::DatabaseEngine;
use crate::controllers::{config::EnvironmentConfig, database_plugins, project::ServiceContext};
use crate::util::prompt::prompt_confirm_with_default;

use super::*;

/// Shared `{id, name}` output shape for the service/environment being acted on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResourceRef {
    pub id: String,
    pub name: String,
}

pub mod ha;
pub mod ops_log;
pub mod pgbouncer;
pub mod pitr;

/// The selectors and output mode every managed-database subcommand accepts.
/// Declared once and flattened into each engine's `Args` so the flags, their
/// shorthands and their help text cannot drift between engines.
#[derive(Parser, Clone, Default)]
pub struct Selectors {
    /// Service name or ID (defaults to linked service)
    #[clap(short, long, global = true)]
    pub service: Option<String>,

    /// Environment to use (defaults to linked environment)
    #[clap(short, long, global = true)]
    pub environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID", global = true)]
    pub project: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    pub json: bool,
}

/// A subcommand from any engine's command tree, after that tree has already
/// established the engine actually has the capability.
pub enum Action {
    Ha(ha::Args),
    Pitr(pitr::Args),
    Pooling(pgbouncer::Args),
    History(HistoryArgs),
}

#[derive(Parser)]
pub struct HistoryArgs {
    /// Maximum entries to show (newest last)
    #[clap(long, default_value_t = 50, value_parser = clap::value_parser!(usize))]
    pub limit: usize,
}

/// The one entry point every engine's command routes through: sets the output
/// mode, runs the action, translates API-mismatch errors, and records the
/// local audit trail.
pub async fn dispatch(
    engine: &'static DatabaseEngine,
    selectors: Selectors,
    action: Action,
) -> Result<()> {
    let Selectors {
        service,
        environment,
        project,
        json,
    } = selectors;

    crate::util::reporter::set_mode(json);

    // `history` only reads the local trail -- it neither needs resolution nor
    // should it append to the very log it displays.
    if let Action::History(history_args) = &action {
        return history(engine, history_args, json);
    }

    let started = std::time::Instant::now();
    let result = match action {
        Action::Ha(sub) => {
            ha::command(
                engine,
                sub,
                project.clone(),
                service.clone(),
                environment.clone(),
                json,
            )
            .await
        }
        Action::Pitr(sub) => {
            pitr::command(
                engine,
                sub,
                project.clone(),
                service.clone(),
                environment.clone(),
                json,
            )
            .await
        }
        Action::Pooling(sub) => {
            pgbouncer::command(
                engine,
                sub,
                project.clone(),
                service.clone(),
                environment.clone(),
                json,
            )
            .await
        }
        Action::History(_) => unreachable!("handled above"),
    };
    let result = result.map_err(add_api_mismatch_guidance);

    // Best-effort persistent audit trail (see ops_log): PITR, HA and pooling
    // compose, and reconstructing WHICH sequence of operations produced a
    // misconfigured database needs more than server-side command counters.
    let (project, environment, service) =
        resolved_selectors_for_log(project, service, environment).await;
    ops_log::record(
        engine,
        &ops_log::OpsLogEntry {
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
        },
    );

    result
}

/// The selectors that actually applied: explicit flags win; otherwise the
/// linked project's ids (config-file read, no network). Best-effort -- the log
/// entry still lands with whatever could be resolved.
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

fn history(engine: &DatabaseEngine, args: &HistoryArgs, json: bool) -> Result<()> {
    let entries = ops_log::read_entries(engine);
    let start = entries.len().saturating_sub(args.limit);
    let window = &entries[start..];

    if json {
        println!("{}", serde_json::to_string_pretty(window)?);
        return Ok(());
    }

    if window.is_empty() {
        println!(
            "No {} operations recorded yet (the trail lives at {}).",
            engine.key,
            ops_log::log_path(engine)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| format!("~/.railway/{}-ops.jsonl", engine.key)),
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

/// Marker phrases the backend uses (or may use in the future) in a `UserError`
/// when an operation this CLI build depends on has been removed or changed and
/// the fix is a newer CLI. Matched case-insensitively against the whole error
/// chain.
const UPGRADE_REQUIRED_MARKERS: &[&str] = &[
    "update your railway cli",
    "upgrade your railway cli",
    "update the railway cli",
    "upgrade the railway cli",
    "newer version of the railway cli",
    "railway cli is out of date",
];

/// GraphQL validation messages that mean the running binary was built against
/// a different API schema than the server is exposing -- an operation or field
/// this command depends on no longer exists (removed, renamed, or
/// re-internalized server-side).
fn is_schema_mismatch_message(lower_chain: &str) -> bool {
    lower_chain.contains("cannot query field")
        || lower_chain.contains("is not defined by type")
        || lower_chain.contains("unknown argument")
        || lower_chain.contains("unknown field")
}

/// These commands drive API operations the backend reserves the right to
/// evolve (they were exposed on the public subgraph specifically for this
/// CLI). When one disappears or the backend explicitly asks for a newer CLI,
/// translate the raw GraphQL error into actionable guidance instead of a
/// cryptic validation dump. Every other error passes through untouched.
pub(crate) fn add_api_mismatch_guidance(err: anyhow::Error) -> anyhow::Error {
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
pub(crate) fn confirm_or_bail(message: &str, yes: bool) -> Result<bool> {
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

/// Service id -> name lookup, used to label cluster members (which are only
/// identified by id in `environment.config`).
pub(crate) fn service_name_map(ctx: &ServiceContext) -> BTreeMap<String, String> {
    ctx.project
        .services
        .edges
        .iter()
        .map(|edge| (edge.node.id.clone(), edge.node.name.clone()))
        .collect()
}

/// The resolved cluster/standalone root for `ctx.service_id` -- if the
/// resolved service is an edge child (a pooler or the cluster's router), this
/// follows `parentServiceId` back to the actual database root.
pub(crate) struct RootContext {
    pub root_id: String,
    pub root_name: String,
}

pub(crate) const FIELD_LABEL_WIDTH: usize = 20;

/// Fixed-width field printer, matching `cdn.rs`'s status output convention.
pub(crate) fn print_field(label: &str, value: &dyn std::fmt::Display) {
    let padded = format!("{label:<FIELD_LABEL_WIDTH$}");
    println!("{} {value}", padded.dimmed());
}

pub(crate) fn status_label(enabled: bool) -> colored::ColoredString {
    if enabled {
        "enabled".green().bold()
    } else {
        "disabled".yellow().bold()
    }
}

pub(crate) fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

pub(crate) fn resolve_root(ctx: &ServiceContext, config: &EnvironmentConfig) -> RootContext {
    let root_id = database_plugins::resolve_root_service_id(config, &ctx.service_id);
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
}
