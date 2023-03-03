use super::*;

/// Logout of your Railway account
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    configs.reset()?;
    configs.write()?;
    println!("Logged out successfully");
    Ok(())
}
