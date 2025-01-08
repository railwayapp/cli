use colored::*;
use std::fmt::Display;

use crate::{
    errors::RailwayError,
    util::prompt::{fake_select, prompt_options, prompt_options_skippable},
};

use super::{
    queries::user_projects::{
        UserProjectsMeProjectsEdgesNode, UserProjectsMeTeamsEdgesNode,
        UserProjectsMeTeamsEdgesNodeProjectsEdgesNode,
    },
    *,
};
use regex::Regex;

/// Associate existing project with current directory, may specify projectId as an argument
#[derive(Parser)]
pub struct Args {
    #[clap(long, short)]
    /// Environment to link to
    environment: Option<String>,

    /// Project to link to
    #[clap(long, short, alias = "project_id")]
    project: Option<String>,

    /// The service to link to
    #[clap(long, short)]
    service: Option<String>,

    /// The team to link to. Use "personal" for your personal account
    #[clap(long, short)]
    team: Option<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let me = post_graphql::<queries::UserProjects, _>(
        &client,
        configs.get_backboard(),
        queries::user_projects::Variables {},
    )
    .await?
    .me;

    let team = select_team(args.project.clone(), args.team, &me)?;

    let project = select_project(team, args.project.clone(), &me)?;

    let environment = select_environment(args.environment, &project)?;

    let service = select_service(&project, &environment, args.service)?;

    configs.link_project(
        project.id,
        Some(project.name.clone()),
        environment.id,
        Some(environment.name),
    )?;
    if let Some(service) = service {
        configs.link_service(service.id)?;
    }

    println!(
        "\n{} {} {}",
        "Project".green(),
        project.name.magenta().bold(),
        "linked successfully! 🎉".green()
    );

    configs.write()?;

    Ok(())
}

fn select_service(
    project: &NormalisedProject,
    environment: &NormalisedEnvironment,
    service: Option<String>,
) -> Result<Option<NormalisedService>, anyhow::Error> {
    let useful_services = project
        .services
        .iter()
        .filter(|&a| {
            a.service_instances
                .iter()
                .any(|instance| instance == &environment.id)
        })
        .cloned()
        .collect::<Vec<NormalisedService>>();

    let service = if !useful_services.is_empty() {
        if let Some(service) = service {
            let service_norm = useful_services.iter().find(|s| {
                (s.name.to_lowercase() == service.to_lowercase())
                    || (s.id.to_lowercase() == service.to_lowercase())
            });
            if let Some(service) = service_norm {
                fake_select("Select a service", &service.name);
                Some(service.clone())
            } else {
                return Err(RailwayError::ServiceNotFound(service).into());
            }
        } else {
            prompt_options_skippable("Select a service <esc to skip>", useful_services)?
        }
    } else {
        None
    };
    Ok(service)
}

fn select_environment(
    environment: Option<String>,
    project: &NormalisedProject,
) -> Result<NormalisedEnvironment, anyhow::Error> {
    let environment = if let Some(environment) = environment {
        let env = project.environments.iter().find(|e| {
            (e.name.to_lowercase() == environment.to_lowercase())
                || (e.id.to_lowercase() == environment.to_lowercase())
        });
        if let Some(env) = env {
            fake_select("Select an environment", &env.name);
            env.clone()
        } else {
            return Err(RailwayError::EnvironmentNotFound(environment).into());
        }
    } else if project.environments.len() == 1 {
        let env = project.environments[0].clone();
        fake_select("Select an environment", &env.name);
        env
    } else {
        prompt_options("Select an environment", project.environments.clone())?
    };
    Ok(environment)
}

fn select_project(
    team: Team<'_>,
    project: Option<String>,
    me: &queries::user_projects::UserProjectsMe,
) -> Result<NormalisedProject, anyhow::Error> {
    let project = NormalisedProject::from(match team {
        Team::Personal => {
            if let Some(project) = project {
                let proj = me.projects.edges.iter().find(|pro| {
                    (pro.node.id.to_lowercase() == project.to_lowercase())
                        || (pro.node.name.to_lowercase() == project.to_lowercase())
                });
                if let Some(project) = proj {
                    fake_select("Select a project", &project.node.name);
                    Project(ProjectType::Personal(project.node.clone()))
                } else {
                    return Err(RailwayError::ProjectNotFound.into());
                }
            } else {
                prompt_personal_projects(me)?
            }
        }
        Team::Team(team) => {
            if let Some(project) = project {
                let proj = team.projects.edges.iter().find(|pro| {
                    (pro.node.id.to_lowercase() == project.to_lowercase())
                        || (pro.node.name.to_lowercase() == project.to_lowercase())
                });
                if let Some(project) = proj {
                    fake_select("Select a project", &project.node.name);
                    Project(ProjectType::Team(project.node.clone()))
                } else {
                    return Err(
                        RailwayError::ProjectNotFoundInTeam(project, team.name.clone()).into(),
                    );
                }
            } else {
                prompt_team_projects(team.projects.clone())?
            }
        }
    });
    Ok(project)
}

fn select_team(
    project: Option<String>,
    team: Option<String>,
    me: &queries::user_projects::UserProjectsMe,
) -> Result<Team<'_>, anyhow::Error> {
    let uuid_regex =
        Regex::new(r#"(?i)^[0-9A-F]{8}-[0-9A-F]{4}-4[0-9A-F]{3}-[89AB][0-9A-F]{3}-[0-9A-F]{12}"#)
            .unwrap();
    let team = match (project.as_ref(), team.as_ref()) {
        (Some(project), None) if uuid_regex.is_match(project) => {
            // It's a project id, figure out team
            if me
                .projects
                .edges
                .iter()
                .any(|pro| pro.node.id.to_lowercase() == project.to_lowercase())
            {
                fake_select("Select a team", "Personal");
                Team::Personal
            } else if let Some(team) = me.teams.edges.iter().find(|team| {
                team.node
                    .projects
                    .edges
                    .iter()
                    .any(|proj| proj.node.id.to_lowercase() == project.to_lowercase())
            }) {
                fake_select("Select a team", &team.node.name);
                Team::Team(&team.node)
            } else {
                return Err(RailwayError::ProjectNotFound.into());
            }
        }
        (Some(project), None) => {
            // this means project name without team
            if me.teams.edges.is_empty() {
                // no teams, so it's personal
                // if there is a project that has the same name
                if me
                    .projects
                    .edges
                    .iter()
                    .any(|p| p.node.name.to_lowercase() == project.to_lowercase())
                {
                    fake_select("Select a team", "Personal");
                    Team::Personal
                } else {
                    return Err(RailwayError::ProjectNotFound.into());
                }
            } else {
                prompt_teams(me)?
            }
        }
        (None, Some(team_arg)) | (Some(_), Some(team_arg)) => {
            match team_arg.to_lowercase().as_str() {
                "personal" => {
                    fake_select("Select a team", "Personal");
                    Team::Personal
                }
                _ => {
                    if let Some(team) = me.teams.edges.iter().find(|team| {
                        (team.node.name.to_lowercase() == team_arg.to_lowercase())
                            || (team.node.id.to_lowercase() == team_arg.to_lowercase())
                    }) {
                        fake_select("Select a team", &team.node.name);
                        Team::Team(&team.node)
                    } else {
                        return Err(RailwayError::TeamNotFound(team_arg.clone()).into());
                    }
                }
            }
        }
        (None, None) if !me.teams.edges.is_empty() => prompt_teams(me)?,
        (None, None) => {
            fake_select("Select a team", "Personal");
            Team::Personal
        }
    };
    Ok(team)
}

fn prompt_teams(me: &queries::user_projects::UserProjectsMe) -> Result<Team<'_>> {
    let teams: Vec<&UserProjectsMeTeamsEdgesNode> =
        me.teams.edges.iter().map(|team| &team.node).collect();
    let mut team_names = vec![Team::Personal];
    team_names.extend(teams.into_iter().map(Team::Team));
    prompt_options("Select a team", team_names)
}

fn prompt_team_projects(
    projects: queries::user_projects::UserProjectsMeTeamsEdgesNodeProjects,
) -> Result<Project, anyhow::Error> {
    let mut team_projects: Vec<
        queries::user_projects::UserProjectsMeTeamsEdgesNodeProjectsEdgesNode,
    > = projects
        .edges
        .iter()
        .cloned()
        .map(|edge| edge.node)
        .collect();
    team_projects.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let prompt_projects = team_projects
        .iter()
        .cloned()
        .map(|project| Project(ProjectType::Team(project)))
        .collect::<Vec<Project>>();
    prompt_options("Select a project", prompt_projects)
}

fn prompt_personal_projects(
    me: &queries::user_projects::UserProjectsMe,
) -> Result<Project, anyhow::Error> {
    let mut personal_projects = me
        .projects
        .edges
        .iter()
        .map(|project| &project.node)
        .collect::<Vec<&UserProjectsMeProjectsEdgesNode>>();
    if personal_projects.is_empty() {
        return Err(RailwayError::NoProjects.into());
    }
    personal_projects.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let prompt_projects = personal_projects
        .iter()
        .cloned()
        .map(|project| Project(ProjectType::Personal(project.clone())))
        .collect::<Vec<Project>>();
    prompt_options("Select a project", prompt_projects)
}

structstruck::strike! {
    #[strikethrough[derive(Debug, Clone, derive_new::new)]]
    struct NormalisedProject {
        /// Project ID
        id: String,
        /// Project name
        name: String,
        /// Project environments
        environments: Vec<struct NormalisedEnvironment {
            /// Environment ID
            id: String,
            /// Environment Name
            name: String
        }>,
        /// Project services
        services: Vec<struct NormalisedService {
            /// Service ID
            id: String,
            /// Service name
            name: String,
            /// A `Vec` of environment IDs where the service is present
            ///
            /// _**note**_: this isn't what the API returns, we are just extracting what we need
            service_instances: Vec<String>,
        }>
    }
}

// unfortunately, due to the graphql client returning 3 different types for some reason (despite them all being identical)
// we need to write 3 match arms to convert it to our normaliesd project type
impl From<Project> for NormalisedProject {
    fn from(value: Project) -> Self {
        match value.0 {
            ProjectType::Personal(personal) => NormalisedProject::new(
                personal.id,
                personal.name,
                personal
                    .environments
                    .edges
                    .into_iter()
                    .map(|env| NormalisedEnvironment::new(env.node.id, env.node.name))
                    .collect(),
                personal
                    .services
                    .edges
                    .into_iter()
                    .map(|service| {
                        NormalisedService::new(
                            service.node.id,
                            service.node.name,
                            service
                                .node
                                .service_instances
                                .edges
                                .into_iter()
                                .map(|instance| instance.node.environment_id)
                                .collect(),
                        )
                    })
                    .collect(),
            ),
            ProjectType::Team(team) => NormalisedProject::new(
                team.id,
                team.name,
                team.environments
                    .edges
                    .into_iter()
                    .map(|env| NormalisedEnvironment::new(env.node.id, env.node.name))
                    .collect(),
                team.services
                    .edges
                    .into_iter()
                    .map(|service| {
                        NormalisedService::new(
                            service.node.id,
                            service.node.name,
                            service
                                .node
                                .service_instances
                                .edges
                                .into_iter()
                                .map(|instance| instance.node.environment_id)
                                .collect(),
                        )
                    })
                    .collect(),
            ),
        }
    }
}

#[derive(Debug, Clone)]
enum Team<'a> {
    Team(&'a UserProjectsMeTeamsEdgesNode),
    Personal,
}

impl Display for Team<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Team::Team(team) => write!(f, "{}", team.name),
            Team::Personal => write!(f, "{}", "Personal".bold()),
        }
    }
}

#[derive(Debug, Clone)]
enum ProjectType {
    Personal(UserProjectsMeProjectsEdgesNode),
    Team(UserProjectsMeTeamsEdgesNodeProjectsEdgesNode),
}

#[derive(Debug, Clone)]
struct Project(ProjectType);

impl Display for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            ProjectType::Personal(personal) => write!(f, "{}", personal.name),
            ProjectType::Team(team_project) => write!(f, "{}", team_project.name),
        }
    }
}

impl Display for NormalisedEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl Display for NormalisedService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}
