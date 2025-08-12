use crate::util::prompt::{fake_select, prompt_select};
use crate::workspace::{workspaces, Workspace};
use crate::errors::RailwayError;

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
        name: Some(project_name),
        description: None,
        team_id: workspace.team_id(),
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
        "{} {} on {}",
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
            .find(|w| w.id() == input || w.name() == input)
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

    // Need a custom inquire prompt here, because of the formatter
    let maybe_name = inquire::Text::new("Project Name")
        .with_formatter(&|s| {
            if s.is_empty() {
                "Will be randomly generated".to_string()
            } else {
                s.to_string()
            }
        })
        .with_placeholder("my-first-project")
        .with_help_message("Leave blank to generate a random name")
        .with_render_config(Configs::get_render_config())
        .prompt()?;

    // If name is empty, generate a random name
    let name = match maybe_name.as_str() {
        "" => {
            use names::Generator;
            let mut generator = Generator::default();
            generator.next().context("Failed to generate name")?
        }
        _ => maybe_name,
    };

    Ok(name)
}
