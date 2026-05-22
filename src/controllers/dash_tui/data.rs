use anyhow::Result;

use crate::workspace::{Project, workspaces};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectCard {
    pub id: String,
    pub name: String,
    pub workspace_name: Option<String>,
    pub service_count: usize,
    pub environment_count: usize,
}

impl ProjectCard {
    pub fn matches_filter(&self, filter: &str) -> bool {
        let filter = filter.trim().to_lowercase();
        if filter.is_empty() {
            return true;
        }

        self.name.to_lowercase().contains(&filter)
            || self.id.to_lowercase().contains(&filter)
            || self
                .workspace_name
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains(&filter)
    }
}

pub async fn load_project_cards() -> Result<Vec<ProjectCard>> {
    let mut cards = Vec::new();

    for workspace in workspaces().await? {
        let workspace_name = workspace.name().to_string();

        for project in workspace.projects() {
            if project.deleted_at().is_some() {
                continue;
            }

            cards.push(project_card_from_project(project, workspace_name.clone()));
        }
    }

    Ok(cards)
}

fn project_card_from_project(project: Project, workspace_name: String) -> ProjectCard {
    match project {
        Project::External(project) => ProjectCard {
            id: project.id,
            name: project.name,
            workspace_name: Some(workspace_name),
            service_count: project.services.edges.len(),
            environment_count: project.environments.edges.len(),
        },
        Project::Workspace(project) => ProjectCard {
            id: project.id,
            name: project.name,
            workspace_name: Some(workspace_name),
            service_count: project.services.edges.len(),
            environment_count: project.environments.edges.len(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_card_filter_matches_name_workspace_and_id() {
        let card = ProjectCard {
            id: "proj_123".to_string(),
            name: "api".to_string(),
            workspace_name: Some("platform".to_string()),
            service_count: 3,
            environment_count: 2,
        };

        assert!(card.matches_filter("api"));
        assert!(card.matches_filter("platform"));
        assert!(card.matches_filter("proj_123"));
        assert!(!card.matches_filter("worker"));
    }
}
