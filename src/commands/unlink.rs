use crate::util::prompt::prompt_confirm;
use anyhow::bail;
use is_terminal::IsTerminal;

use crate::consts::ABORTED_BY_USER;

use super::*;

/// Disassociate project from current directory
#[derive(Parser)]
pub struct Args {
    /// Unlink a service
    #[clap(short, long)]
    service: bool,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let vars = queries::project::Variables {
        id: linked_project.project.to_owned(),
    };

    let res = post_graphql::<queries::Project, _>(&client, configs.get_backboard(), vars).await?;

    let body = res.data.context("Failed to retrieve response body")?;
    let linked_service = body
        .project
        .services
        .edges
        .iter()
        .find(|service| Some(service.node.id.clone()) == linked_project.service);

    if args.service {
        let Some(service) = linked_service else {
            bail!("No linked service");
        };
        println!(
            "Linked to {} on {}",
            service.node.name.bold(),
            body.project.name.bold()
        );
        let confirmed = !if std::io::stdout().is_terminal() {
            prompt_confirm("Are you sure you want to unlink this service?")?
        } else {
            true
        };

        if !confirmed {
            bail!(ABORTED_BY_USER);
        }
        configs.unlink_service()?;
        configs.write()?;
        return Ok(());
    }

    if let Some(service) = linked_service {
        println!(
            "Linked to {} on {}",
            service.node.name.bold(),
            body.project.name.bold()
        );
    } else {
        println!("Linked to {}", body.project.name.bold());
    }

    let confirmed = !if std::io::stdout().is_terminal() {
        prompt_confirm("Are you sure you want to unlink this project?")?
    } else {
        true
    };

    if !confirmed {
        bail!(ABORTED_BY_USER);
    }
    configs.unlink_project()?;
    configs.write()?;
    Ok(())
}
