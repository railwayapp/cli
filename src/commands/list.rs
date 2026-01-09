use super::*;
use crate::workspace::{ProjectWithWorkspace, workspaces};

/// List all projects in your Railway account
#[derive(Parser)]
pub struct Args {
    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let linked_project = configs.get_linked_project().await.ok();

    let workspaces = workspaces().await?;
    let mut all_projects: Vec<ProjectWithWorkspace> = Vec::new();

    for workspace in workspaces {
        if !args.json {
            println!();
            println!("{}", workspace.name().bold());

            for project in workspace.projects() {
                let project_name =
                    if Some(project.id()) == linked_project.as_ref().map(|p| p.project.as_str()) {
                        project.name().purple().bold()
                    } else {
                        project.name().white()
                    };
                println!("  {project_name}");
            }
        }

        all_projects.extend(workspace.projects_with_workspace());
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&all_projects)?);
    }
    Ok(())
}
