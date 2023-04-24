use std::fmt::Display;

use anyhow::bail;
use is_terminal::IsTerminal;

use crate::{
    commands::queries::user_projects::UserProjectsMeTeamsEdgesNode, consts::PROJECT_NOT_FOUND,
    controllers::project::get_project, util::prompt::prompt_options,
};

use super::{
    queries::{
        project::ProjectProjectEnvironmentsEdgesNode,
        projects::{ProjectsProjectsEdgesNode, ProjectsProjectsEdgesNodeEnvironmentsEdgesNode},
        user_projects::{
            UserProjectsMeProjectsEdgesNode, UserProjectsMeProjectsEdgesNodeEnvironmentsEdgesNode,
        },
    },
    *,
};

/// Associate existing project with current directory, may specify projectId as an argument
#[derive(Parser)]
pub struct Args {
    #[clap(long)]
    /// Environment to link to
    environment: Option<String>,

    /// Project ID to link to
    project_id: Option<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    if let Some(project_id) = args.project_id {
        let project = get_project(&client, &configs, project_id.clone()).await?;

        let environment = if let Some(environment_name_or_id) = args.environment {
            let environment = project
                .environments
                .edges
                .iter()
                .find(|env| {
                    env.node.name == environment_name_or_id || env.node.id == environment_name_or_id
                })
                .context("Environment not found")?;
            ProjectEnvironment(&environment.node)
        } else if !std::io::stdout().is_terminal() {
            bail!("Environment must be provided when not running in a terminal");
        } else if project.environments.edges.len() == 1 {
            ProjectEnvironment(&project.environments.edges[0].node)
        } else {
            prompt_options(
                "Select an environment",
                project
                    .environments
                    .edges
                    .iter()
                    .map(|env| ProjectEnvironment(&env.node))
                    .collect(),
            )?
        };

        configs.link_project(
            project.id.clone(),
            Some(project.name.clone()),
            environment.0.id.clone(),
            Some(environment.0.name.clone()),
        )?;
        configs.write()?;
        return Ok(());
    } else if !std::io::stdout().is_terminal() {
        bail!("Project must be provided when not running in a terminal");
    }

    let vars = queries::user_projects::Variables {};
    let res =
        post_graphql::<queries::UserProjects, _>(&client, configs.get_backboard(), vars).await?;
    let body = res.data.context("Failed to retrieve response body")?;

    let mut personal_projects: Vec<_> = body
        .me
        .projects
        .edges
        .iter()
        .map(|project| &project.node)
        .collect();
    personal_projects.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let personal_project_names = personal_projects
        .iter()
        .map(|project| PersonalProject(project))
        .collect::<Vec<_>>();

    let teams: Vec<_> = body.me.teams.edges.iter().map(|team| &team.node).collect();

    if teams.is_empty() {
        let (project, environment) = prompt_personal_projects(personal_project_names)?;
        configs.link_project(
            project.0.id.clone(),
            Some(project.0.name.clone()),
            environment.0.id.clone(),
            Some(environment.0.name.clone()),
        )?;
        configs.write()?;
        return Ok(());
    }

    let mut team_names = teams
        .iter()
        .map(|team| Team::Team(team))
        .collect::<Vec<_>>();
    team_names.insert(0, Team::Personal);

    let team = prompt_options("Select a team", team_names)?;
    match team {
        Team::Personal => {
            let (project, environment) = prompt_personal_projects(personal_project_names)?;
            configs.link_project(
                project.0.id.clone(),
                Some(project.0.name.clone()),
                environment.0.id.clone(),
                Some(environment.0.name.clone()),
            )?;
        }
        Team::Team(team) => {
            let vars = queries::projects::Variables {
                team_id: Some(team.id.clone()),
            };

            let res = post_graphql::<queries::Projects, _>(&client, configs.get_backboard(), vars)
                .await?;

            let body = res.data.context("Failed to retrieve response body")?;
            let mut projects: Vec<_> = body
                .projects
                .edges
                .iter()
                .map(|project| &project.node)
                .collect();
            projects.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

            let project_names = projects
                .iter()
                .map(|project| Project(project))
                .collect::<Vec<_>>();
            let (project, environment) = prompt_team_projects(project_names)?;
            configs.link_project(
                project.0.id.clone(),
                Some(project.0.name.clone()),
                environment.0.id.clone(),
                Some(environment.0.name.clone()),
            )?;
        }
    }

    configs.write()?;

    Ok(())
}

fn prompt_team_projects(project_names: Vec<Project>) -> Result<(Project, Environment)> {
    let project = prompt_options("Select a project", project_names)?;
    let environments = project
        .0
        .environments
        .edges
        .iter()
        .map(|env| Environment(&env.node))
        .collect();
    let environment = prompt_options("Select an environment", environments)?;
    Ok((project, environment))
}

fn prompt_personal_projects(
    personal_project_names: Vec<PersonalProject>,
) -> Result<(PersonalProject, PersonalEnvironment)> {
    let project = prompt_options("Select a project", personal_project_names)?;
    let environments = project
        .0
        .environments
        .edges
        .iter()
        .map(|env| PersonalEnvironment(&env.node))
        .collect();
    let environment = prompt_options("Select an environment", environments)?;
    Ok((project, environment))
}

#[derive(Debug, Clone)]
struct PersonalProject<'a>(&'a UserProjectsMeProjectsEdgesNode);

impl<'a> Display for PersonalProject<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}

#[derive(Debug, Clone)]
struct PersonalEnvironment<'a>(&'a UserProjectsMeProjectsEdgesNodeEnvironmentsEdgesNode);

impl<'a> Display for PersonalEnvironment<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}

#[derive(Debug, Clone)]
struct Project<'a>(&'a ProjectsProjectsEdgesNode);

impl<'a> Display for Project<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}

#[derive(Debug, Clone)]
struct Environment<'a>(&'a ProjectsProjectsEdgesNodeEnvironmentsEdgesNode);

impl<'a> Display for Environment<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}

#[derive(Debug, Clone)]
enum Team<'a> {
    Team(&'a UserProjectsMeTeamsEdgesNode),
    Personal,
}

impl<'a> Display for Team<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Team::Team(team) => write!(f, "{}", team.name),
            Team::Personal => write!(f, "{}", "Personal".bold()),
        }
    }
}

#[derive(Debug, Clone)]
struct ProjectEnvironment<'a>(&'a ProjectProjectEnvironmentsEdgesNode);

impl<'a> Display for ProjectEnvironment<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.name)
    }
}
