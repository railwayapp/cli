use crate::controllers::project::ensure_project_and_environment_exist;
use is_terminal::IsTerminal;

use super::*;

/// Open your project dashboard
#[derive(Parser)]
pub struct Args {
    /// Print the URL instead of opening it
    #[clap(long, short)]
    print: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let hostname = configs.get_host();
    let linked_project = configs.get_linked_project().await?;
    let client = GQLClient::new_authorized(&configs)?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let url = format!(
        "https://{hostname}/project/{}?environmentId={}",
        linked_project.project, linked_project.environment
    );

    if args.print || !std::io::stdout().is_terminal() {
        println!("{url}");
    } else {
        ::open::that(&url)?;
    }
    Ok(())
}
