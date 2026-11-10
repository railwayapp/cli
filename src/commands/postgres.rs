//! `railway postgres` -- the managed Postgres features: point-in-time
//! recovery, high-availability clustering, and PgBouncer connection pooling.
//!
//! Only the capability set lives here; every subcommand body is the shared
//! implementation in [`crate::commands::database`], which takes the engine as
//! a parameter. Postgres is the one engine with all three surfaces, so this
//! tree is the widest of them.

use crate::controllers::database_engines::POSTGRES;

use super::database::{self, Action, HistoryArgs, Selectors};
use super::*;

/// Manage Postgres features: point-in-time recovery, high availability, and connection pooling
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway postgres pitr status --service postgres\n  railway postgres pitr enable --service postgres\n  railway postgres ha status --service postgres\n  railway postgres ha convert --service postgres --replicas 2\n  railway postgres pgbouncer add --service postgres --pool-mode transaction\n\nAutomation notes:\n  --service/--environment/--project/--json apply to every subcommand below `railway postgres`.\n  Actions that change config (enable/disable/convert/revert/add/remove/configure/scale) commit and deploy by default; pass --no-deploy to commit the config change without triggering deploys (it then applies on each affected service's next deploy)."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    #[clap(flatten)]
    selectors: Selectors,
}

#[derive(Parser)]
enum Commands {
    /// Manage point-in-time recovery (continuous backups)
    Pitr(database::pitr::Args),

    /// Manage high-availability clustering
    Ha(database::ha::Args),

    /// Manage PgBouncer connection pooling
    Pgbouncer(database::pool::Args),

    /// Show the local audit trail of Postgres operations
    History(HistoryArgs),
}

pub async fn command(args: Args) -> Result<()> {
    let action = match args.command {
        Commands::Pitr(sub) => Action::Pitr(sub),
        Commands::Ha(sub) => Action::Ha(sub),
        Commands::Pgbouncer(sub) => Action::Pooling(sub),
        Commands::History(sub) => Action::History(sub),
    };
    database::dispatch(&POSTGRES, args.selectors, action).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_every_feature_subcommand() {
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
        assert!(matches!(
            Args::parse_from(["postgres", "history"]).command,
            Commands::History(HistoryArgs { limit: 50 })
        ));
        assert!(matches!(
            Args::parse_from(["postgres", "history", "--limit", "5"]).command,
            Commands::History(HistoryArgs { limit: 5 })
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
        assert_eq!(args.selectors.project.as_deref(), Some("project-id"));
        assert_eq!(args.selectors.environment.as_deref(), Some("production"));
        assert_eq!(args.selectors.service.as_deref(), Some("web"));
        assert!(args.selectors.json);

        let args = Args::parse_from(["postgres", "ha", "status", "--service", "web", "--json"]);
        assert_eq!(args.selectors.service.as_deref(), Some("web"));
        assert!(args.selectors.json);
    }
}
