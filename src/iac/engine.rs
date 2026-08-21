use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::sleep;

use crate::{
    client::{GQLClient, post_graphql_raw},
    config::{Configs, LinkedProject},
};

use super::change_set::{DiffOptions, diff_graphs, render_change_set};
use super::compiler::{EnvironmentConfigToGraphOptions, environment_config_to_graph};
use super::eval::evaluate_file;
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
struct EdgesQuery<T> {
    project: Option<Connection<T>>,
}

#[derive(Debug, Deserialize)]
struct Connection<T> {
    edges: Vec<Edge<T>>,
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

pub async fn run(
    args: &NativeRun,
    configs: &Configs,
    linked_project: &LinkedProject,
    command: &str,
) -> Result<Value> {
    let cwd = std::env::current_dir()?;
    let file = args
        .file
        .clone()
        .or_else(|| find_authoring_file(&cwd))
        .context("Could not find .railway/railway.ts, railway.py, or railway.go")?;
    let evaluated = evaluate_file(&file)?;
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
    let change_set = diff_graphs(DiffOptions {
        current: &current_graph,
        desired: &evaluated.graph,
        reveal_values: args.show_values,
        partial: evaluated.partial.as_deref(),
        owners: owners.as_ref(),
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
    })?;
    Ok(serialized)
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
    if let Ok(data) = post_graphql_raw::<EdgesQuery<Named>, _>(
        client,
        endpoint,
        "query IacProjectServices($projectId: String!) { project(id: $projectId) { services(first: 1000) { edges { node { id name } } } } }",
        json!({ "projectId": project_id }),
    )
    .await
    {
        for edge in data.project.unwrap_or(Connection { edges: vec![] }).edges {
            if let Some(name) = edge.node.name {
                options.service_names_by_id.insert(edge.node.id, json!(name));
            }
        }
    }
    if let Ok(data) = post_graphql_raw::<EdgesQuery<Named>, _>(
        client,
        endpoint,
        "query IacProjectVolumes($projectId: String!) { project(id: $projectId) { volumes(first: 1000) { edges { node { id name } } } } }",
        json!({ "projectId": project_id }),
    )
    .await
    {
        for edge in data.project.unwrap_or(Connection { edges: vec![] }).edges {
            if let Some(name) = edge.node.name {
                options.volume_names_by_id.insert(edge.node.id.clone(), json!(name));
            }
            if let Some(refs) = current.canvas_group_refs.as_ref().and_then(Value::as_object) {
                if let Some(group_id) = refs.get(&edge.node.id) {
                    options.volume_group_ids_by_id.insert(edge.node.id, group_id.clone());
                }
            }
        }
    }
    if let Ok(data) = post_graphql_raw::<EdgesQuery<Named>, _>(
        client,
        endpoint,
        "query IacProjectBuckets($projectId: String!) { project(id: $projectId) { buckets(first: 1000) { edges { node { id name groupId } } } } }",
        json!({ "projectId": project_id }),
    )
    .await
    {
        for edge in data.project.unwrap_or(Connection { edges: vec![] }).edges {
            if let Some(name) = edge.node.name {
                options.bucket_names_by_id.insert(edge.node.id.clone(), json!(name));
            }
            if let Some(group_id) = edge.node.group_id {
                options.bucket_group_ids_by_id.insert(edge.node.id, json!(group_id));
            }
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

async fn apply_change_set(
    client: &reqwest::Client,
    endpoint: &str,
    environment_id: &str,
    change_set: &super::change_set::ChangeSet,
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
