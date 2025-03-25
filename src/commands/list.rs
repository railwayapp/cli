use serde::Serialize;

use super::{
    queries::user_projects::{
        UserProjectsExternalWorkspacesProjects, UserProjectsMeWorkspacesTeamProjectsEdgesNode,
    },
    *,
};

/// List all projects in your Railway account
#[derive(Parser)]
pub struct Args {}

pub async fn command(_args: Args, json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await.ok();

    let vars = queries::user_projects::Variables {};
    let mut response =
        post_graphql::<queries::UserProjects, _>(&client, configs.get_backboard(), vars).await?;

    let mut all_projects = Vec::new();

    response.me.workspaces.sort_by(|a, b| b.id.cmp(&a.id));
    for workspace in response.me.workspaces {
        if !json {
            println!();
            println!("{}", workspace.name.bold());
        }

        if let Some(mut team) = workspace.team {
            team.projects
                .edges
                .sort_by(|a, b| b.node.updated_at.cmp(&a.node.updated_at));
            if !json {
                for project in &team.projects.edges {
                    let project_name = if linked_project.is_some()
                        && project.node.id == linked_project.as_ref().unwrap().project
                    {
                        project.node.name.purple().bold()
                    } else {
                        project.node.name.white()
                    };
                    println!("  {project_name}");
                }
            }

            all_projects.extend(
                team.projects
                    .edges
                    .into_iter()
                    .map(|edge| Project::Team(edge.node)),
            );
        }
    }

    response.external_workspaces.sort_by(|a, b| b.id.cmp(&a.id));
    for mut workspace in response.external_workspaces {
        workspace
            .projects
            .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        if !json {
            println!();
            println!("{}", workspace.name.bold());

            for project in &workspace.projects {
                let project_name = if linked_project.is_some()
                    && project.id == linked_project.as_ref().unwrap().project
                {
                    project.name.purple().bold()
                } else {
                    project.name.white()
                };
                println!("  {project_name}");
            }
        }
        all_projects.extend(workspace.projects.into_iter().map(|p| Project::External(p)));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&all_projects)?);
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum Project {
    External(UserProjectsExternalWorkspacesProjects),
    Team(UserProjectsMeWorkspacesTeamProjectsEdgesNode),
}
