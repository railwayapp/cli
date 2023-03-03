use super::*;

/// Logout of your Railway account
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    unimplemented!("logout command is not implemented yet");
}
