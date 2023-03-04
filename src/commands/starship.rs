use super::*;

/// Starship Metadata
#[derive(Parser)]
#[clap(hide = true)]
pub struct Args {}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let linked_project = configs.get_linked_project().await?;
    println!("{}", serde_json::to_string(&linked_project)?);
    Ok(())
}
