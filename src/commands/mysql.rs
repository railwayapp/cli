//! `railway mysql` -- the managed MySQL features: high-availability
//! clustering (Group Replication behind a routing proxy) and point-in-time
//! recovery (binlog archiving).
//!
//! Only the capability set lives here; every subcommand body is the shared
//! implementation in [`crate::commands::database`]. There is no pooling
//! subcommand because no MySQL pooler companion ships, and MySQL's PITR is
//! standalone-only -- its archiver refuses to run while the cluster's seed
//! list is set -- which the shared PITR tree enforces from the engine's own
//! declaration rather than from anything stated here.

use crate::controllers::database_engines::MYSQL;

use super::database::{self, Action, HistoryArgs, Selectors};
use super::*;

/// Manage MySQL features: high availability and point-in-time recovery
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway mysql ha status --service mysql\n  railway mysql ha convert --service mysql --replicas 2\n  railway mysql ha switchover --service mysql --to MySQL-2\n  railway mysql pitr enable --service mysql\n  railway mysql pitr restore --service mysql --at 2026-07-20T12:00:00Z\n\nAutomation notes:\n  --service/--environment/--project/--json apply to every subcommand below `railway mysql`.\n  Actions that change config (enable/disable/convert/revert/scale) commit and deploy by default; pass --no-deploy to commit the config change without triggering deploys (it then applies on each affected service's next deploy).\n  MySQL clusters carry the failover vote on the data nodes themselves, so their total must be odd and at least three -- pass an even --replicas.\n  Point-in-time recovery is standalone-only on MySQL: it cannot be enabled on an HA cluster."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    #[clap(flatten)]
    selectors: Selectors,
}

#[derive(Parser)]
enum Commands {
    /// Manage high-availability clustering
    Ha(database::ha::Args),

    /// Manage point-in-time recovery (continuous backups)
    Pitr(database::pitr::Args),

    /// Show the local audit trail of MySQL operations
    History(HistoryArgs),
}

pub async fn command(args: Args) -> Result<()> {
    let action = match args.command {
        Commands::Ha(sub) => Action::Ha(sub),
        Commands::Pitr(sub) => Action::Pitr(sub),
        Commands::History(sub) => Action::History(sub),
    };
    database::dispatch(&MYSQL, args.selectors, action).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_the_capabilities_mysql_actually_ships() {
        assert!(matches!(
            Args::parse_from(["mysql", "ha", "status"]).command,
            Commands::Ha(_)
        ));
        assert!(matches!(
            Args::parse_from(["mysql", "pitr", "status"]).command,
            Commands::Pitr(_)
        ));
        assert!(matches!(
            Args::parse_from(["mysql", "history"]).command,
            Commands::History(_)
        ));
    }

    #[test]
    fn no_pooling_subcommand_is_offered() {
        // No MySQL pooler companion ships, so the subcommand must not exist
        // at all -- advertising it in --help and refusing at runtime would be
        // worse than not having it.
        assert!(Args::try_parse_from(["mysql", "pgbouncer", "status"]).is_err());
    }

    #[test]
    fn global_selectors_are_accepted_before_and_after_the_subcommand() {
        let args = Args::parse_from([
            "mysql",
            "--project",
            "project-id",
            "--environment",
            "production",
            "--service",
            "db",
            "--json",
            "ha",
            "status",
        ]);
        assert_eq!(args.selectors.project.as_deref(), Some("project-id"));
        assert_eq!(args.selectors.environment.as_deref(), Some("production"));
        assert_eq!(args.selectors.service.as_deref(), Some("db"));
        assert!(args.selectors.json);

        let args = Args::parse_from(["mysql", "pitr", "status", "--service", "db", "--json"]);
        assert_eq!(args.selectors.service.as_deref(), Some("db"));
        assert!(args.selectors.json);
    }
}
