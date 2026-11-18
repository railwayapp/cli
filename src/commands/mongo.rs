//! `railway mongo` -- the managed MongoDB features: high-availability
//! clustering (a replica set whose elections run inside mongod, behind a
//! routing proxy).
//!
//! Only the capability set lives here; the subcommand bodies are the shared
//! implementation in [`crate::commands::database`]. MongoDB offers HA alone:
//! no mongo image ships a continuous archiver, so there is no point-in-time
//! recovery surface to expose, and no mongo pooler companion ships either.

use crate::controllers::database_engines::MONGO;

use super::database::{self, Action, HistoryArgs, Selectors};
use super::*;

/// Manage MongoDB features: high availability
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway mongo ha status --service mongodb\n  railway mongo ha convert --service mongodb --replicas 2\n  railway mongo ha scale --service mongodb --replicas 4\n  railway mongo ha switchover --service mongodb --to MongoDB-2\n\nAutomation notes:\n  --service/--environment/--project/--json apply to every subcommand below `railway mongo`.\n  Actions that change config (convert/revert/scale) commit and deploy by default; pass --no-deploy to commit the config change without triggering deploys (it then applies on each affected service's next deploy).\n  MongoDB replica sets carry the election vote on the data nodes themselves, so their total must be odd and at least three -- pass an even --replicas.\n  Conversion pins every node to the source image's exact major.minor version, so the service must already run a minor-tagged image."
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

    /// Show the local audit trail of MongoDB operations
    History(HistoryArgs),
}

pub async fn command(args: Args) -> Result<()> {
    let action = match args.command {
        Commands::Ha(sub) => Action::Ha(sub),
        Commands::History(sub) => Action::History(sub),
    };
    database::dispatch(&MONGO, args.selectors, action).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_the_capabilities_mongo_actually_ships() {
        assert!(matches!(
            Args::parse_from(["mongo", "ha", "status"]).command,
            Commands::Ha(_)
        ));
        assert!(matches!(
            Args::parse_from(["mongo", "history"]).command,
            Commands::History(_)
        ));
    }

    #[test]
    fn every_ha_subcommand_the_other_engines_expose_parses() {
        for argv in [
            vec!["mongo", "ha", "status"],
            vec!["mongo", "ha", "convert", "--replicas", "2"],
            vec!["mongo", "ha", "revert"],
            vec!["mongo", "ha", "scale", "--replicas", "4"],
            vec!["mongo", "ha", "switchover", "--to", "MongoDB-2"],
        ] {
            assert!(
                matches!(
                    Args::try_parse_from(&argv).map(|a| a.command),
                    Ok(Commands::Ha(_))
                ),
                "{argv:?} should parse"
            );
        }
    }

    #[test]
    fn no_pitr_or_pooling_subcommands_are_offered() {
        // No mongo image ships a continuous archiver and no mongo pooler
        // companion ships, so neither surface exists. Advertising them in
        // --help and refusing at runtime would be worse than not having them.
        assert!(Args::try_parse_from(["mongo", "pitr", "status"]).is_err());
        assert!(Args::try_parse_from(["mongo", "pgbouncer", "status"]).is_err());
    }

    #[test]
    fn global_selectors_are_accepted_before_and_after_the_subcommand() {
        let args = Args::parse_from([
            "mongo",
            "--project",
            "project-id",
            "--environment",
            "production",
            "--service",
            "mongodb",
            "--json",
            "ha",
            "status",
        ]);
        assert_eq!(args.selectors.project.as_deref(), Some("project-id"));
        assert_eq!(args.selectors.environment.as_deref(), Some("production"));
        assert_eq!(args.selectors.service.as_deref(), Some("mongodb"));
        assert!(args.selectors.json);

        let args = Args::parse_from(["mongo", "ha", "status", "--service", "mongodb", "--json"]);
        assert_eq!(args.selectors.service.as_deref(), Some("mongodb"));
        assert!(args.selectors.json);
    }
}
