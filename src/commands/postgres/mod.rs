//! `railway postgres {pitr,ha,pgbouncer}` -- CLI parity for the three biggest
//! Postgres-plugin features (continuous backups/point-in-time recovery, high
//! availability clustering, and PgBouncer connection pooling). Nested under a
//! single `postgres` command (rather than three flat top-level commands) to
//! match how customers think about these features and mirror existing
//! nesting precedent (`railway service source connect/disconnect`, `railway
//! service files ...`).

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
        Commands::Pitr(sub) => pitr::command(sub, project, service, environment, json).await,
        Commands::Ha(sub) => ha::command(sub, project, service, environment, json).await,
        Commands::Pgbouncer(sub) => {
            pgbouncer::command(sub, project, service, environment, json).await
        }
    }
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
