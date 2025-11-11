use super::*;

use clap::CommandFactory;
use clap_complete::{Shell, generate};
use std::io;

/// Generate completion script
#[derive(Parser)]
pub struct Args {
    shell: Shell,
}

pub async fn command(args: Args) -> Result<()> {
    generate(
        args.shell,
        &mut self::Args::command(),
        "railway",
        &mut io::stdout(),
    );
    Ok(())
}
