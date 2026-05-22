use super::*;

/// Open the Railway dashboard TUI
#[derive(Parser)]
pub struct Args {
    /// Optional project ID to open directly
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Optional environment name or ID to open directly
    #[clap(short, long)]
    environment: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    crate::interact_or!("`railway dash` requires a terminal");

    let configs = Configs::new()?;
    let _client = GQLClient::new_authorized(&configs)?;

    crate::controllers::dash_tui::run(crate::controllers::dash_tui::DashTuiParams {
        project: args.project,
        environment: args.environment,
    })
    .await
}
