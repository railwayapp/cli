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
    println!("Logged out successfully");
    Ok(())
}
