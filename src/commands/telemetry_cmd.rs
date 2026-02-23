use super::*;
use crate::telemetry::{Preferences, is_telemetry_disabled_by_env};

/// Manage telemetry preferences
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Parser)]
enum Commands {
    /// Enable telemetry data collection
    Enable,
    /// Disable telemetry data collection
    Disable,
    /// Show current telemetry status
    Status,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Commands::Enable => {
            let mut prefs = Preferences::read();
            prefs.telemetry_disabled = false;
            prefs.write();
            println!("{}", "Telemetry enabled.".green());
        }
        Commands::Disable => {
            let mut prefs = Preferences::read();
            prefs.telemetry_disabled = true;
            prefs.write();
            println!("{}", "Telemetry disabled.".yellow());
        }
        Commands::Status => {
            let prefs = Preferences::read();
            let env_disabled = is_telemetry_disabled_by_env();

            if env_disabled {
                println!(
                    "Telemetry is {} (disabled by environment variable)",
                    "disabled".yellow()
                );
            } else if prefs.telemetry_disabled {
                println!(
                    "Telemetry is {} (disabled via {})",
                    "disabled".yellow(),
                    "railway telemetry disable".bold()
                );
            } else {
                println!("Telemetry is {}", "enabled".green());
            }
        }
    }
    Ok(())
}
