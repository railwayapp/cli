//! `railway mysql` -- the managed MySQL features: high-availability
//! clustering, Group Replication behind a routing proxy.
//!
//! Only the capability set lives here; the subcommand bodies are the shared
//! implementation in [`crate::commands::database`]. There is no pooling
//! subcommand because no MySQL pooler companion ships.

use crate::controllers::database_engines::MYSQL;

use super::database::{self, Action, HistoryArgs, Selectors};
use super::*;

/// Manage MySQL features: high availability
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway mysql ha status --service mysql\n  railway mysql ha convert --service mysql --replicas 2\n  railway mysql ha scale --service mysql --replicas 4\n  railway mysql ha switchover --service mysql --to MySQL-2\n\nAutomation notes:\n  --service/--environment/--project/--json apply to every subcommand below `railway mysql`.\n  Actions that change config (convert/revert/scale) commit and deploy by default; pass --no-deploy to commit the config change without triggering deploys (it then applies on each affected service's next deploy).\n  MySQL clusters carry the failover vote on the data nodes themselves, so their total must be odd and at least three -- pass an even --replicas.\n  Conversion pins every node to the source image's exact major.minor version, so the service must already run a minor-tagged image."
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

    /// Show the local audit trail of MySQL operations
    History(HistoryArgs),
}

pub async fn command(args: Args) -> Result<()> {
    let action = match args.command {
        Commands::Ha(sub) => Action::Ha(sub),
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

        let args = Args::parse_from(["mysql", "ha", "status", "--service", "db", "--json"]);
        assert_eq!(args.selectors.service.as_deref(), Some("db"));
        assert!(args.selectors.json);
    }
}
