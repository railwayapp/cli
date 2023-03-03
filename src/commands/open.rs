use anyhow::bail;
use is_terminal::IsTerminal;

use crate::consts::NON_INTERACTIVE_FAILURE;

use super::*;

/// Open your project dashboard
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!(NON_INTERACTIVE_FAILURE);
    }

    let configs = Configs::new()?;
    let hostname = configs.get_host();
    let linked_project = configs.get_linked_project().await?;
    ::open::that(format!(
        "https://{hostname}/project/{}",
        linked_project.project
    ))?;
    Ok(())
}
