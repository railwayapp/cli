use std::fmt::Display;

use anyhow::bail;
use is_terminal::IsTerminal;

use crate::{
    errors::RailwayError,
    util::{
        progress::create_spinner_if,
        prompt::{fake_select, prompt_confirm_with_default, prompt_options},
        two_factor::validate_two_factor_if_enabled,
    },
    workspace::{Project, Workspace, workspaces},
};

use super::*;

/// Delete a project
#[derive(Parser)]
pub struct Args {
    /// The project ID or name to delete
    #[clap(short, long)]
    project: Option<String>,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    yes: bool,

    /// Output in JSON format
    #[clap(long)]
    json: bool,

    /// 2FA code for verification (required if 2FA is enabled in non-interactive mode)
    #[clap(long = "2fa-code")]
    two_factor_code: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized_with_scope(&configs, AuthScope::GlobalOnly)?;
    let is_terminal = std::io::stdout().is_terminal();

    let all_workspaces = workspaces().await?;
    let (project_id, project_name) = select_project(args.project, &all_workspaces, is_terminal)?;

    if !args.yes {
        if !is_terminal {
            bail!(
                "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
            );
        }

        let confirmed = prompt_confirm_with_default(
            format!(
                r#"Are you sure you want to delete the project "{}"? This action cannot be undone."#,
                project_name.red()
            )
            .as_str(),
            false,
        )?;

        if !confirmed {
            println!("Deletion cancelled.");
            return Ok(());
        }
    }

    validate_two_factor_if_enabled(&client, &configs, is_terminal, args.two_factor_code).await?;

    let spinner = create_spinner_if(!args.json, "Deleting project...".into());

    let vars = mutations::project_delete::Variables {
        id: project_id.clone(),
    };

    post_graphql::<mutations::ProjectDelete, _>(&client, &configs.get_backboard(), vars).await?;

    if args.json {
        println!("{}", serde_json::json!({"id": project_id}));
    } else if let Some(spinner) = spinner {
        spinner.finish_with_message(format!(
            "{} {} {}",
            "Project".green(),
            project_name.magenta().bold(),
            "deleted!".green()
        ));
    }

    Ok(())
}

fn select_project(
    project_arg: Option<String>,
    all_workspaces: &[Workspace],
    is_terminal: bool,
) -> Result<(String, String)> {
    let all_projects: Vec<ProjectWithWorkspace> = all_workspaces
        .iter()
        .flat_map(|w| {
            w.projects()
                .into_iter()
                .filter(|p| p.deleted_at().is_none())
                .map(|p| ProjectWithWorkspace {
                    project: p,
                    workspace_name: w.name().to_string(),
                })
        })
        .collect();

    if all_projects.is_empty() {
        bail!(RailwayError::NoProjects);
    }

    if let Some(project) = project_arg {
        let found = all_projects.iter().find(|p| {
            p.project.id().to_lowercase() == project.to_lowercase()
                || p.project.name().to_lowercase() == project.to_lowercase()
        });

        if let Some(p) = found {
            fake_select("Select the project to delete", &p.to_string());
            return Ok((p.project.id().to_string(), p.project.name().to_string()));
        } else {
            bail!("Project \"{}\" not found", project);
        }
    }

    if !is_terminal {
        bail!(
            "Project must be specified when not running in a terminal. Use --project <id or name>"
        );
    }

    let selected = prompt_options("Select the project to delete", all_projects)?;
    Ok((
        selected.project.id().to_string(),
        selected.project.name().to_string(),
    ))
}

#[derive(Debug, Clone)]
struct ProjectWithWorkspace {
    project: Project,
    workspace_name: String,
}

impl Display for ProjectWithWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.project.name(), self.workspace_name)
    }
}
