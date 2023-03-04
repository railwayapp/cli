use super::*;

/// Get the current logged in user
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let vars = queries::user_meta::Variables {};

    let res = post_graphql::<queries::UserMeta, _>(&client, configs.get_backboard(), vars).await?;
    let me = res.data.context("No data")?.me;

    println!(
        "Logged in as {} ({})",
        me.name.context("No name")?.bold(),
        me.email
    );

    Ok(())
}
