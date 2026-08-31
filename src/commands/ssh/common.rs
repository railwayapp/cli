use anyhow::{Context, Result, anyhow};
use reqwest::Client;

use crate::commands::queries::RailwayProject;
use crate::config::Configs;
use crate::controllers::{
    environment::get_matched_environment,
    project::{get_project, resolve_project_id_or_name},
    service::get_or_prompt_service,
};

use super::Args;

pub struct SshConnectParams {
    pub project_id: String,
    pub environment_id: String,
    pub service_id: String,
    pub service_name: String,
}

pub fn find_service_by_name(
    project: &RailwayProject,
    service_id_or_name: &str,
) -> Result<(String, String)> {
    let services = project.services.edges.iter().collect::<Vec<_>>();

    let service = services
        .iter()
        .find(|service| {
            service.node.name.to_lowercase() == service_id_or_name.to_lowercase()
                || service.node.id == service_id_or_name
        })
        .with_context(|| format!("Service '{service_id_or_name}' not found"))?
        .node
        .to_owned();

    Ok((service.id, service.name))
}

pub async fn get_ssh_connect_params(
    args: Args,
    configs: &Configs,
    client: &Client,
) -> Result<SshConnectParams> {
    let needs_linked_project = args.project.is_none() || args.environment.is_none();

    let linked_project = if needs_linked_project {
        Some(configs.get_linked_project().await?)
    } else {
        None
    };

    let project_id = if let Some(project) = args.project {
        resolve_project_id_or_name(client, configs, &project).await?
    } else {
        linked_project.as_ref().unwrap().project.clone()
    };
    let project = get_project(client, configs, project_id.clone()).await?;

    let environment = if let Some(env) = args.environment {
        env
    } else {
        linked_project
            .as_ref()
            .unwrap()
            .environment_id()?
            .to_string()
    };
    let environment_id = get_matched_environment(&project, environment)?.id;

    let (service_id, service_name) = if let Some(service_id_or_name) = args.service {
        find_service_by_name(&project, &service_id_or_name)?
    } else if let [service] = project.services.edges.as_slice() {
        (service.node.id.clone(), service.node.name.clone())
    } else {
        let service_id = get_or_prompt_service(linked_project.clone(), project.clone(), None)
            .await?
            .ok_or_else(|| anyhow!("No service found. Please specify a service to connect to via the `--service` flag."))?;
        let service_name = project
            .services
            .edges
            .iter()
            .find(|service| service.node.id == service_id)
            .map(|service| service.node.name.clone())
            .unwrap_or_else(|| service_id.clone());

        (service_id, service_name)
    };

    Ok(SshConnectParams {
        project_id: project.id,
        environment_id,
        service_id,
        service_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MockBackboard;
    use serde_json::json;

    #[tokio::test]
    async fn resolves_explicit_project_name_without_a_link_or_service() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub_graphql_error("Project", "Project not found");
        server.stub(
            "UserProjects",
            json!({
                "externalWorkspaces": [],
                "me": {
                    "workspaces": [{
                        "id": "workspace-id",
                        "name": "Workspace",
                        "team": { "id": "team-id" },
                        "projects": {
                            "edges": [{
                                "node": {
                                    "id": "project-id",
                                    "name": "preview-environments",
                                    "createdAt": "2026-01-01T00:00:00Z",
                                    "updatedAt": "2026-01-01T00:00:00Z",
                                    "deletedAt": null,
                                    "environments": { "edges": [] },
                                    "services": { "edges": [] }
                                }
                            }]
                        }
                    }]
                }
            }),
        );
        server.stub(
            "Project",
            json!({
                "project": {
                    "id": "project-id",
                    "name": "preview-environments",
                    "workspaceId": "workspace-id",
                    "deletedAt": null,
                    "workspace": { "name": "Workspace" },
                    "buckets": { "edges": [] },
                    "environments": {
                        "edges": [{
                            "node": {
                                "id": "environment-id",
                                "name": "production",
                                "canAccess": true,
                                "deletedAt": null,
                                "unmergedChangesCount": 0
                            }
                        }]
                    },
                    "services": {
                        "edges": [{
                            "node": { "id": "service-id", "name": "api" }
                        }]
                    }
                }
            }),
        );

        let configs = server.configs(&dir);
        let client = reqwest::Client::new();
        let params = get_ssh_connect_params(
            Args {
                subcommand: None,
                project: Some("preview-environments".to_string()),
                service: None,
                environment: Some("production".to_string()),
                deployment_instance: None,
                session: None,
                native: false,
                identity_file: None,
                command: Vec::new(),
            },
            &configs,
            &client,
        )
        .await
        .unwrap();

        assert_eq!(params.project_id, "project-id");
        assert_eq!(params.environment_id, "environment-id");
        assert_eq!(params.service_id, "service-id");
        assert_eq!(
            server.variables_for("Project"),
            vec![
                json!({ "id": "preview-environments" }),
                json!({ "id": "project-id" })
            ]
        );
    }
}
