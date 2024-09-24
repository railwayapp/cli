use colored::*;
use std::fmt::Display;

use crate::{
    controllers::project::get_project,
    errors::RailwayError,
    queries::project::ProjectProject,
    util::prompt::{fake_select, prompt_options},
};

use super::{
    queries::{
        projects::ProjectsProjectsEdgesNode,
        user_projects::{UserProjectsMeProjectsEdgesNode, UserProjectsMeTeamsEdgesNode},
    },
    *,
};

/// Associate existing project with current directory, may specify projectId as an argument
#[derive(Parser)]
pub struct Args {
    #[clap(long, short)]
    /// Environment to link to
    environment: Option<String>,

    /// Project ID to link to
    #[clap(long, short)]
    project_id: Option<String>,

    /// The service to link to
    #[clap(long, short)]
    service: Option<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;

    let project = NormalisedProject::from(if let Some(project_id) = args.project_id {
        let fetched_project = get_project(&client, &configs, project_id).await?;
        // fake_select is used to mimic the user providing input in the terminal
        // just for detail
        if let Some(team) = fetched_project.clone().team {
            fake_select("Select a team", &team.name);
        }
        fake_select("Select a project", fetched_project.name.as_str());
        Project(ProjectType::Fetched(fetched_project))
    } else {
        let me = post_graphql::<queries::UserProjects, _>(
            &client,
            configs.get_backboard(),
            queries::user_projects::Variables {},
        )
        .await?
        .me;
        let teams: Vec<_> = me.teams.edges.iter().map(|team| &team.node).collect();
        if teams.is_empty() {
            // prompt projects on personal account
            prompt_personal_projects(me)?
        } else {
            // prompt teams
            let mut team_names = vec![Team::Personal];
            team_names.extend(teams.into_iter().map(Team::Team));
            match prompt_options("Select a team", team_names)? {
                Team::Personal => prompt_personal_projects(me)?,
                Team::Team(team) => {
                    let vars = queries::projects::Variables {
                        team_id: Some(team.id.clone()),
                    };
                    let projects = post_graphql::<queries::Projects, _>(
                        &client,
                        configs.get_backboard(),
                        vars,
                    )
                    .await?
                    .projects;
                    prompt_team_projects(projects)?
                }
            }
        }
    });

    let environment = if let Some(environment) = args.environment {
        let env = project.environments.iter().find(|e| {
            (e.name.to_lowercase() == environment.to_lowercase())
                || (e.id.to_lowercase() == environment.to_lowercase())
        });
        if let Some(env) = env {
            fake_select("Select an environment", env.name.as_str());
            env.clone()
        } else {
            return Err(RailwayError::EnvironmentNotFound(environment).into());
        }
    } else if project.environments.len() == 1 {
        let env = project.environments[0].clone();
        fake_select("Select an environment", env.name.as_str());
        env
    } else {
        prompt_options("Select an environment", project.environments)?
    };
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
        Some(if let Some(service) = args.service {
            let service_norm = useful_services.iter().find(|s| {
                (s.name.to_lowercase() == service.to_lowercase())
                    || (s.id.to_lowercase() == service.to_lowercase())
            });
            if let Some(service) = service_norm {
                fake_select("Select a service", &service.name);
                service.clone()
            } else {
                return Err(RailwayError::ServiceNotFound(service).into());
            }
        } else {
            prompt_options("Select a service", useful_services)?
        })
    } else {
        None
    };

    configs.link_project(
        project.id,
        Some(project.name),
        environment.id,
        Some(environment.name),
    )?;
    if let Some(service) = service {
        configs.link_service(service.id)?;
    }

    configs.write()?;

    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.to_owned()).await?;

    println!(
        "\n{} {} {}",
        "Project".green(),
        project.name.magenta().bold(),
        "linked successfully! 🎉".green()
    );
    println!("  Next steps:");
    println!("    - {} Deploy your project", "railway up".blue());
    println!(
        "    - {} {} Run a command locally with variables from Railway",
        "railway run".blue(),
        "<command>".blue().bold()
    );
    println!("    - {} See what else you can do", "railway --help".blue());

    Ok(())
}

fn prompt_team_projects(
    projects: queries::projects::ProjectsProjects,
) -> Result<Project, anyhow::Error> {
    let mut team_projects: Vec<ProjectsProjectsEdgesNode> = projects
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
    me: queries::user_projects::UserProjectsMe,
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
            ProjectType::Fetched(fetched) => NormalisedProject::new(
                fetched.id,
                fetched.name,
                fetched
                    .environments
                    .edges
                    .into_iter()
                    .map(|env| NormalisedEnvironment::new(env.node.id, env.node.name))
                    .collect(),
                fetched
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

impl<'a> Display for Team<'a> {
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
    Team(ProjectsProjectsEdgesNode),
    Fetched(ProjectProject),
}

#[derive(Debug, Clone)]
struct Project(ProjectType);

impl Display for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            ProjectType::Personal(personal) => write!(f, "{}", personal.name),
            ProjectType::Team(team_project) => write!(f, "{}", team_project.name),
            ProjectType::Fetched(fetched) => write!(f, "{}", fetched.name),
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
