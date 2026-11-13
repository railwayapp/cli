use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::{Value, json};

use super::change_set::{ChangeSet, Diagnostic};
use super::eval::EvalContext;
use super::graph::{RailwayGraph, resource_addr, resource_name, resource_type};

/// A persistent project environment the file is evaluated against.
#[derive(Clone, Debug)]
pub struct PersistentEnvironment {
    pub id: String,
    pub name: String,
}

/// One evaluation of the authoring file. `is_target` selects the resource
/// bodies used for the plan; other evaluations only constrain existence.
#[derive(Clone, Debug)]
pub struct EnvironmentEvaluation {
    pub name: String,
    pub resources: Vec<Value>,
    pub is_target: bool,
}

#[derive(Clone, Debug)]
pub struct Exclusion {
    pub address: String,
    pub name: String,
    pub resource_type: String,
    pub environments: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ScopePlan {
    pub diagnostics: Vec<Diagnostic>,
    pub resources: Vec<Value>,
    pub exclusions: Vec<Exclusion>,
}

/// Target context, then each other persistent environment with `pr: null`.
/// A persistent target is not evaluated twice.
pub fn evaluation_contexts(
    target: &EvalContext,
    persistent: &[PersistentEnvironment],
) -> Vec<EvalContext> {
    let mut contexts = vec![target.clone()];
    let target_id = target.environment_id.as_deref();
    for env in persistent {
        if target.pr.is_none() && target_id == Some(env.id.as_str()) {
            continue;
        }
        let mut ctx = target.clone();
        ctx.environment_id = Some(env.id.clone());
        ctx.environment = Some(env.name.clone());
        ctx.environment_name = Some(env.name.clone());
        ctx.pr = None;
        contexts.push(ctx);
    }
    contexts
}

pub fn plan_scope(
    evaluations: &[EnvironmentEvaluation],
    project_environments: &[String],
    match_name: &str,
    check_names: bool,
) -> ScopePlan {
    let mut diagnostics = Vec::new();
    if evaluations.is_empty() {
        return ScopePlan::default();
    }
    let eval_names: Vec<&str> = evaluations.iter().map(|eval| eval.name.as_str()).collect();
    let mut present: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut display_names: BTreeMap<String, String> = BTreeMap::new();
    for eval in evaluations {
        let mut seen = BTreeSet::new();
        for resource in &eval.resources {
            let address = resource_addr(resource);
            if !seen.insert(address.clone()) {
                continue;
            }
            present
                .entry(address.clone())
                .or_default()
                .push(eval.name.clone());
            display_names
                .entry(address)
                .or_insert_with(|| display_name(resource));
        }
    }
    let mut conditional = BTreeSet::new();
    for (address, where_present) in &present {
        if where_present.len() == evaluations.len() {
            continue;
        }
        conditional.insert(address.clone());
        let missing = eval_names
            .iter()
            .copied()
            .filter(|name| !where_present.iter().any(|present| present == name))
            .collect::<Vec<_>>()
            .join(", ");
        let declared = where_present.join(", ");
        let suggestion = serde_json::to_string(where_present).unwrap_or_else(|_| "[]".into());
        let name = display_names
            .get(address)
            .map(String::as_str)
            .unwrap_or(address);
        diagnostics.push(error(
            address,
            format!(
                "{name} is declared when evaluating {declared} but not {missing}. Use environments: {suggestion} instead of a conditional."
            ),
        ));
    }

    let mut env_by_address: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for eval in evaluations {
        let mut seen = BTreeSet::new();
        for resource in &eval.resources {
            let address = resource_addr(resource);
            if !seen.insert(address.clone()) || conditional.contains(&address) {
                continue;
            }
            env_by_address
                .entry(address)
                .or_default()
                .push((eval.name.clone(), display_env_field(resource)));
        }
    }
    for (address, values) in &env_by_address {
        let first = &values[0].1;
        if values.iter().all(|(_, value)| value == first) {
            continue;
        }
        let listed = values
            .iter()
            .map(|(env, value)| format!("{env} {value}"))
            .collect::<Vec<_>>()
            .join(", ");
        let name = display_names
            .get(address)
            .map(String::as_str)
            .unwrap_or(address);
        diagnostics.push(error(
            address,
            format!("{name} environments differ across evaluations: {listed}."),
        ));
    }

    let target = evaluations
        .iter()
        .find(|eval| eval.is_target)
        .unwrap_or(&evaluations[0]);
    let mut resources = Vec::new();
    let mut exclusions = Vec::new();
    let mut seen = BTreeSet::new();
    for resource in &target.resources {
        let address = resource_addr(resource);
        if !seen.insert(address.clone()) || conditional.contains(&address) {
            continue;
        }
        if env_by_address
            .get(&address)
            .is_some_and(|values| values.iter().any(|(_, value)| values[0].1 != *value))
        {
            continue;
        }
        let parsed = match parse_environments(resource) {
            Ok(parsed) => parsed,
            Err(message) => {
                diagnostics.push(error(&address, message));
                continue;
            }
        };
        if check_names {
            if let Some(names) = &parsed {
                let known = project_environments.iter().collect::<BTreeSet<_>>();
                for name in names {
                    if known.contains(name) {
                        continue;
                    }
                    let listed = list_environments(project_environments);
                    diagnostics.push(error(
                        &address,
                        format!(
                            "Unknown environment \"{name}\" on {}. Project environments: {listed}.",
                            display_name(resource)
                        ),
                    ));
                }
            }
        }
        let included = match &parsed {
            None => true,
            Some(names) => names.iter().any(|name| name == match_name),
        };
        if included {
            resources.push(without_environments(resource));
            continue;
        }
        exclusions.push(Exclusion {
            address,
            name: display_name(resource),
            resource_type: resource_type(resource).to_string(),
            environments: parsed.unwrap_or_default(),
        });
    }
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == "error")
    {
        return ScopePlan {
            diagnostics,
            resources: Vec::new(),
            exclusions: Vec::new(),
        };
    }
    ScopePlan {
        diagnostics,
        resources,
        exclusions,
    }
}

pub fn graph_with_resources(graph: &RailwayGraph, resources: Vec<Value>) -> RailwayGraph {
    let addresses: HashSet<String> = resources.iter().map(resource_addr).collect();
    let mut graph = graph.clone();
    graph
        .edges
        .retain(|edge| addresses.contains(&edge.from) && addresses.contains(&edge.to));
    graph.resources = resources;
    graph
}

/// `resource.delete` is the change set the server compiles to a service-instance
/// `isDeleted` patch: that drops this environment and deletes the service only
/// when no instance remains.
/// True only when the instance list includes the target and no other environment.
/// An incomplete list (target missing despite a live instance) is not a claim.
pub fn last_instances(
    exclusions: &[Exclusion],
    current_addresses: &HashSet<String>,
    instances_by_name: &BTreeMap<String, Vec<String>>,
    target_environment_id: &str,
) -> BTreeMap<String, bool> {
    let mut last = BTreeMap::new();
    for exclusion in exclusions {
        if !matches!(exclusion.resource_type.as_str(), "service" | "database") {
            continue;
        }
        if !current_addresses.contains(&exclusion.address) {
            continue;
        }
        let Some(ids) = instances_by_name.get(&exclusion.name) else {
            continue;
        };
        if !ids.iter().any(|id| id == target_environment_id) {
            continue;
        }
        if ids.iter().any(|id| id != target_environment_id) {
            continue;
        }
        last.insert(exclusion.address.clone(), true);
    }
    last
}

pub fn apply_exclusions(
    change_set: &mut ChangeSet,
    current: &RailwayGraph,
    exclusions: &[Exclusion],
    target_name: &str,
    last_instance: &BTreeMap<String, bool>,
) {
    let current_by: BTreeMap<String, Value> = current
        .resources
        .iter()
        .map(|resource| (resource_addr(resource), resource.clone()))
        .collect();
    let mut info = Vec::new();
    for exclusion in exclusions {
        let Some(previous) = current_by.get(&exclusion.address) else {
            info.push(format!(
                "{} · not in {target_name} (environments: {})",
                exclusion.name,
                exclusion.environments.join(", ")
            ));
            continue;
        };
        let project_wide = last_instance
            .get(&exclusion.address)
            .copied()
            .unwrap_or(false);
        let summary = removal_summary(exclusion, target_name, project_wide);
        if let Some(change) = change_set.changes.iter_mut().find(|change| {
            change.get("kind").and_then(Value::as_str) == Some("resource.delete")
                && change.get("address").and_then(Value::as_str) == Some(exclusion.address.as_str())
        }) {
            change["summary"] = json!(summary);
            continue;
        }
        change_set.changes.push(json!({
            "kind": "resource.delete",
            "address": exclusion.address,
            "previous": previous,
            "path": format!("resources.{}", exclusion.address),
            "summary": summary,
            "severity": "destructive",
            "deployEffect": if matches!(exclusion.resource_type.as_str(), "service" | "database") {
                "deploy"
            } else {
                "none"
            },
        }));
        let path = format!("resources.{}", exclusion.address);
        change_set.diagnostics.retain(|diagnostic| {
            diagnostic.path != path || !diagnostic.message.contains("never deleted")
        });
    }
    change_set.info = info;
}

fn removal_summary(exclusion: &Exclusion, target_name: &str, project_wide: bool) -> String {
    let summary = format!(
        "Remove {} {} from {target_name} (environments: {})",
        exclusion.resource_type,
        exclusion.name,
        exclusion.environments.join(", ")
    );
    if project_wide {
        format!("{summary}; this deletes it from the project")
    } else {
        summary
    }
}

pub fn without_environments(resource: &Value) -> Value {
    let mut resource = resource.clone();
    if let Some(object) = resource.as_object_mut() {
        object.remove("environments");
    }
    resource
}

fn parse_environments(resource: &Value) -> Result<Option<Vec<String>>, String> {
    let Some(value) = resource
        .get("environments")
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    let name = display_name(resource);
    let Some(items) = value.as_array() else {
        return Err(format!(
            "environments on {name} must be an array of environment names."
        ));
    };
    let mut names = Vec::new();
    for item in items {
        let Some(item) = item.as_str() else {
            return Err(format!(
                "environments on {name} must be an array of environment names."
            ));
        };
        names.push(item.to_string());
    }
    Ok(Some(names))
}

fn display_env_field(resource: &Value) -> String {
    match resource.get("environments") {
        None | Some(Value::Null) => "omitted".to_string(),
        Some(value) => value.to_string(),
    }
}

fn display_name(resource: &Value) -> String {
    let name = resource_name(resource);
    if name.is_empty() {
        resource_addr(resource)
    } else {
        name.to_string()
    }
}

fn list_environments(names: &[String]) -> String {
    if names.is_empty() {
        return "none".to_string();
    }
    let mut names = names.to_vec();
    names.sort();
    names.dedup();
    names.join(", ")
}

fn error(address: &str, message: String) -> Diagnostic {
    Diagnostic {
        severity: "error".into(),
        path: format!("resources.{address}"),
        message,
    }
}
