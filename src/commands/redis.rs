//! `railway redis` -- the managed Redis features: high-availability
//! clustering (Sentinel colocated on the data nodes, behind a routing proxy).
//!
//! Only the capability set lives here; the subcommand bodies are the shared
//! implementation in [`crate::commands::database`]. Redis offers HA alone:
//! no Redis image ships a continuous archiver, so there is no point-in-time
//! recovery surface to expose, and no Redis pooler companion ships either.

use crate::controllers::database_engines::REDIS;

use super::database::{self, Action, HistoryArgs, Selectors};
use super::*;

/// Manage Redis features: high availability
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway redis ha status --service redis\n  railway redis ha convert --service redis --replicas 2\n  railway redis ha scale --service redis --replicas 4\n  railway redis ha switchover --service redis --to Redis-2\n\nAutomation notes:\n  --service/--environment/--project/--json apply to every subcommand below `railway redis`.\n  Actions that change config (convert/revert/scale) commit and deploy by default; pass --no-deploy to commit the config change without triggering deploys (it then applies on each affected service's next deploy).\n  Redis clusters carry the failover vote on the data nodes themselves, so their total must be odd and at least three -- pass an even --replicas.\n  Conversion pins every node to the source image's exact major.minor version, so the service must already run a minor-tagged image."
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

    /// Show the local audit trail of Redis operations
    History(HistoryArgs),
}

pub async fn command(args: Args) -> Result<()> {
    let action = match args.command {
        Commands::Ha(sub) => Action::Ha(sub),
        Commands::History(sub) => Action::History(sub),
    };
    database::dispatch(&REDIS, args.selectors, action).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_the_capabilities_redis_actually_ships() {
        assert!(matches!(
            Args::parse_from(["redis", "ha", "status"]).command,
            Commands::Ha(_)
        ));
        assert!(matches!(
            Args::parse_from(["redis", "history"]).command,
            Commands::History(_)
        ));
    }

    #[test]
    fn no_pitr_or_pooling_subcommands_are_offered() {
        // No Redis image ships a continuous archiver and no Redis pooler
        // companion ships, so neither surface exists. Advertising them in
        // --help and refusing at runtime would be worse than not having them.
        assert!(Args::try_parse_from(["redis", "pitr", "status"]).is_err());
        assert!(Args::try_parse_from(["redis", "pgbouncer", "status"]).is_err());
    }

    #[test]
    fn global_selectors_are_accepted_before_and_after_the_subcommand() {
        let args = Args::parse_from([
            "redis",
            "--project",
            "project-id",
            "--environment",
            "production",
            "--service",
            "cache",
            "--json",
            "ha",
            "status",
        ]);
        assert_eq!(args.selectors.project.as_deref(), Some("project-id"));
        assert_eq!(args.selectors.environment.as_deref(), Some("production"));
        assert_eq!(args.selectors.service.as_deref(), Some("cache"));
        assert!(args.selectors.json);

        let args = Args::parse_from(["redis", "ha", "status", "--service", "cache", "--json"]);
        assert_eq!(args.selectors.service.as_deref(), Some("cache"));
        assert!(args.selectors.json);
    }
}
