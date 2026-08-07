use super::*;

/// Logout of your Railway account
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args) -> Result<()> {
    let mut configs = Configs::new()?;
    configs.reset()?;
    // Logout owns the credentials it is erasing: an ordinary `write` would
    // re-adopt whatever is still on disk and leave the user logged in.
    configs.write_credentials()?;
    // `railway code --claude` caches a year-long Claude setup-token. It is the
    // user's own third-party credential rather than a Railway one, but logging
    // out is the point at which they expect this machine to stop holding
    // anything — leaving it behind would be a surprise. Revoking it upstream is
    // still theirs to do; this only drops our copy.
    super::code::clear_claude_token_cache();
    println!("Logged out successfully");
    Ok(())
}
