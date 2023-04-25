use crate::{controllers::project::get_project, util::prompt::prompt_confirm_with_default};
use anyhow::bail;
use is_terminal::IsTerminal;

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

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let linked_service = project
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
            project.name.bold()
        );
        let confirmed = if std::io::stdout().is_terminal() {
            prompt_confirm_with_default("Are you sure you want to unlink this service?", true)?
        } else {
            true
        };

        if !confirmed {
            return Ok(());
        }

        configs.unlink_service()?;
        configs.write()?;
        return Ok(());
    }

    if let Some(service) = linked_service {
        println!(
            "Linked to {} on {}",
            service.node.name.bold(),
            project.name.bold()
        );
    } else {
        println!("Linked to {}", project.name.bold());
    }

    let confirmed = if std::io::stdout().is_terminal() {
        prompt_confirm_with_default("Are you sure you want to unlink this project?", true)?
    } else {
        true
    };

    if !confirmed {
        return Ok(());
    }

    configs.unlink_project();
    configs.write()?;
    Ok(())
}
