use serde::Serialize;

use super::{
    queries::{
        projects::ProjectsProjectsEdgesNode, user_projects::UserProjectsMeProjectsEdgesNode,
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
    let me = post_graphql::<queries::UserProjects, _>(&client, configs.get_backboard(), vars)
        .await?
        .me;

    let mut personal_projects: Vec<_> = me
        .projects
        .edges
        .iter()
        .map(|project| &project.node)
        .collect();
    // Sort by most recently updated (matches dashboard behavior)
    personal_projects.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let mut all_projects: Vec<_> = personal_projects
        .iter()
        .map(|project| Project::Me((*project).clone()))
        .collect();

    let teams: Vec<_> = me.teams.edges.iter().map(|team| &team.node).collect();
    if !json {
        println!("{}", "Personal".bold());
        for project in &personal_projects {
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

    for team in teams {
        if !json {
            println!();
            println!("{}", team.name.bold());
        }
        {
            let vars = queries::projects::Variables {
                team_id: Some(team.id.clone()),
            };

            let projects =
                post_graphql::<queries::Projects, _>(&client, configs.get_backboard(), vars)
                    .await?
                    .projects;

            let mut projects: Vec<_> = projects.edges.iter().map(|project| &project.node).collect();
            projects.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
            let mut team_projects: Vec<_> = projects
                .iter()
                .map(|project| Project::Team((*project).clone()))
                .collect();
            all_projects.append(&mut team_projects);
            if !json {
                for project in &projects {
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
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&all_projects)?);
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum Project {
    Me(UserProjectsMeProjectsEdgesNode),
    Team(ProjectsProjectsEdgesNode),
}
