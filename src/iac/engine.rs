use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::sleep;

use crate::{
    client::{GQLClient, post_graphql_raw},
    config::{Configs, LinkedProject},
};

use super::change_set::{ChangeSetTelemetry, DiffOptions, diff_graphs, render_change_set};
use super::compiler::{EnvironmentConfigToGraphOptions, environment_config_to_graph};
use super::eval::{EvalContext, evaluate_file_with_context};
use super::graph::validate_graph;
use super::partial::needs_partial_claim_apply;

#[derive(Debug, Deserialize)]
struct EnvQuery {
    environment: EnvNode,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvNode {
    id: String,
    name: Option<String>,
    project_id: Option<String>,
    config: Value,
    config_etag: Option<String>,
    canvas_group_refs: Option<Value>,
    iac_partials: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct NameQuery {
    project: Option<NameNode>,
}

#[derive(Debug, Deserialize)]
struct NameNode {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectServicesQuery {
    project: Option<ProjectServices>,
}

#[derive(Debug, Deserialize)]
struct ProjectServices {
    #[serde(default)]
    services: Connection<Named>,
}

#[derive(Debug, Deserialize)]
struct ProjectVolumesQuery {
    project: Option<ProjectVolumes>,
}

#[derive(Debug, Deserialize)]
struct ProjectVolumes {
    #[serde(default)]
    volumes: Connection<Named>,
}

#[derive(Debug, Deserialize)]
struct ProjectBucketsQuery {
    project: Option<ProjectBuckets>,
}

#[derive(Debug, Deserialize)]
struct ProjectBuckets {
    #[serde(default)]
    buckets: Connection<Named>,
}

#[derive(Debug, Deserialize)]
struct Connection<T> {
    edges: Vec<Edge<T>>,
}

impl<T> Default for Connection<T> {
    fn default() -> Self {
        Self { edges: Vec::new() }
    }
}

impl Default for ProjectServices {
    fn default() -> Self {
        Self {
            services: Connection::default(),
        }
    }
}

impl Default for ProjectVolumes {
    fn default() -> Self {
        Self {
            volumes: Connection::default(),
        }
    }
}

impl Default for ProjectBuckets {
    fn default() -> Self {
        Self {
            buckets: Connection::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Edge<T> {
    node: T,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Named {
    id: String,
    name: Option<String>,
    #[serde(default)]
    group_id: Option<String>,
}

pub struct NativeRun {
    pub file: Option<std::path::PathBuf>,
    pub decrypt_variables: bool,
    pub show_values: bool,
}

fn authoring_language(file: &std::path::Path) -> &'static str {
    match file
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "py" => "python",
        "go" => "go",
        _ => "typescript",
    }
}

pub async fn run(
    args: &NativeRun,
    configs: &Configs,
    linked_project: &LinkedProject,
    command: &str,
) -> Result<Value> {
    if command == "current" {
        return import_current_environment(args, configs, linked_project).await;
    }

    let cwd = std::env::current_dir()?;
    let file = args
        .file
        .clone()
        .or_else(|| find_authoring_file(&cwd))
        .context("Could not find .railway/railway.ts, railway.py, or railway.go")?;
    let evaluated = evaluate_file_with_context(
        &file,
        &EvalContext::from_linked_project(linked_project, command),
    )?;
    let diagnostics: Vec<Value> = validate_graph(&evaluated.graph)
        .into_iter()
        .map(|message| json!({ "severity": "error", "path": "graph", "message": message }))
        .collect();

    let client = GQLClient::new_authorized(configs)?;
    let endpoint = configs.get_backboard();
    let environment_id = linked_project.environment_id()?;
    let current =
        fetch_current_environment(&client, &endpoint, environment_id, args.decrypt_variables)
            .await?;
    let mut options = EnvironmentConfigToGraphOptions {
        project_name: linked_project
            .name
            .clone()
            .or_else(|| Some(evaluated.graph.project.name.clone())),
        ..Default::default()
    };
    if let Some(project_id) = current
        .project_id
        .clone()
        .or_else(|| Some(linked_project.project.clone()))
    {
        fill_name_maps(
            &client,
            &endpoint,
            &project_id,
            current.id.as_str(),
            &current,
            &mut options,
        )
        .await?;
    }
    let current_graph = environment_config_to_graph(&current.config, &options);
    let owners = parse_owners(current.iac_partials.as_ref());
    let mut change_set = diff_graphs(DiffOptions {
        current: &current_graph,
        desired: &evaluated.graph,
        reveal_values: args.show_values,
        partial: evaluated.partial.as_deref(),
        owners: owners.as_ref(),
    });
    change_set.telemetry = Some(ChangeSetTelemetry {
        language: authoring_language(&evaluated.file).to_string(),
    });
    let mut all_diagnostics = diagnostics;
    for diagnostic in &change_set.diagnostics {
        all_diagnostics.push(json!({
            "severity": diagnostic.severity,
            "path": diagnostic.path,
            "message": diagnostic.message,
        }));
    }
    let ok = all_diagnostics
        .iter()
        .all(|d| d.get("severity").and_then(Value::as_str) != Some("error"));

    // Environment config reads mask variable values unless decryption was
    // requested, so the local diff can report phantom variable updates (the
    // current value looks like preserve()). Backboard's preview compares the
    // change set against decrypted state server-side and drops those no-ops;
    // plan/apply must use the previewed changes or variables never converge.
    let mut preview = None;
    if ok && !change_set.changes.is_empty() {
        let previewed = preview_change_set(&client, &endpoint, &current.id, &change_set).await?;
        if let Some(changes) = previewed
            .get("changeSet")
            .and_then(|set| set.get("changes"))
            .and_then(Value::as_array)
        {
            change_set.changes = changes.clone();
        }
        preview = Some(previewed);
    }

    let claim = needs_partial_claim_apply(
        &change_set.declared,
        owners.as_ref(),
        evaluated.partial.as_deref(),
    );

    let mut apply_result = None;
    if command == "apply" && ok && (!change_set.changes.is_empty() || claim) {
        apply_result = Some(
            apply_change_set(
                &client,
                &endpoint,
                &current.id,
                &change_set,
                current.config_etag.as_deref(),
            )
            .await?,
        );
    }

    let serialized = serde_json::to_value(RunnerWire {
        ok,
        command: command.to_string(),
        file: evaluated.file.to_string_lossy().to_string(),
        current_environment: Some(json!({
            "projectId": current.project_id,
            "projectName": options.project_name,
            "environmentId": current.id,
            "environmentName": current.name,
            "configEtag": current.config_etag,
        })),
        change_set: Some(change_set.clone()),
        diff: Some(render_change_set(&change_set)),
        diagnostics: all_diagnostics,
        current_graph: Some(current_graph),
        desired_graph: Some(evaluated.graph),
        apply_result,
        claim,
        preview,
    })?;
    Ok(serialized)
}

/// Import live environment state without evaluating an authoring file.
///
/// `railway config pull` only needs the current graph. Evaluating a stub
/// `service("web")` file was colliding with Config as Code ownership checks
/// and required the `railway` npm package to be installed.
async fn import_current_environment(
    args: &NativeRun,
    configs: &Configs,
    linked_project: &LinkedProject,
) -> Result<Value> {
    let client = GQLClient::new_authorized(configs)?;
    let endpoint = configs.get_backboard();
    let environment_id = linked_project.environment_id()?;
    let current =
        fetch_current_environment(&client, &endpoint, environment_id, args.decrypt_variables)
            .await?;
    let mut options = EnvironmentConfigToGraphOptions {
        project_name: linked_project.name.clone(),
        ..Default::default()
    };
    if let Some(project_id) = current
        .project_id
        .clone()
        .or_else(|| Some(linked_project.project.clone()))
    {
        fill_name_maps(
            &client,
            &endpoint,
            &project_id,
            current.id.as_str(),
            &current,
            &mut options,
        )
        .await?;
    }
    let current_graph = environment_config_to_graph(&current.config, &options);
    Ok(serde_json::to_value(RunnerWire {
        ok: true,
        command: "current".to_string(),
        file: args
            .file
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default(),
        current_environment: Some(json!({
            "projectId": current.project_id,
            "projectName": options.project_name,
            "environmentId": current.id,
            "environmentName": current.name,
            "configEtag": current.config_etag,
        })),
        change_set: None,
        diff: None,
        diagnostics: Vec::new(),
        current_graph: Some(current_graph),
        desired_graph: None,
        apply_result: None,
        claim: false,
        preview: None,
    })?)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerWire {
    ok: bool,
    command: String,
    file: String,
    current_environment: Option<Value>,
    change_set: Option<super::change_set::ChangeSet>,
    diff: Option<String>,
    diagnostics: Vec<Value>,
    current_graph: Option<super::graph::RailwayGraph>,
    desired_graph: Option<super::graph::RailwayGraph>,
    apply_result: Option<Value>,
    claim: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<Value>,
}

async fn fetch_current_environment(
    client: &reqwest::Client,
    endpoint: &str,
    environment_id: &str,
    decrypt_variables: bool,
) -> Result<EnvNode> {
    let query_with_partials = r#"
      query IacEnvironmentConfig($environmentId: String!, $decryptVariables: Boolean) {
        environment(id: $environmentId) {
          id name projectId config(decryptVariables: $decryptVariables) configEtag canvasGroupRefs iacPartials
        }
      }
    "#;
    let vars = json!({ "environmentId": environment_id, "decryptVariables": decrypt_variables });
    match post_graphql_raw::<EnvQuery, _>(client, endpoint, query_with_partials, vars.clone()).await
    {
        Ok(data) => Ok(data.environment),
        Err(err) if err.to_string().contains("iacPartials") => {
            let query = r#"
              query IacEnvironmentConfig($environmentId: String!, $decryptVariables: Boolean) {
                environment(id: $environmentId) {
                  id name projectId config(decryptVariables: $decryptVariables) configEtag canvasGroupRefs
                }
              }
            "#;
            Ok(
                post_graphql_raw::<EnvQuery, _>(client, endpoint, query, vars)
                    .await?
                    .environment,
            )
        }
        Err(err) => Err(err.into()),
    }
}

async fn fill_name_maps(
    client: &reqwest::Client,
    endpoint: &str,
    project_id: &str,
    environment_id: &str,
    current: &EnvNode,
    options: &mut EnvironmentConfigToGraphOptions,
) -> Result<()> {
    if let Ok(data) = post_graphql_raw::<NameQuery, _>(
        client,
        endpoint,
        "query IacProjectName($projectId: String!) { project(id: $projectId) { name } }",
        json!({ "projectId": project_id }),
    )
    .await
    {
        if let Some(name) = data.project.and_then(|p| p.name) {
            options.project_name = Some(name);
        }
    }
    let services = post_graphql_raw::<ProjectServicesQuery, _>(
        client,
        endpoint,
        "query IacProjectServices($projectId: String!) { project(id: $projectId) { services(first: 1000) { edges { node { id name } } } } }",
        json!({ "projectId": project_id }),
    )
    .await
    .context("Failed to load project services for IaC name maps")?;
    for edge in services.project.unwrap_or_default().services.edges {
        if let Some(name) = edge.node.name {
            options
                .service_names_by_id
                .insert(edge.node.id, json!(name));
        }
    }

    let volumes = post_graphql_raw::<ProjectVolumesQuery, _>(
        client,
        endpoint,
        "query IacProjectVolumes($projectId: String!) { project(id: $projectId) { volumes(first: 1000) { edges { node { id name } } } } }",
        json!({ "projectId": project_id }),
    )
    .await
    .context("Failed to load project volumes for IaC name maps")?;
    for edge in volumes.project.unwrap_or_default().volumes.edges {
        if let Some(name) = edge.node.name {
            options
                .volume_names_by_id
                .insert(edge.node.id.clone(), json!(name));
        }
        if let Some(refs) = current
            .canvas_group_refs
            .as_ref()
            .and_then(Value::as_object)
        {
            if let Some(group_id) = refs.get(&edge.node.id) {
                options
                    .volume_group_ids_by_id
                    .insert(edge.node.id, group_id.clone());
            }
        }
    }

    let buckets = post_graphql_raw::<ProjectBucketsQuery, _>(
        client,
        endpoint,
        "query IacProjectBuckets($projectId: String!) { project(id: $projectId) { buckets(first: 1000) { edges { node { id name groupId } } } } }",
        json!({ "projectId": project_id }),
    )
    .await
    .context("Failed to load project buckets for IaC name maps")?;
    for edge in buckets.project.unwrap_or_default().buckets.edges {
        if let Some(name) = edge.node.name {
            options
                .bucket_names_by_id
                .insert(edge.node.id.clone(), json!(name));
        }
        if let Some(group_id) = edge.node.group_id {
            options
                .bucket_group_ids_by_id
                .insert(edge.node.id, json!(group_id));
        }
    }
    let _ = environment_id;
    Ok(())
}

fn find_authoring_file(start: &std::path::Path) -> Option<std::path::PathBuf> {
    const NAMES: &[&str] = &["railway.ts", "railway.py", "railway.go"];
    for directory in start.ancestors() {
        let railway_dir =
            if directory.file_name().and_then(|name| name.to_str()) == Some(".railway") {
                directory.to_path_buf()
            } else {
                directory.join(".railway")
            };
        for name in NAMES {
            let candidate = railway_dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn parse_owners(value: Option<&Value>) -> Option<super::partial::IacPartials> {
    let object = value.and_then(Value::as_object)?;
    let mut out = super::partial::IacPartials::new();
    for (key, value) in object {
        if let Some(owner) = value.as_str() {
            out.insert(key.clone(), owner.to_string());
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

async fn preview_change_set(
    client: &reqwest::Client,
    endpoint: &str,
    environment_id: &str,
    change_set: &super::change_set::ChangeSet,
) -> Result<Value> {
    let mutation = r#"
      mutation IacPreviewChangeSet($environmentId: String!, $input: JSON!) {
        environmentPreviewChangeSet(environmentId: $environmentId, input: $input) {
          changeSet diagnostics effects
        }
      }
    "#;
    #[derive(Deserialize)]
    struct PreviewQuery {
        #[serde(rename = "environmentPreviewChangeSet")]
        result: Value,
    }
    let data = post_graphql_raw::<PreviewQuery, _>(
        client,
        endpoint,
        mutation,
        json!({ "environmentId": environment_id, "input": change_set }),
    )
    .await
    .context("Failed to preview change set")?;
    Ok(data.result)
}

pub(crate) async fn fetch_config_etag(
    configs: &Configs,
    environment_id: &str,
) -> Result<Option<String>> {
    let client = GQLClient::new_authorized(configs)?;
    let endpoint = configs.get_backboard();
    let current = fetch_current_environment(&client, &endpoint, environment_id, false).await?;
    Ok(current.config_etag)
}

pub(crate) async fn apply_change_set(
    client: &reqwest::Client,
    endpoint: &str,
    environment_id: &str,
    change_set: &impl serde::Serialize,
    base_etag: Option<&str>,
) -> Result<Value> {
    let mut variables = json!({
        "environmentId": environment_id,
        "input": change_set,
        "commitMessage": "Apply Railway configuration",
        "waitForCompletion": false,
    });
    if let Some(etag) = base_etag {
        variables["baseConfigEtag"] = json!(etag);
    }
    let mutation = r#"
      mutation IacApplyChangeSet($environmentId: String!, $input: JSON!, $commitMessage: String, $baseConfigEtag: String, $waitForCompletion: Boolean) {
        environmentApplyChangeSet(environmentId: $environmentId, input: $input, commitMessage: $commitMessage, baseConfigEtag: $baseConfigEtag, waitForCompletion: $waitForCompletion) {
          id status deploymentId stagedPatchId diagnostics changes { kind path summary status outputs }
        }
      }
    "#;
    #[derive(Deserialize)]
    struct ApplyQuery {
        #[serde(rename = "environmentApplyChangeSet")]
        result: ApplyResult,
    }
    #[derive(Deserialize)]
    struct ApplyResult {
        id: String,
        status: String,
        #[serde(flatten)]
        rest: Value,
    }
    let applied = post_graphql_raw::<ApplyQuery, _>(client, endpoint, mutation, variables).await?;
    if applied.result.status != "applying" {
        let mut value = serde_json::to_value(&applied.result.rest)?;
        value["id"] = json!(applied.result.id);
        value["status"] = json!(applied.result.status);
        return Ok(value);
    }
    wait_for_apply(client, endpoint, environment_id, &applied.result.id).await
}

async fn wait_for_apply(
    client: &reqwest::Client,
    endpoint: &str,
    environment_id: &str,
    id: &str,
) -> Result<Value> {
    let query = r#"
      query IacChangeSetApplyStatus($environmentId: String!, $id: String!) {
        environmentChangeSetApply(environmentId: $environmentId, id: $id) {
          id status deploymentId stagedPatchId diagnostics changes { kind path summary status outputs }
        }
      }
    "#;
    #[derive(Deserialize)]
    struct StatusQuery {
        #[serde(rename = "environmentChangeSetApply")]
        result: Value,
    }
    for _ in 0..120 {
        let data = post_graphql_raw::<StatusQuery, _>(
            client,
            endpoint,
            query,
            json!({ "environmentId": environment_id, "id": id }),
        )
        .await?;
        if data.result.get("status").and_then(Value::as_str) != Some("applying") {
            return Ok(data.result);
        }
        sleep(Duration::from_secs(1)).await;
    }
    bail!("Timed out waiting for Railway ChangeSet apply {id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_services_query_reads_nested_connection() {
        let data: ProjectServicesQuery = serde_json::from_value(json!({
            "project": {
                "services": {
                    "edges": [
                        { "node": { "id": "svc-1", "name": "eu-api" } }
                    ]
                }
            }
        }))
        .unwrap();
        let edges = data.project.unwrap().services.edges;
        assert_eq!(edges[0].node.id, "svc-1");
        assert_eq!(edges[0].node.name.as_deref(), Some("eu-api"));
    }

    #[test]
    fn project_volumes_and_buckets_query_read_nested_connections() {
        let volumes: ProjectVolumesQuery = serde_json::from_value(json!({
            "project": {
                "volumes": { "edges": [{ "node": { "id": "vol-1", "name": "data" } }] }
            }
        }))
        .unwrap();
        assert_eq!(
            volumes.project.unwrap().volumes.edges[0]
                .node
                .name
                .as_deref(),
            Some("data")
        );

        let buckets: ProjectBucketsQuery = serde_json::from_value(json!({
            "project": {
                "buckets": {
                    "edges": [{ "node": { "id": "bkt-1", "name": "uploads", "groupId": "grp-1" } }]
                }
            }
        }))
        .unwrap();
        let node = &buckets.project.unwrap().buckets.edges[0].node;
        assert_eq!(node.name.as_deref(), Some("uploads"));
        assert_eq!(node.group_id.as_deref(), Some("grp-1"));
    }

    #[test]
    fn leftover_edges_query_shape_cannot_read_services() {
        // The previous deserializer expected { project: { edges } }. The API
        // returns { project: { services: { edges } } }, so that shape dropped
        // every name map and plan treated live services as UUID deletes.
        #[derive(Deserialize)]
        struct Broken {
            project: Option<Connection<Named>>,
        }
        let broken = serde_json::from_value::<Broken>(json!({
            "project": {
                "services": {
                    "edges": [{ "node": { "id": "svc-1", "name": "eu-api" } }]
                }
            }
        }));
        assert!(
            broken.is_err(),
            "the live GraphQL payload is not {{ project: {{ edges }} }}"
        );
    }

    #[test]
    fn authoring_language_normalizes_supported_extensions() {
        assert_eq!(
            authoring_language(std::path::Path::new(".railway/railway.ts")),
            "typescript"
        );
        assert_eq!(
            authoring_language(std::path::Path::new(".railway/railway.py")),
            "python"
        );
        assert_eq!(
            authoring_language(std::path::Path::new(".railway/railway.go")),
            "go"
        );
    }
}
