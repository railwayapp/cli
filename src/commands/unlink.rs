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

    /// Skip confirmation prompt
    #[clap(short = 'y', long = "yes")]
    yes: bool,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    let linked_service = project
        .services
        .edges
        .iter()
        .find(|service| Some(service.node.id.clone()) == linked_project.service);

    let is_terminal = std::io::stdout().is_terminal();

    if args.service {
        let Some(service) = linked_service else {
            bail!("No linked service");
        };
        if !args.json {
            println!(
                "Linked to {} on {}",
                service.node.name.bold(),
                project.name.bold()
            );
        }
        let confirmed = if args.yes {
            true
        } else if is_terminal {
            prompt_confirm_with_default("Are you sure you want to unlink this service?", true)?
        } else {
            bail!(
                "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
            );
        };

        if !confirmed {
            return Ok(());
        }

        configs.unlink_service()?;
        configs.write()?;
        if args.json {
            println!("{}", serde_json::json!({"success": true}));
        }
        return Ok(());
    }

    if !args.json {
        if let Some(service) = linked_service {
            println!(
                "Linked to {} on {}",
                service.node.name.bold(),
                project.name.bold()
            );
        } else {
            println!("Linked to {}", project.name.bold());
        }
    }

    let confirmed = if args.yes {
        true
    } else if is_terminal {
        prompt_confirm_with_default("Are you sure you want to unlink this project?", true)?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };

    if !confirmed {
        return Ok(());
    }

    configs.unlink_project();
    configs.write()?;
    if args.json {
        println!("{}", serde_json::json!({"success": true}));
    }
    Ok(())
}
