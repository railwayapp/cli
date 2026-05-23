use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use reqwest::Client;

use crate::{
    client::GQLClient,
    commands::{
        Configs,
        queries::{RailwayProject, project::ProjectProjectEnvironmentsEdgesNode},
    },
    controllers::{
        environment::get_matched_environment,
        project::{
            ProjectEnvironmentInstances, find_service_instance, get_environment_instances,
            get_project,
        },
    },
    errors::RailwayError,
    workspace::{Project, workspaces},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectCard {
    pub id: String,
    pub name: String,
    pub workspace_name: Option<String>,
    pub service_count: usize,
    pub environment_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectLoadTarget {
    pub project_id: String,
    pub environment_hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardProject {
    pub id: String,
    pub name: String,
    pub workspace_name: Option<String>,
    pub selected_environment_id: String,
    pub selected_environment_name: String,
    pub environments: Vec<EnvironmentSummary>,
    pub services: Vec<DashboardService>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentSummary {
    pub id: String,
    pub name: String,
    pub deleted: bool,
    pub accessible: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardService {
    pub id: String,
    pub name: String,
    pub active_in_environment: bool,
    pub num_replicas: Option<i64>,
    pub latest_deployment: Option<DeploymentSummary>,
    pub domains: Vec<String>,
    pub source_repo: Option<String>,
    pub source_image: Option<String>,
    pub cron_schedule: Option<String>,
    pub next_cron_run_at: Option<DateTime<Utc>>,
    pub start_command: Option<String>,
    pub volume_mounts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeploymentSummary {
    pub id: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub can_redeploy: bool,
    pub stopped: bool,
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

impl DashboardProject {
    pub fn accessible_environments(&self) -> Vec<&EnvironmentSummary> {
        self.environments
            .iter()
            .filter(|environment| environment.accessible && !environment.deleted)
            .collect()
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

pub async fn load_dashboard_project(target: ProjectLoadTarget) -> Result<DashboardProject> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    load_dashboard_project_with_client(&configs, &client, target).await
}

pub async fn load_dashboard_project_with_client(
    configs: &Configs,
    client: &Client,
    target: ProjectLoadTarget,
) -> Result<DashboardProject> {
    let project = get_project(client, configs, target.project_id.clone()).await?;

    if project.deleted_at.is_some() {
        bail!(RailwayError::ProjectDeleted);
    }

    let environment = resolve_project_environment(configs, &project, &target).await?;
    if environment.deleted_at.is_some() {
        bail!(RailwayError::EnvironmentDeleted);
    }

    let instances =
        get_environment_instances(client, configs, &project.id, &environment.id).await?;

    Ok(map_dashboard_project(project, environment, instances))
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

async fn resolve_project_environment(
    configs: &Configs,
    project: &RailwayProject,
    target: &ProjectLoadTarget,
) -> Result<ProjectProjectEnvironmentsEdgesNode> {
    if let Some(environment_hint) = target.environment_hint.clone() {
        return get_matched_environment(project, environment_hint);
    }

    if let Ok(linked_project) = configs.get_linked_project().await
        && linked_project.project == project.id
        && let Some(environment_hint) = linked_project
            .environment_name
            .clone()
            .or(linked_project.environment.clone())
    {
        return get_matched_environment(project, environment_hint);
    }

    project
        .environments
        .edges
        .iter()
        .find(|environment| environment.node.can_access && environment.node.deleted_at.is_none())
        .map(|environment| environment.node.clone())
        .ok_or_else(|| anyhow!("Project `{}` has no accessible environments.", project.name))
}

fn map_dashboard_project(
    project: RailwayProject,
    selected_environment: ProjectProjectEnvironmentsEdgesNode,
    instances: ProjectEnvironmentInstances,
) -> DashboardProject {
    let mut services: Vec<_> = project
        .services
        .edges
        .into_iter()
        .map(|edge| {
            let service = edge.node;
            let instance = find_service_instance(&instances, &service.id);
            let domains = instance
                .map(|instance| {
                    instance
                        .domains
                        .service_domains
                        .iter()
                        .map(|domain| domain.domain.clone())
                        .chain(
                            instance
                                .domains
                                .custom_domains
                                .iter()
                                .map(|domain| domain.domain.clone()),
                        )
                        .collect()
                })
                .unwrap_or_default();
            let latest_deployment = instance.and_then(|instance| {
                instance
                    .latest_deployment
                    .as_ref()
                    .map(|deployment| DeploymentSummary {
                        id: deployment.id.clone(),
                        status: format!("{:?}", deployment.status),
                        created_at: deployment.created_at,
                        can_redeploy: deployment.can_redeploy,
                        stopped: deployment.deployment_stopped,
                    })
            });
            let volume_mounts = instances
                .volume_instances
                .iter()
                .filter(|volume| volume.node.service_id.as_deref() == Some(service.id.as_str()))
                .map(|volume| format!("{} → {}", volume.node.volume.name, volume.node.mount_path))
                .collect();

            DashboardService {
                id: service.id,
                name: service.name,
                active_in_environment: instance.is_some(),
                num_replicas: instance.and_then(|instance| instance.num_replicas),
                latest_deployment,
                domains,
                source_repo: instance.and_then(|instance| {
                    instance
                        .source
                        .as_ref()
                        .and_then(|source| source.repo.clone())
                        .filter(|repo| !repo.is_empty())
                }),
                source_image: instance.and_then(|instance| {
                    instance
                        .source
                        .as_ref()
                        .and_then(|source| source.image.clone())
                        .filter(|image| !image.is_empty())
                }),
                cron_schedule: instance.and_then(|instance| instance.cron_schedule.clone()),
                next_cron_run_at: instance.and_then(|instance| instance.next_cron_run_at),
                start_command: instance.and_then(|instance| instance.start_command.clone()),
                volume_mounts,
            }
        })
        .collect();
    services.sort_by_key(|service| service.name.to_ascii_lowercase());

    DashboardProject {
        id: project.id,
        name: project.name,
        workspace_name: project.workspace.map(|workspace| workspace.name),
        selected_environment_id: selected_environment.id,
        selected_environment_name: selected_environment.name,
        environments: project
            .environments
            .edges
            .into_iter()
            .map(|environment| EnvironmentSummary {
                id: environment.node.id,
                name: environment.node.name,
                deleted: environment.node.deleted_at.is_some(),
                accessible: environment.node.can_access,
            })
            .collect(),
        services,
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

    #[test]
    fn accessible_environment_filter_excludes_deleted_and_restricted_entries() {
        let project = DashboardProject {
            id: "proj_123".to_string(),
            name: "api".to_string(),
            workspace_name: Some("platform".to_string()),
            selected_environment_id: "env_123".to_string(),
            selected_environment_name: "production".to_string(),
            environments: vec![
                EnvironmentSummary {
                    id: "env_123".to_string(),
                    name: "production".to_string(),
                    deleted: false,
                    accessible: true,
                },
                EnvironmentSummary {
                    id: "env_456".to_string(),
                    name: "staging".to_string(),
                    deleted: false,
                    accessible: false,
                },
                EnvironmentSummary {
                    id: "env_789".to_string(),
                    name: "old".to_string(),
                    deleted: true,
                    accessible: true,
                },
            ],
            services: Vec::new(),
        };

        assert_eq!(
            project
                .accessible_environments()
                .into_iter()
                .map(|environment| environment.name.as_str())
                .collect::<Vec<_>>(),
            vec!["production"]
        );
    }
}
