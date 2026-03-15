use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fmt::Display;

use super::{
    queries::user_projects::{
        UserProjectsExternalWorkspaces, UserProjectsExternalWorkspacesProjects,
        UserProjectsMeWorkspaces, UserProjectsMeWorkspacesProjectsEdgesNode,
    },
    *,
};

pub async fn workspaces() -> Result<Vec<Workspace>> {
    let configs = Configs::new()?;
    let vars = queries::user_projects::Variables {};
    let client = GQLClient::new_authorized_with_scope(&configs, AuthScope::GlobalOnly)?;
    let response =
        post_graphql::<queries::UserProjects, _>(&client, configs.get_backboard(), vars).await?;

    let mut workspaces: Vec<Workspace> = response
        .me
        .workspaces
        .into_iter()
        .map(Workspace::Member)
        .collect();
    workspaces.extend(
        response
            .external_workspaces
            .into_iter()
            .map(Workspace::External),
    );
    workspaces.sort_by(|a, b| b.id().cmp(a.id()));
    Ok(workspaces)
}

#[derive(Debug, Clone)]
pub enum Workspace {
    External(UserProjectsExternalWorkspaces),
    Member(UserProjectsMeWorkspaces),
}

impl Workspace {
    pub fn id(&self) -> &str {
        match self {
            Self::External(w) => w.id.as_str(),
            Self::Member(w) => w.id.as_str(),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::External(w) => w.name.as_str(),
            Self::Member(w) => w.name.as_str(),
        }
    }

    #[allow(deprecated)] // team field deprecated but needed for backwards compat with scripts using team IDs
    pub fn team_id(&self) -> Option<&str> {
        match self {
            Self::External(w) => w.team_id.as_deref(),
            Self::Member(w) => w.team.as_ref().map(|t| t.id.as_str()),
        }
    }

    pub fn projects(&self) -> Vec<Project> {
        let mut projects: Vec<_> = match self {
            Self::External(w) => w.projects.iter().cloned().map(Project::External).collect(),
            Self::Member(w) => w
                .projects
                .edges
                .iter()
                .cloned()
                .map(|e| Project::Workspace(e.node))
                .collect(),
        };
        projects.sort_by_key(|b| std::cmp::Reverse(b.updated_at()));
        projects
    }

    pub fn projects_with_workspace(&self) -> Vec<ProjectWithWorkspace> {
        let workspace_info = WorkspaceInfo {
            id: self.id().to_string(),
            name: self.name().to_string(),
        };
        self.projects()
            .into_iter()
            .map(|project| ProjectWithWorkspace {
                workspace: workspace_info.clone(),
                project,
            })
            .collect()
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
pub enum Project {
    External(UserProjectsExternalWorkspacesProjects),
    Workspace(UserProjectsMeWorkspacesProjectsEdgesNode),
}

impl Project {
    pub fn id(&self) -> &str {
        match self {
            Self::External(w) => &w.id,
            Self::Workspace(w) => &w.id,
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Self::External(w) => &w.name,
            Self::Workspace(w) => &w.name,
        }
    }
    pub fn updated_at(&self) -> DateTime<Utc> {
        match self {
            Self::External(w) => w.updated_at,
            Self::Workspace(w) => w.updated_at,
        }
    }
    pub fn deleted_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::External(w) => w.deleted_at,
            Self::Workspace(w) => w.deleted_at,
        }
    }
}

impl Display for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Workspace(project) => write!(f, "{}", project.name),
            Self::External(project) => write!(f, "{}", project.name),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectWithWorkspace {
    pub workspace: WorkspaceInfo,
    #[serde(flatten)]
    pub project: Project,
}
