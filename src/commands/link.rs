use anyhow::bail;
use colored::*;
use is_terminal::IsTerminal;
use serde::Serialize;
use std::fmt::Display;

use crate::{
    errors::RailwayError,
    util::prompt::{fake_select, prompt_options, prompt_options_skippable},
    workspace::{Project, Workspace, workspaces},
};

use super::*;

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

    /// The team to link to (deprecated: use --workspace instead).
    #[clap(long, short)]
    team: Option<String>,

    /// The workspace to link to.
    #[clap(long, short)]
    workspace: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkOutput {
    project_id: String,
    project_name: String,
    environment_id: String,
    environment_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_name: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    let mut configs = Configs::new()?;

    // Support both team (deprecated) and workspace arguments
    let workspace_arg = match (args.team.as_ref(), args.workspace.as_ref()) {
        (Some(_), None) => {
            eprintln!(
                "{}",
                "Warning: The --team flag is deprecated. Please use --workspace instead.".yellow()
            );
            args.team
        }
        (None, workspace) => workspace.cloned(),
        (Some(_), Some(_)) => {
            eprintln!("{}", "Warning: Both --team and --workspace provided. Using --workspace. The --team flag is deprecated.".yellow());
            args.workspace
        }
    };

    let workspaces = workspaces().await?;
    let workspace = select_workspace(args.project.clone(), workspace_arg, workspaces)?;

    let project = select_project(workspace, args.project.clone())?;

    let environment = select_environment(args.environment, &project)?;

    let service = select_service(&project, &environment, args.service)?;

    configs.link_project(
        project.id.clone(),
        Some(project.name.clone()),
        environment.id.clone(),
        Some(environment.name.clone()),
    )?;

    let (service_id, service_name) = if let Some(ref svc) = service {
        configs.link_service(svc.id.clone())?;
        (Some(svc.id.clone()), Some(svc.name.clone()))
    } else {
        (None, None)
    };

    configs.write()?;

    if args.json {
        let output = LinkOutput {
            project_id: project.id,
            project_name: project.name,
            environment_id: environment.id,
            environment_name: environment.name,
            service_id,
            service_name,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "\n{} {} {}",
            "Project".green(),
            project.name.magenta().bold(),
            "linked successfully! 🎉".green()
        );
    }

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
    if project.environments.is_empty() {
        if project.has_restricted_environments {
            bail!("All environments in this project are restricted");
        } else {
            bail!("Project has no environments");
        }
    }

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
        if !std::io::stdout().is_terminal() {
            bail!(
                "--environment required in non-interactive mode (multiple environments available)"
            );
        }
        prompt_options("Select an environment", project.environments.clone())?
    };
    Ok(environment)
}

fn select_project(
    workspace: Workspace,
    project: Option<String>,
) -> Result<NormalisedProject, anyhow::Error> {
    let projects = workspace
        .projects()
        .into_iter()
        .filter(|p| p.deleted_at().is_none())
        .collect::<Vec<_>>();

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
                return Err(RailwayError::ProjectNotFoundInWorkspace(
                    project,
                    workspace.name().to_owned(),
                )
                .into());
            }
        } else {
            prompt_workspace_projects(projects)?
        }
    });
    Ok(project)
}

fn select_workspace(
    project: Option<String>,
    workspace_name: Option<String>,
    workspaces: Vec<Workspace>,
) -> Result<Workspace, anyhow::Error> {
    let workspace = match (project, workspace_name) {
        (Some(project), None) => {
            // It's a project id or name, figure out workspace
            if let Some(workspace) = workspaces.iter().find(|w| {
                w.projects().iter().any(|pro| {
                    pro.id().to_lowercase() == project.to_lowercase()
                        || pro.name().to_lowercase() == project.to_lowercase()
                })
            }) {
                fake_select("Select a workspace", workspace.name());
                workspace.clone()
            } else {
                prompt_workspaces(workspaces)?
            }
        }
        (None, Some(workspace_arg)) | (Some(_), Some(workspace_arg)) => {
            if let Some(workspace) = workspaces.iter().find(|w| {
                w.id().to_lowercase() == workspace_arg.to_lowercase()
                    || w.team_id().map(str::to_lowercase) == Some(workspace_arg.to_lowercase())
                    || w.name().to_lowercase() == workspace_arg.to_lowercase()
            }) {
                fake_select("Select a workspace", workspace.name());
                workspace.clone()
            } else if workspace_arg.to_lowercase() == "personal" {
                bail!(RailwayError::NoPersonalWorkspace);
            } else {
                return Err(RailwayError::WorkspaceNotFound(workspace_arg.clone()).into());
            }
        }
        (None, None) => prompt_workspaces(workspaces)?,
    };
    Ok(workspace)
}

fn prompt_workspaces(workspaces: Vec<Workspace>) -> Result<Workspace> {
    if workspaces.is_empty() {
        return Err(RailwayError::NoProjects.into());
    }
    if workspaces.len() == 1 {
        fake_select("Select a workspace", workspaces[0].name());
        return Ok(workspaces[0].clone());
    }
    if !std::io::stdout().is_terminal() {
        bail!("--workspace required in non-interactive mode (multiple workspaces available)");
    }
    prompt_options("Select a workspace", workspaces)
}

fn prompt_workspace_projects(projects: Vec<Project>) -> Result<Project, anyhow::Error> {
    if !std::io::stdout().is_terminal() {
        bail!("--project required in non-interactive mode");
    }
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
        }>,
        /// Whether the project has restricted environments
        has_restricted_environments: bool,
    }
}

// unfortunately, due to the graphql client returning 3 different types for some reason (despite them all being identical)
// we need to write 3 match arms to convert it to our normalised project type
macro_rules! build_service_env_map {
    ($environments:expr) => {{
        let mut map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for env in $environments {
            for si in &env.node.service_instances.edges {
                map.entry(si.node.service_id.clone())
                    .or_default()
                    .push(env.node.id.clone());
            }
        }
        map
    }};
}

impl From<Project> for NormalisedProject {
    fn from(value: Project) -> Self {
        match value {
            Project::External(project) => {
                let total_envs = project.environments.edges.len();
                let mut service_env_map = build_service_env_map!(&project.environments.edges);
                let accessible_envs: Vec<_> = project
                    .environments
                    .edges
                    .into_iter()
                    .filter(|env| env.node.can_access)
                    .map(|env| NormalisedEnvironment::new(env.node.id, env.node.name))
                    .collect();
                let has_restricted = total_envs > accessible_envs.len();
                NormalisedProject::new(
                    project.id,
                    project.name,
                    accessible_envs,
                    project
                        .services
                        .edges
                        .into_iter()
                        .map(|service| {
                            let env_ids =
                                service_env_map.remove(&service.node.id).unwrap_or_default();
                            NormalisedService::new(service.node.id, service.node.name, env_ids)
                        })
                        .collect(),
                    has_restricted,
                )
            }
            Project::Workspace(project) => {
                let total_envs = project.environments.edges.len();
                let mut service_env_map = build_service_env_map!(&project.environments.edges);
                let accessible_envs: Vec<_> = project
                    .environments
                    .edges
                    .into_iter()
                    .filter(|env| env.node.can_access)
                    .map(|env| NormalisedEnvironment::new(env.node.id, env.node.name))
                    .collect();
                let has_restricted = total_envs > accessible_envs.len();
                NormalisedProject::new(
                    project.id,
                    project.name,
                    accessible_envs,
                    project
                        .services
                        .edges
                        .into_iter()
                        .map(|service| {
                            let env_ids =
                                service_env_map.remove(&service.node.id).unwrap_or_default();
                            NormalisedService::new(service.node.id, service.node.name, env_ids)
                        })
                        .collect(),
                    has_restricted,
                )
            }
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
