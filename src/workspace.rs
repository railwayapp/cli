use chrono::{DateTime, Utc};
use futures_util::{StreamExt, stream};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
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
    let client = GQLClient::new_authorized(&configs)?;
    workspaces_with_client(&client, &configs).await
}

pub async fn workspaces_with_client(
    client: &reqwest::Client,
    configs: &Configs,
) -> Result<Vec<Workspace>> {
    let vars = queries::user_projects::Variables {};
    let response =
        post_graphql::<queries::UserProjects, _>(client, configs.get_backboard(), vars).await?;

    // Member variants are yielded first so that a workspace the user both owns
    // and is an external member of keeps the richer Member representation.
    let mut seen: HashSet<String> = HashSet::new();
    let mut workspaces: Vec<Workspace> = response
        .me
        .workspaces
        .into_iter()
        .map(Workspace::Member)
        .chain(
            response
                .external_workspaces
                .into_iter()
                .map(Workspace::External),
        )
        .filter(|w| seen.insert(w.id().to_string()))
        .collect();
    workspaces.sort_by(|a, b| b.id().cmp(a.id()));
    Ok(workspaces)
}

/// `projectFavorites` takes a single workspace, so a listing that spans
/// workspaces is one request each. Bounded so an account with many workspaces
/// opens a handful of connections rather than one per workspace at once.
const FAVORITES_CONCURRENCY: usize = 6;

/// The authenticated user's favorited project ids, keyed by workspace id, in
/// the order the API returned them.
///
/// Favorites are decoration, so **every** failure degrades to "no favorites"
/// for that workspace rather than propagating. That is load-bearing: the field
/// requires a user principal, so it is refused outright for the
/// workspace-scoped and project tokens that `railway list` must keep serving,
/// and it can additionally be gated off by a feature flag. A caller that
/// cannot read favorites still gets exactly the listing it got before.
///
/// Errors are swallowed silently rather than warned about: on a project token
/// the refusal is the expected outcome on every single run, and a warning
/// there would be noise on a path that is working as intended.
pub async fn project_favorites_by_workspace(
    client: &reqwest::Client,
    configs: &Configs,
    workspace_ids: Vec<String>,
) -> HashMap<String, Vec<String>> {
    let backboard = configs.get_backboard();

    stream::iter(workspace_ids)
        .map(|workspace_id| {
            let backboard = backboard.clone();
            async move {
                let vars = queries::project_favorites::Variables {
                    workspace_id: workspace_id.clone(),
                };
                let favorites =
                    post_graphql::<queries::ProjectFavorites, _>(client, backboard, vars)
                        .await
                        .map(|data| data.project_favorites)
                        .unwrap_or_default();
                (workspace_id, favorites)
            }
        })
        .buffer_unordered(FAVORITES_CONCURRENCY)
        .collect()
        .await
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

    /// The workspace's projects ordered for display: favorites first, in the
    /// order `projectFavorites` returned them, then everything else by most
    /// recently updated. Each project is paired with whether it is favorited.
    ///
    /// `favorite_ids` may name projects this workspace does not contain (or
    /// that were deleted); those simply never match. Passing an empty slice
    /// reproduces the pre-favorites ordering exactly.
    pub fn projects_ranked(&self, favorite_ids: &[String]) -> Vec<(Project, bool)> {
        let rank: HashMap<&str, usize> = favorite_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();

        // `projects()` is already updated-at descending, and `sort_by_key` is
        // stable, so the non-favorites keep that order beneath the favorites.
        let mut projects = self.projects();
        projects.sort_by_key(|p| rank.get(p.id()).copied().unwrap_or(usize::MAX));

        projects
            .into_iter()
            .map(|project| {
                let is_favorite = rank.contains_key(project.id());
                (project, is_favorite)
            })
            .collect()
    }

    pub fn projects_with_workspace(&self, favorite_ids: &[String]) -> Vec<ProjectWithWorkspace> {
        let workspace_info = WorkspaceInfo {
            id: self.id().to_string(),
            name: self.name().to_string(),
        };
        self.projects_ranked(favorite_ids)
            .into_iter()
            .map(|(project, is_favorite)| ProjectWithWorkspace {
                workspace: workspace_info.clone(),
                is_favorite,
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

    /// The project's environments, flattened out of the two edge shapes the
    /// external and member variants use. `can_access` rides along because a
    /// listing can include environments this caller may not act in.
    pub fn environments(&self) -> Vec<ProjectEnvironment> {
        // The two variants carry structurally identical but nominally distinct
        // generated types, so each arm maps its own edges rather than sharing a
        // borrow.
        match self {
            Self::External(w) => w
                .environments
                .edges
                .iter()
                .map(|e| ProjectEnvironment {
                    id: e.node.id.clone(),
                    name: e.node.name.clone(),
                    can_access: e.node.can_access,
                })
                .collect(),
            Self::Workspace(w) => w
                .environments
                .edges
                .iter()
                .map(|e| ProjectEnvironment {
                    id: e.node.id.clone(),
                    name: e.node.name.clone(),
                    can_access: e.node.can_access,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectEnvironment {
    pub id: String,
    pub name: String,
    pub can_access: bool,
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
    /// Whether the authenticated user has starred this project. Always `false`
    /// for callers the favorites field refuses (workspace- and project-scoped
    /// tokens), which is indistinguishable from "nothing starred" by design —
    /// see [`project_favorites_by_workspace`].
    pub is_favorite: bool,
    #[serde(flatten)]
    pub project: Project,
}

/// Resolve a workspace from the list using an optional CLI-supplied
/// identifier (name or id). If exactly one workspace is available,
/// auto-selects it; if more than one and TTY, prompts; otherwise
/// bails with a helpful message.
///
/// Non-TTY callers with more than one workspace must pass --workspace
/// or this bails. When the workspace is auto-selected (flag or
/// single-workspace cases), echoes the choice via `fake_select` so
/// the user can see what landed.
pub fn pick_workspace(workspaces: Vec<Workspace>, requested: Option<String>) -> Result<Workspace> {
    use crate::errors::RailwayError;
    use crate::util::prompt::{fake_select, prompt_select};
    use is_terminal::IsTerminal;

    let confirm = |w: &Workspace| {
        fake_select("Select a workspace", w.name());
        w.clone()
    };

    if let Some(input) = requested {
        return workspaces
            .iter()
            .find(|w| w.id().eq_ignore_ascii_case(&input) || w.name().eq_ignore_ascii_case(&input))
            .map(confirm)
            .ok_or_else(|| RailwayError::WorkspaceNotFound(input).into());
    }
    if workspaces.len() == 1 {
        return Ok(confirm(&workspaces[0]));
    }
    if !std::io::stdout().is_terminal() {
        bail!("--workspace required in non-interactive mode (multiple workspaces available)");
    }
    let workspace = prompt_select("Select a workspace", workspaces)?;
    Ok(workspace)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a member workspace whose projects carry the given ids, names and
    /// updated-at stamps. Deserialized from JSON rather than constructed
    /// field-by-field so the test exercises the same generated types the query
    /// produces.
    fn workspace_with(projects: &[(&str, &str, &str)]) -> Workspace {
        let edges: Vec<serde_json::Value> = projects
            .iter()
            .map(|(id, name, updated_at)| {
                serde_json::json!({
                    "node": {
                        "id": id,
                        "name": name,
                        "createdAt": "2026-01-01T00:00:00Z",
                        "updatedAt": updated_at,
                        "deletedAt": null,
                        "environments": { "edges": [] },
                        "services": { "edges": [] },
                    }
                })
            })
            .collect();

        let raw = serde_json::json!({
            "id": "workspace-1",
            "name": "Workspace One",
            "team": null,
            "projects": { "edges": edges },
        });

        Workspace::Member(serde_json::from_value(raw).expect("workspace fixture should deserialize"))
    }

    /// The fixture used by most cases: three projects, oldest to newest, so the
    /// default (updated-at descending) order is `newest`, `middle`, `oldest`.
    fn three_projects() -> Workspace {
        workspace_with(&[
            ("oldest", "oldest-project", "2026-01-01T00:00:00Z"),
            ("middle", "middle-project", "2026-02-01T00:00:00Z"),
            ("newest", "newest-project", "2026-03-01T00:00:00Z"),
        ])
    }

    fn ranked_ids(workspace: &Workspace, favorites: &[String]) -> Vec<(String, bool)> {
        workspace
            .projects_ranked(favorites)
            .into_iter()
            .map(|(project, is_favorite)| (project.id().to_string(), is_favorite))
            .collect()
    }

    #[test]
    fn no_favorites_preserves_updated_at_ordering() {
        // The pre-favorites behaviour has to survive verbatim: this is what a
        // project-token caller, whom the API refuses, keeps seeing.
        assert_eq!(
            ranked_ids(&three_projects(), &[]),
            vec![
                ("newest".to_string(), false),
                ("middle".to_string(), false),
                ("oldest".to_string(), false),
            ]
        );
    }

    #[test]
    fn favorites_sort_above_everything_else() {
        assert_eq!(
            ranked_ids(&three_projects(), &["oldest".to_string()]),
            vec![
                ("oldest".to_string(), true),
                ("newest".to_string(), false),
                ("middle".to_string(), false),
            ]
        );
    }

    #[test]
    fn favorites_keep_api_order_not_updated_at_order() {
        // `projectFavorites` returns ids in the order the Favorites row should
        // render them, so a favorite updated long ago still outranks a newer
        // one that was starred later.
        assert_eq!(
            ranked_ids(
                &three_projects(),
                &["oldest".to_string(), "newest".to_string()],
            ),
            vec![
                ("oldest".to_string(), true),
                ("newest".to_string(), true),
                ("middle".to_string(), false),
            ]
        );
    }

    #[test]
    fn unknown_favorite_ids_are_ignored() {
        // Favorites are fetched per workspace, but a starred project may have
        // been deleted, or the id may simply not belong to this workspace.
        assert_eq!(
            ranked_ids(
                &three_projects(),
                &["not-in-this-workspace".to_string(), "middle".to_string()],
            ),
            vec![
                ("middle".to_string(), true),
                ("newest".to_string(), false),
                ("oldest".to_string(), false),
            ]
        );
    }

    #[test]
    fn projects_with_workspace_carries_the_favorite_flag() {
        // The `--json` shape is the contract for scripts, so assert the flag
        // rides along with the right project rather than just the ordering.
        let workspace = three_projects();
        let rows = workspace.projects_with_workspace(&["middle".to_string()]);

        let flags: Vec<(&str, bool)> = rows
            .iter()
            .map(|row| (row.project.id(), row.is_favorite))
            .collect();

        assert_eq!(
            flags,
            vec![("middle", true), ("newest", false), ("oldest", false)]
        );
        assert!(rows.iter().all(|row| row.workspace.id == "workspace-1"));
    }
}
