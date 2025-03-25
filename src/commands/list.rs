use super::*;
use crate::workspace::workspaces;

/// List all projects in your Railway account
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let linked_project = configs.get_linked_project().await.ok();

    let workspaces = workspaces().await?;
    let mut all_projects = Vec::new();

    for workspace in workspaces {
        if !json {
            println!();
            println!("{}", workspace.name().bold());
        }

        let projects = workspace.projects();
        if !json {
            for project in &projects {
                let project_name =
                    if Some(project.id()) == linked_project.as_ref().map(|p| p.project.as_str()) {
                        project.name().purple().bold()
                    } else {
                        project.name().white()
                    };
                println!("  {project_name}");
            }
        }

        all_projects.extend(projects);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&all_projects)?);
    }
    Ok(())
}
