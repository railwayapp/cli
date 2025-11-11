use crate::errors::RailwayError;
use crate::util::prompt::{fake_select, prompt_select, prompt_text_with_placeholder_if_blank};
use crate::workspace::{Workspace, workspaces};

use super::*;

/// Create a new project
#[derive(Parser)]
#[clap(alias = "new")]
pub struct Args {
    /// Project name
    #[clap(short, long)]
    name: Option<String>,

    /// Workspace ID or name
    #[clap(short, long)]
    workspace: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    let mut configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;

    let workspaces = workspaces().await?;
    let workspace = prompt_workspace(workspaces, args.workspace)?;

    let project_name = prompt_project_name(args.name)?;

    let vars = mutations::project_create::Variables {
        // Railway's API will automatically generate a name if one is not provided
        name: if project_name.is_empty() {
            None
        } else {
            Some(project_name)
        },
        description: None,
        workspace_id: Some(workspace.id().to_owned()),
    };
    let project_create =
        post_graphql::<mutations::ProjectCreate, _>(&client, configs.get_backboard(), vars)
            .await?
            .project_create;

    let environment = project_create
        .environments
        .edges
        .first()
        .context("No environments")?
        .node
        .clone();

    configs.link_project(
        project_create.id.clone(),
        Some(project_create.name.clone()),
        environment.id,
        Some(environment.name),
    )?;
    configs.write()?;

    println!(
        "\n{} {} on {}",
        "Created project".green().bold(),
        project_create.name.bold(),
        workspace,
    );

    println!(
        "{}",
        format!(
            "https://{}/project/{}",
            configs.get_host(),
            project_create.id
        )
        .bold()
        .underline()
    );
    Ok(())
}

fn prompt_workspace(workspaces: Vec<Workspace>, workspace: Option<String>) -> Result<Workspace> {
    let select = |w: &Workspace| {
        fake_select("Select a workspace", w.name());
        w.clone()
    };

    if let Some(input) = workspace {
        return workspaces
            .iter()
            .find(|w| w.id().eq_ignore_ascii_case(&input) || w.name().eq_ignore_ascii_case(&input))
            .map(select)
            .ok_or_else(|| RailwayError::WorkspaceNotFound(input).into());
    }

    if workspaces.len() == 1 {
        return Ok(select(&workspaces[0]));
    }

    let workspace = prompt_select("Select a workspace", workspaces)?;
    Ok(workspace)
}

fn prompt_project_name(name: Option<String>) -> Result<String> {
    if let Some(name) = name {
        fake_select("Project Name", &name);

        return Ok(name);
    }

    let maybe_name = prompt_text_with_placeholder_if_blank(
        "Project Name",
        "<leave blank for randomly generated>",
        "<randomly generated>",
    )?;

    Ok(maybe_name.trim().to_string())
}
