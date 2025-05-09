use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fmt::Display;

use super::{
    queries::user_projects::{
        UserProjectsExternalWorkspaces, UserProjectsExternalWorkspacesProjects,
        UserProjectsMeWorkspaces, UserProjectsMeWorkspacesTeamProjectsEdgesNode,
    },
    *,
};

pub async fn workspaces() -> Result<Vec<Workspace>> {
    let configs = Configs::new()?;
    let vars = queries::user_projects::Variables {};
    let client = GQLClient::new_authorized(&configs)?;
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

    pub fn team_id(&self) -> Option<String> {
        match self {
            Self::External(w) => w.team_id.clone(),
            Self::Member(w) => w.team.as_ref().map(|t| t.id.clone()),
        }
    }

    pub fn projects(&self) -> Vec<Project> {
        let mut projects = match self {
            Self::External(w) => w.projects.iter().cloned().map(Project::External).collect(),
            Self::Member(w) => w.team.as_ref().map_or_else(Vec::new, |t| {
                t.projects
                    .edges
                    .iter()
                    .cloned()
                    .map(|e| Project::Team(e.node))
                    .collect()
            }),
        };
        projects.sort_by_key(|b| std::cmp::Reverse(b.updated_at()));
        projects
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
