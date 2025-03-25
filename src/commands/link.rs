use chrono::{DateTime, Utc};
use colored::*;
use is_terminal::IsTerminal;
use serde::Serialize;
use std::fmt::Display;

use crate::{
    errors::RailwayError,
    util::prompt::{fake_select, prompt_options, prompt_options_skippable},
};

use super::{
    queries::user_projects::{
        UserProjectsExternalWorkspaces, UserProjectsExternalWorkspacesProjects,
        UserProjectsMeWorkspaces, UserProjectsMeWorkspacesTeamProjectsEdgesNode,
    },
    *,
};

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

    /// The team to link to.
    #[clap(long, short)]
    team: Option<String>,
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let mut configs = Configs::new()?;

    let client = GQLClient::new_authorized(&configs)?;
    let response = post_graphql::<queries::UserProjects, _>(
        &client,
        configs.get_backboard(),
        queries::user_projects::Variables {},
    )
    .await?;

    let workspace = select_workspace(args.project.clone(), args.team, response)?;

    let project = select_project(workspace, args.project.clone())?;

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
        } else if std::io::stdout().is_terminal() {
            prompt_options_skippable("Select a service <esc to skip>", useful_services)?
        } else {
            None
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
    workspace: Workspace,
    project: Option<String>,
) -> Result<NormalisedProject, anyhow::Error> {
    let projects = workspace.projects();

    let project = NormalisedProject::from({
        if let Some(project) = project {
            let proj = projects.into_iter().find(|pro| {
                (pro.id().to_lowercase() == project.to_lowercase())
                    || (pro.name().to_lowercase() == project.to_lowercase())
            });
            if let Some(project) = proj {
                fake_select("Select a project", &project.to_string());
                project
            } else {
                return Err(RailwayError::ProjectNotFoundInTeam(
                    project,
                    workspace.name().to_owned(),
                )
                .into());
            }
        } else {
            prompt_team_projects(projects)?
        }
    });
    Ok(project)
}

fn select_workspace(
    project: Option<String>,
    workspace_name: Option<String>,
    response: queries::user_projects::ResponseData,
) -> Result<Workspace, anyhow::Error> {
    let workspace = match (project, workspace_name) {
        (Some(project), None) => {
            // It's a project id or name, figure out workspace
            if let Some(workspace) = response.external_workspaces.iter().find(|w| {
                w.projects.iter().any(|pro| {
                    pro.id.to_lowercase() == project.to_lowercase()
                        || pro.name.to_lowercase() == project.to_lowercase()
                })
            }) {
                fake_select("Select a workspace", &workspace.name);
                Workspace::External(workspace.clone())
            } else if let Some(w) = response.me.workspaces.iter().find(|w| {
                w.team.as_ref().map_or(false, |t| {
                    t.projects.edges.iter().any(|pro| {
                        pro.node.id.to_lowercase() == project.to_lowercase()
                            || pro.node.name.to_lowercase() == project.to_lowercase()
                    })
                })
            }) {
                fake_select("Select a workspace", &w.name);
                Workspace::Member(w.clone())
            } else {
                prompt_workspaces(response)?
            }
        }
        (None, Some(team_arg)) | (Some(_), Some(team_arg)) => {
            if let Some(workspace) = response.me.workspaces.iter().find(|w| {
                (w.name.to_lowercase() == team_arg.to_lowercase())
                    || w.team
                        .as_ref()
                        .map_or(false, |t| t.id.to_lowercase() == team_arg.to_lowercase())
            }) {
                fake_select("Select a workspace", &workspace.name);
                Workspace::Member(workspace.clone())
            } else if let Some(workspace) = response.external_workspaces.iter().find(|w| {
                (w.name.to_lowercase() == team_arg.to_lowercase())
                    || w.team_id
                        .iter()
                        .any(|team_id| team_id.to_lowercase() == team_arg.to_lowercase())
            }) {
                fake_select("Select a workspace", &workspace.name);
                Workspace::External(workspace.clone())
            } else {
                return Err(RailwayError::TeamNotFound(team_arg.clone()).into());
            }
        }
        (None, None) => prompt_workspaces(response)?,
    };
    Ok(workspace)
}

fn prompt_workspaces(response: queries::user_projects::ResponseData) -> Result<Workspace> {
    let mut workspaces: Vec<Workspace> = response
        .me
        .workspaces
        .into_iter()
        .map(|w| Workspace::Member(w))
        .collect();
    workspaces.extend(
        response
            .external_workspaces
            .into_iter()
            .map(|w| Workspace::External(w)),
    );
    if workspaces.is_empty() {
        return Err(RailwayError::NoProjects.into());
    }
    if workspaces.len() == 1 {
        fake_select("Select a workspace", &workspaces[0].name());
        return Ok(workspaces[0].clone());
    }
    prompt_options("Select a workspace", workspaces)
}

fn prompt_team_projects(mut projects: Vec<Project>) -> Result<Project, anyhow::Error> {
    projects.sort_by(|a, b| b.updated_at().cmp(&a.updated_at()));
    prompt_options("Select a project", projects)
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
        match value {
            Project::External(project) => NormalisedProject::new(
                project.id,
                project.name,
                project
                    .environments
                    .edges
                    .into_iter()
                    .map(|env| NormalisedEnvironment::new(env.node.id, env.node.name))
                    .collect(),
                project
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
            Project::Team(project) => NormalisedProject::new(
                project.id,
                project.name,
                project
                    .environments
                    .edges
                    .into_iter()
                    .map(|env| NormalisedEnvironment::new(env.node.id, env.node.name))
                    .collect(),
                project
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
        }
    }
}

#[derive(Debug, Clone)]
enum Workspace {
    External(UserProjectsExternalWorkspaces),
    Member(UserProjectsMeWorkspaces),
}

impl Workspace {
    pub fn name(&self) -> &str {
        let name = match self {
            Self::External(w) => w.name.as_str(),
            Self::Member(w) => w.name.as_str(),
        };
        name
    }

    pub fn projects(&self) -> Vec<Project> {
        match self {
            Self::External(w) => w
                .projects
                .iter()
                .cloned()
                .map(|p| Project::External(p))
                .collect(),
            Self::Member(w) => w.team.as_ref().map_or_else(Vec::new, |t| {
                t.projects
                    .edges
                    .iter()
                    .cloned()
                    .map(|e| Project::Team(e.node))
                    .collect()
            }),
        }
    }
}

impl Display for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::External(w) => w.name.as_str(),
            Self::Member(w) => w.name.as_str(),
        };
        write!(f, "{name}")
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum Project {
    External(UserProjectsExternalWorkspacesProjects),
    Team(UserProjectsMeWorkspacesTeamProjectsEdgesNode),
}

impl Project {
    pub fn id(&self) -> &str {
        match self {
            Self::External(w) => &w.id,
            Self::Team(w) => &w.id,
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Self::External(w) => &w.name,
            Self::Team(w) => &w.name,
        }
    }
    pub fn updated_at(&self) -> DateTime<Utc> {
        match self {
            Self::External(w) => w.updated_at,
            Self::Team(w) => w.updated_at,
        }
    }
}

impl Display for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Team(team_project) => write!(f, "{}", team_project.name),
            Self::External(team_project) => write!(f, "{}", team_project.name),
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
