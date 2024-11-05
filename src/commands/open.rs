use crate::{
    consts::NON_INTERACTIVE_FAILURE, controllers::project::ensure_project_and_environment_exist,
    interact_or,
};

use super::*;

/// Open your project dashboard
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, _json: bool) -> Result<()> {
    interact_or!(NON_INTERACTIVE_FAILURE);

    let configs = Configs::new()?;
    let hostname = configs.get_host();
    let linked_project = configs.get_linked_project().await?;
    let client = GQLClient::new_authorized(&configs)?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    ::open::that(format!(
        "https://{hostname}/project/{}",
        linked_project.project
    ))?;
    Ok(())
}
