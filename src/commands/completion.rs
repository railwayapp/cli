use super::*;

use clap::CommandFactory;
use clap_complete::{generate, Shell};
use std::io;

/// Generate completion script
#[derive(Parser)]
pub struct Args {
    shell: Shell,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    generate(
        args.shell,
        &mut crate::Args::command(),
        "railway",
        &mut io::stdout(),
    );
    Ok(())
}
