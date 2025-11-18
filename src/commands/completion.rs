use super::*;

use clap::CommandFactory;
use clap_complete::{generate, Shell};
use std::io;

/// Generate completion script
#[derive(Parser)]
pub struct Args {
    shell: Shell,
}

pub async fn command(args: Args) -> Result<()> {
    let mut railway = crate::build_args();

    generate(args.shell, &mut railway, "railway", &mut io::stdout());

    Ok(())
}
