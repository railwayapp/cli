use anyhow::bail;
use is_terminal::IsTerminal;

use crate::consts::{ABORTED_BY_USER, NON_INTERACTIVE_FAILURE};

use super::*;

/// Open Railway Documentation in default browser
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!(NON_INTERACTIVE_FAILURE);
    }
    let config = Configs::new()?;
    let confirm = inquire::Confirm::new("Open the browser")
        .with_default(true)
        .with_render_config(config.get_render_config())
        .prompt()?;

    if !confirm {
        bail!(ABORTED_BY_USER);
    }

    ::open::that("https://docs.railway.app/")?;
    Ok(())
}
