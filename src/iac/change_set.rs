use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::graph::{RailwayGraph, resource_addr, resource_name, resource_type};
use super::json::{field, field_str, stable_stringify};
use super::partial::{
    IacPartials, effective_partial, foreign_resource_message, has_named_partials,
    nameless_file_message, owner_of,
};

pub const RAILWAY_CHANGE_SET_VERSION: u32 = 1;
const MASKED_CREDENTIAL_VALUE: &str = "*****";
const REDACTED_VARIABLE_VALUE: &str = "«hidden»";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ChangeSet {
    pub version: u32,
    pub changes: Vec<Value>,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Diagnostic {
    pub severity: String,
    pub path: String,
    pub message: String,
}

pub struct DiffOptions<'a> {
    pub current: &'a RailwayGraph,
    pub desired: &'a RailwayGraph,
    pub reveal_values: bool,
    pub partial: Option<&'a str>,
    pub owners: Option<&'a IacPartials>,
}

pub fn diff_graphs(options: DiffOptions<'_>) -> ChangeSet {
    let mut changes = Vec::new();
    let mut diagnostics = Vec::new();
    let current_by: Map<String, Value> = options
        .current
        .resources
        .iter()
        .map(|resource| (resource_addr(resource), resource.clone()))
        .collect();
    let desired_by: Map<String, Value> = options
        .desired
        .resources
        .iter()
        .map(|resource| (resource_addr(resource), resource.clone()))
        .collect();
    let p = effective_partial(options.partial).to_string();
    let declared: Vec<String> = options
        .desired
        .resources
        .iter()
        .map(resource_addr)
        .collect();

    if p == super::partial::PROJECT_PARTIAL && has_named_partials(options.owners) {
        diagnostics.push(Diagnostic {
            severity: "error".into(),
            path: "partial".into(),
            message: nameless_file_message(),
        });
        return change_set_result(changes, diagnostics, options.partial, declared);
    }

    for resource in &options.desired.resources {
        let address = resource_addr(resource);
        let previous = current_by.get(&address);
        if let Some(owner) = owner_of(options.owners, &address) {
            if owner != p {
                diagnostics.push(Diagnostic {
                    severity: "error".into(),
                    path: format!("resources.{address}"),
                    message: foreign_resource_message(&address, owner),
                });
                continue;
            }
        }
        if let Some(previous) = previous {
            if is_managed_by_repo_config(previous) {
                let config_file = field_str(previous, "configFile").unwrap_or("repo config");
                diagnostics.push(Diagnostic {
                    severity: "error".into(),
                    path: format!("resources.{address}.configFile"),
                    message: format!(
                        "{} is already managed by {config_file}. Remove or migrate the repo config before managing this service from .railway/railway.ts.",
                        resource_name(previous)
                    ),
                });
                continue;
            }
        }
        if previous.is_none() {
            diagnose_unsupported_custom_domains(resource, &mut diagnostics, None);
            changes.push(json!({
                "kind": "resource.create",
                "address": address,
                "resource": resource,
                "path": format!("resources.{address}"),
                "summary": format!("Create {} {}", resource_type(resource), resource_name(resource)),
                "severity": "safe",
                "deployEffect": if matches!(resource_type(resource), "service" | "database") { "deploy" } else { "none" },
            }));
            continue;
        }
        let previous = previous.unwrap();
        if field_str(previous, "name") != field_str(resource, "name") {
            changes.push(update(
                &address,
                "name",
                json!(field_str(previous, "name")),
                json!(field_str(resource, "name")),
                format!(
                    "Rename {} {} to {}",
                    resource_type(resource),
                    resource_name(previous),
                    resource_name(resource)
                ),
                None,
                "safe",
            ));
        }
        diff_variables(
            previous,
            resource,
            &mut changes,
            &desired_by,
            options.reveal_values,
        );
        diff_top_level_field(previous, resource, "source", &mut changes);
        diff_top_level_field(previous, resource, "build", &mut changes);
        if resource_type(previous) == "database" && resource_type(resource) == "database" {
            diff_database_deploy(previous, resource, &mut changes);
        } else if resource_type(previous) == "service" && resource_type(resource) == "service" {
            diff_service_deploy(previous, resource, &mut changes);
        } else {
            diff_top_level_field(previous, resource, "deploy", &mut changes);
        }
        diff_top_level_field(previous, resource, "groupId", &mut changes);
        diff_networking(previous, resource, &mut changes, &mut diagnostics);
        if resource_type(previous) == "bucket" && resource_type(resource) == "bucket" {
            if bucket_region(previous) != bucket_region(resource) {
                diagnostics.push(Diagnostic {
                    severity: "error".into(),
                    path: format!("resources.{address}.config.region"),
                    message: format!(
                        "Bucket region cannot be changed after creation. Create a new bucket in {} and migrate data instead.",
                        bucket_region(resource).unwrap_or_else(|| "the desired region".to_string())
                    ),
                });
            } else {
                diff_top_level_field(previous, resource, "config", &mut changes);
            }
        } else if resource_type(previous) == "volume" && resource_type(resource) == "volume" {
            diff_volume_config(previous, resource, &mut changes);
        } else {
            diff_top_level_field(previous, resource, "config", &mut changes);
        }
        diff_volume_attachments(previous, resource, &mut changes);
    }

    for resource in &options.current.resources {
        let address = resource_addr(resource);
        if desired_by.contains_key(&address) {
            continue;
        }
        if p != super::partial::PROJECT_PARTIAL
            && owner_of(options.owners, &address) != Some(p.as_str())
        {
            continue;
        }
        if resource_type(resource) == "volume" {
            let mounted = options.current.edges.iter().any(|edge| {
                edge.kind == "mount" && edge.to == address && desired_by.contains_key(&edge.from)
            });
            if mounted {
                diagnostics.push(Diagnostic {
                    severity: "warning".into(),
                    path: format!("resources.{address}"),
                    message: format!(
                        "Volume {} exists on Railway but is not declared in the config. Volumes are never deleted by config apply; delete it from the dashboard if that is intended.",
                        resource_name(resource)
                    ),
                });
            }
            continue;
        }
        changes.push(json!({
            "kind": "resource.delete",
            "address": address,
            "previous": resource,
            "path": format!("resources.{address}"),
            "summary": format!("Delete {} {}", resource_type(resource), resource_name(resource)),
            "severity": "destructive",
            "deployEffect": if matches!(resource_type(resource), "service" | "database") { "deploy" } else { "none" },
        }));
    }

    change_set_result(changes, diagnostics, options.partial, declared)
}

fn change_set_result(
    changes: Vec<Value>,
    diagnostics: Vec<Diagnostic>,
    partial: Option<&str>,
    declared: Vec<String>,
) -> ChangeSet {
    ChangeSet {
        version: RAILWAY_CHANGE_SET_VERSION,
        changes,
        diagnostics,
        partial: partial.map(str::to_string),
        declared,
    }
}

pub fn render_change_set(change_set: &ChangeSet) -> String {
    if change_set.changes.is_empty() {
        return "No changes.".to_string();
    }
    change_set
        .changes
        .iter()
        .map(|change| {
            let marker = match field_str(change, "kind") {
                Some("resource.create") | Some("domain.create") => "+",
                Some("resource.delete") => "-",
                _ => "~",
            };
            format!(
                "{marker} {}",
                field_str(change, "summary").unwrap_or("change")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn diff_variables(
    previous: &Value,
    resource: &Value,
    changes: &mut Vec<Value>,
    resources_by_address: &Map<String, Value>,
    reveal_values: bool,
) {
    if previous.get("variables").is_none() && resource.get("variables").is_none() {
        return;
    }
    let before = previous
        .get("variables")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let after = resource
        .get("variables")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, value) in &after {
        if is_preserved_variable(value) {
            continue;
        }
        let current = before.get(key);
        if !is_unknown_current_variable(current)
            && stable_stringify(&normalize_variable_for_diff(current, resources_by_address))
                == stable_stringify(&normalize_variable_for_diff(
                    Some(value),
                    resources_by_address,
                ))
        {
            continue;
        }
        let mut change = json!({
            "kind": "variable.set",
            "address": resource_addr(resource),
            "variable": key,
            "after": value,
            "path": format!("resources.{}.variables.{key}", resource_addr(resource)),
            "summary": format!("{} variable {}.{}", if current.is_some() { "Update" } else { "Set" }, resource_name(resource), key),
            "details": [format!(
                "{}.{} ({} → {})",
                resource_name(resource),
                key,
                format_variable_diff_value(current, resources_by_address, reveal_values),
                format_variable_diff_value(Some(value), resources_by_address, reveal_values)
            )],
            "severity": "safe",
            "deployEffect": "deploy",
        });
        if let Some(current) = current {
            change["before"] = current.clone();
        }
        changes.push(change);
    }
    for (key, value) in &before {
        if after.contains_key(key) {
            continue;
        }
        changes.push(json!({
            "kind": "variable.delete",
            "address": resource_addr(resource),
            "variable": key,
            "previous": value,
            "path": format!("resources.{}.variables.{key}", resource_addr(resource)),
            "summary": format!("Delete variable {}.{}", resource_name(resource), key),
            "severity": "destructive",
            "deployEffect": "deploy",
        }));
    }
}

fn diff_networking(
    previous: &Value,
    resource: &Value,
    changes: &mut Vec<Value>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let before = previous.get("networking");
    let after = resource.get("networking");
    let before_domains = before.and_then(|n| n.get("customDomains"));
    diagnose_unsupported_custom_domains(resource, diagnostics, before_domains);
    let mut before_copy = before.cloned().unwrap_or(json!({}));
    let mut after_copy = after.cloned().unwrap_or(json!({}));
    if let Some(obj) = before_copy.as_object_mut() {
        obj.remove("customDomains");
        obj.remove("serviceDomains");
    }
    if let Some(obj) = after_copy.as_object_mut() {
        obj.remove("customDomains");
        obj.remove("serviceDomains");
    }
    let normalized_before = normalize_for_diff("networking", &before_copy);
    let normalized_after = normalize_for_diff("networking", &after_copy);
    if stable_stringify(&normalized_before) != stable_stringify(&normalized_after) {
        changes.push(update(
            &resource_addr(resource),
            "networking",
            normalized_before,
            normalized_after,
            format!("Update {} networking", resource_name(resource)),
            None,
            "safe",
        ));
    }
}

fn diagnose_unsupported_custom_domains(
    resource: &Value,
    diagnostics: &mut Vec<Diagnostic>,
    existing_domains: Option<&Value>,
) {
    let desired = resource
        .get("networking")
        .and_then(|n| n.get("customDomains"))
        .and_then(Value::as_object);
    let Some(desired) = desired else {
        return;
    };
    let existing = existing_domains.and_then(Value::as_object);
    for domain in desired.keys() {
        if existing.is_some_and(|map| map.contains_key(domain)) {
            continue;
        }
        diagnostics.push(Diagnostic {
            severity: "error".into(),
            path: format!("resources.{}.domains.{domain}", resource_addr(resource)),
            message: format!(
                "Custom-domain registration is not supported by Railway configuration. Add {domain} in the dashboard, then run railway config pull."
            ),
        });
    }
}

fn diff_volume_attachments(previous: &Value, resource: &Value, changes: &mut Vec<Value>) {
    let before = previous.get("volumeAttachments");
    let after = resource.get("volumeAttachments");
    let normalized_before = normalize_for_diff("volumeAttachments", before.unwrap_or(&Value::Null));
    let normalized_after = normalize_for_diff("volumeAttachments", after.unwrap_or(&Value::Null));
    if stable_stringify(&normalized_before) == stable_stringify(&normalized_after) {
        return;
    }
    let destructive = before
        .and_then(Value::as_object)
        .map(|before| {
            let after = after
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            before.keys().any(|key| !after.contains_key(key))
        })
        .unwrap_or(false);
    let details = changed_leaf_paths(&normalized_before, &normalized_after, "volumeAttachments");
    changes.push(update(
        &resource_addr(resource),
        "volumeAttachments",
        before.cloned().unwrap_or(Value::Null),
        after.cloned().unwrap_or(Value::Null),
        summary_for_field(
            resource,
            "volumeAttachments",
            &normalized_before,
            &normalized_after,
        ),
        Some(details),
        if destructive { "destructive" } else { "safe" },
    ));
}

fn diff_top_level_field(
    previous: &Value,
    resource: &Value,
    field_name: &str,
    changes: &mut Vec<Value>,
) {
    let mut before = previous.get(field_name).cloned().unwrap_or(Value::Null);
    let mut after = resource.get(field_name).cloned().unwrap_or(Value::Null);
    if field_name == "source"
        && resource_type(previous) == "database"
        && is_equivalent_database_source(previous, &after)
    {
        return;
    }
    if field_name == "source"
        && resource_type(resource) == "service"
        && !source_supports_auto_updates(resource.get("source"))
    {
        before = without_auto_updates(&before);
        after = without_auto_updates(&after);
    }
    if field_name == "deploy" {
        let stripped = strip_write_only_registry_credentials(&before, &after);
        before = stripped.0;
        after = stripped.1;
    }
    let normalized_before = normalize_for_diff(field_name, &before);
    let normalized_after = normalize_for_diff(field_name, &after);
    if stable_stringify(&normalized_before) == stable_stringify(&normalized_after) {
        return;
    }
    let details = changed_leaf_paths(&normalized_before, &normalized_after, field_name);
    changes.push(update(
        &resource_addr(resource),
        field_name,
        before,
        after,
        summary_for_field(resource, field_name, &normalized_before, &normalized_after),
        Some(details),
        "safe",
    ));
}

fn diff_service_deploy(previous: &Value, resource: &Value, changes: &mut Vec<Value>) {
    let desired_deploy =
        service_deploy_with_current_region(previous.get("deploy"), resource.get("deploy"));
    let switching_away_from_image =
        field_str(previous.get("source").unwrap_or(&Value::Null), "type") == Some("image")
            && field_str(resource.get("source").unwrap_or(&Value::Null), "type") != Some("image");
    let (before, after) = if switching_away_from_image
        && previous
            .get("deploy")
            .and_then(|d| d.get("registryCredentials"))
            .is_some()
    {
        (
            without_registry_credentials(previous.get("deploy").unwrap_or(&Value::Null)),
            {
                let mut copy = desired_deploy.clone();
                if copy.is_null() {
                    copy = json!({});
                }
                copy["registryCredentials"] = Value::Null;
                copy
            },
        )
    } else {
        strip_write_only_registry_credentials(
            previous.get("deploy").unwrap_or(&Value::Null),
            &desired_deploy,
        )
    };
    let normalized_before = normalize_for_diff("deploy", &before);
    let normalized_after = normalize_for_diff("deploy", &after);
    if stable_stringify(&normalized_before) == stable_stringify(&normalized_after) {
        return;
    }
    let details = changed_leaf_paths(&normalized_before, &normalized_after, "deploy");
    changes.push(update(
        &resource_addr(resource),
        "deploy",
        before,
        after,
        summary_for_field(resource, "deploy", &normalized_before, &normalized_after),
        Some(details),
        "safe",
    ));
}

fn service_deploy_with_current_region(previous: Option<&Value>, desired: Option<&Value>) -> Value {
    let desired = desired.cloned().unwrap_or(Value::Null);
    if desired.get("numReplicas").is_none() || desired.get("multiRegionConfig").is_some() {
        return desired;
    }
    let Some(region) = single_deploy_region(previous) else {
        return desired;
    };
    let mut rest = desired.clone();
    let replicas = rest.get("numReplicas").cloned();
    if let Some(obj) = rest.as_object_mut() {
        obj.remove("numReplicas");
        if let Some(replicas) = replicas {
            obj.insert(
                "multiRegionConfig".into(),
                json!({ region: { "numReplicas": replicas } }),
            );
        }
    }
    rest
}

fn single_deploy_region(deploy: Option<&Value>) -> Option<String> {
    let regions = deploy
        .and_then(|d| d.get("multiRegionConfig"))
        .and_then(Value::as_object)?;
    let present: Vec<_> = regions
        .iter()
        .filter(|(_, config)| !config.is_null())
        .collect();
    if present.len() != 1 {
        return None;
    }
    Some(present[0].0.clone())
}

fn diff_volume_config(previous: &Value, resource: &Value, changes: &mut Vec<Value>) {
    let previous_config = drop_unauthored_platform_fields(
        previous.get("config").unwrap_or(&Value::Null),
        resource.get("config").unwrap_or(&Value::Null),
        &["alerts", "allowOnlineResize"],
    );
    let before = normalize_for_diff("config", &previous_config);
    let after = normalize_for_diff("config", resource.get("config").unwrap_or(&Value::Null));
    if stable_stringify(&before) == stable_stringify(&after) {
        return;
    }
    let previous_size = previous_config.get("sizeMB").and_then(Value::as_i64);
    let desired_size = resource
        .get("config")
        .and_then(|c| c.get("sizeMB"))
        .and_then(Value::as_i64);
    let severity = if previous_config.get("region")
        != resource.get("config").and_then(|c| c.get("region"))
        || matches!((previous_size, desired_size), (Some(prev), Some(next)) if next < prev)
    {
        "destructive"
    } else {
        "safe"
    };
    let details = changed_leaf_paths(&before, &after, "config");
    let summary = if let (Some(prev), Some(next)) = (previous_size, desired_size) {
        if next > prev {
            format!(
                "Resize volume {} from {prev}MB to {next}MB",
                resource_name(resource)
            )
        } else {
            summary_for_field(resource, "config", &before, &after)
        }
    } else {
        summary_for_field(resource, "config", &before, &after)
    };
    changes.push(update(
        &resource_addr(resource),
        "config",
        previous_config,
        resource.get("config").cloned().unwrap_or(Value::Null),
        summary,
        Some(details),
        severity,
    ));
}

fn diff_database_deploy(previous: &Value, resource: &Value, changes: &mut Vec<Value>) {
    let (mut previous_deploy, resource_deploy) = strip_write_only_registry_credentials(
        previous.get("deploy").unwrap_or(&Value::Null),
        resource.get("deploy").unwrap_or(&Value::Null),
    );
    previous_deploy = drop_platform_start_command(&previous_deploy, &resource_deploy);
    let previous_region = database_region(previous);
    let desired_region = database_region(resource);
    if desired_region.is_some() && desired_region != previous_region {
        let details = changed_leaf_paths(
            &normalize_database_deploy(&previous_deploy),
            &normalize_database_deploy(&resource_deploy),
            "deploy",
        );
        changes.push(update(
            &resource_addr(resource),
            "deploy",
            previous_deploy,
            resource_deploy,
            format!(
                "Move database {} to {}",
                resource_name(resource),
                desired_region.unwrap()
            ),
            Some(details),
            "destructive",
        ));
        return;
    }
    let before = normalize_database_deploy(&previous_deploy);
    let after = normalize_database_deploy(&resource_deploy);
    if stable_stringify(&before) == stable_stringify(&after) {
        return;
    }
    let details = changed_leaf_paths(&before, &after, "deploy");
    changes.push(update(
        &resource_addr(resource),
        "deploy",
        previous_deploy,
        resource_deploy,
        summary_for_field(resource, "deploy", &before, &after),
        Some(details),
        "safe",
    ));
}

fn drop_unauthored_platform_fields(
    previous_value: &Value,
    desired_value: &Value,
    fields: &[&str],
) -> Value {
    let Some(obj) = previous_value.as_object() else {
        return previous_value.clone();
    };
    let desired = desired_value.as_object().cloned().unwrap_or_default();
    let mut copy = obj.clone();
    let mut changed = false;
    for field_name in fields {
        if copy.contains_key(*field_name) && !desired.contains_key(*field_name) {
            copy.remove(*field_name);
            changed = true;
        } else if copy.contains_key(*field_name)
            && desired.get(*field_name).is_none_or(Value::is_null)
        {
            copy.remove(*field_name);
            changed = true;
        }
    }
    if !changed {
        return previous_value.clone();
    }
    if copy.is_empty() {
        Value::Null
    } else {
        Value::Object(copy)
    }
}

fn drop_platform_start_command(previous_deploy: &Value, resource_deploy: &Value) -> Value {
    drop_unauthored_platform_fields(previous_deploy, resource_deploy, &["startCommand"])
}

fn is_masked_registry_credentials(value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return false;
    };
    if value.is_null() {
        return true;
    }
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.get("username").and_then(Value::as_str) == Some(MASKED_CREDENTIAL_VALUE)
        || obj.get("password").and_then(Value::as_str) == Some(MASKED_CREDENTIAL_VALUE)
}

fn strip_write_only_registry_credentials(before: &Value, after: &Value) -> (Value, Value) {
    let before_credentials = before.get("registryCredentials");
    let after_credentials = after.get("registryCredentials");
    let desired_credentials = real_credential_fields(after_credentials);
    let with_credentials = |value: &Value, credentials: Option<&Value>| {
        if !value.is_object() {
            return value.clone();
        }
        let mut copy = value.as_object().cloned().unwrap();
        match credentials {
            None => {
                copy.remove("registryCredentials");
            }
            Some(credentials) => {
                copy.insert("registryCredentials".into(), credentials.clone());
            }
        }
        if copy.is_empty() {
            Value::Null
        } else {
            Value::Object(copy)
        }
    };
    let desired_side = if after_credentials.is_none() {
        after.clone()
    } else {
        with_credentials(after, desired_credentials.as_ref())
    };
    if desired_credentials.is_some() && is_masked_registry_credentials(after_credentials) {
        return (with_credentials(before, None), desired_side);
    }
    if desired_credentials.is_some() && !is_masked_registry_credentials(before_credentials) {
        return (before.clone(), desired_side);
    }
    let stripped_before = if before_credentials.is_none() {
        before.clone()
    } else {
        with_credentials(before, None)
    };
    let stripped_after = if desired_credentials.is_none() {
        desired_side
    } else {
        with_credentials(&desired_side, None)
    };
    (stripped_before, stripped_after)
}

fn source_supports_auto_updates(source: Option<&Value>) -> bool {
    let Some(source) = source else {
        return false;
    };
    if field_str(source, "type") != Some("image") {
        return false;
    }
    let Some(image) = field_str(source, "image")
        .map(str::trim)
        .map(str::to_lowercase)
    else {
        return false;
    };
    if image.is_empty() {
        return false;
    }
    if !image.contains('/') {
        return true;
    }
    let registry = image.split('/').next().unwrap_or("");
    (!registry.contains('.') && !registry.contains(':') && registry != "localhost")
        || registry == "docker.io"
        || registry == "ghcr.io"
}

fn without_auto_updates(source: &Value) -> Value {
    let Some(obj) = source.as_object() else {
        return source.clone();
    };
    let mut copy = obj.clone();
    copy.remove("autoUpdates");
    if copy.is_empty() {
        Value::Null
    } else {
        Value::Object(copy)
    }
}

fn without_registry_credentials(deploy: &Value) -> Value {
    let Some(obj) = deploy.as_object() else {
        return deploy.clone();
    };
    let mut copy = obj.clone();
    copy.remove("registryCredentials");
    if copy.is_empty() {
        Value::Null
    } else {
        Value::Object(copy)
    }
}

fn real_credential_fields(credentials: Option<&Value>) -> Option<Value> {
    let obj = credentials.and_then(Value::as_object)?;
    let entries: Map<String, Value> = obj
        .iter()
        .filter(|(_, value)| !value.is_null() && value.as_str() != Some(MASKED_CREDENTIAL_VALUE))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if entries.is_empty() {
        None
    } else {
        Some(Value::Object(entries))
    }
}

fn normalize_database_deploy(value: &Value) -> Value {
    let mut normalized = normalize_for_diff("deploy", value);
    if let Some(obj) = normalized.as_object_mut() {
        obj.remove("requiredMountPath");
        if obj.is_empty() {
            return Value::Null;
        }
    }
    normalized
}

fn summary_for_field(resource: &Value, field_name: &str, before: &Value, after: &Value) -> String {
    let details = changed_leaf_paths(before, after, field_name);
    if !details.is_empty() {
        let shown = details
            .iter()
            .take(3)
            .map(|detail| detail_path_for_summary(detail))
            .collect::<Vec<_>>()
            .join(", ");
        let remaining = details.len().saturating_sub(3);
        return format!(
            "Update {} {shown}{}",
            resource_name(resource),
            if remaining > 0 {
                format!(" and {remaining} more")
            } else {
                String::new()
            }
        );
    }
    format!("Update {} {field_name}", resource_name(resource))
}

fn detail_path_for_summary(detail: &str) -> String {
    detail.split(" (").next().unwrap_or(detail).to_string()
}

fn changed_leaf_paths(before: &Value, after: &Value, prefix: &str) -> Vec<String> {
    let before_flat = flatten_for_diff(before, "");
    let after_flat = flatten_for_diff(after, "");
    let mut keys: Vec<_> = before_flat
        .keys()
        .chain(after_flat.keys())
        .cloned()
        .collect();
    keys.sort();
    keys.dedup();
    let changed: Vec<_> = keys
        .into_iter()
        .filter(|key| {
            stable_stringify(before_flat.get(key).unwrap_or(&Value::Null))
                != stable_stringify(after_flat.get(key).unwrap_or(&Value::Null))
        })
        .collect();
    changed
        .iter()
        .filter(|key| key.is_empty() && changed.len() == 1 || !key.is_empty())
        .filter(|key| {
            key.is_empty()
                || !changed
                    .iter()
                    .any(|other| *other != **key && other.starts_with(&format!("{key}.")))
        })
        .map(|key| {
            let path = if key.is_empty() {
                prefix.to_string()
            } else {
                format!("{prefix}.{key}")
            };
            if path.contains("registryCredentials") {
                return format!(
                    "{} ({REDACTED_VARIABLE_VALUE} → {REDACTED_VARIABLE_VALUE})",
                    friendly_path(&path)
                );
            }
            format!(
                "{} ({} → {})",
                friendly_path(&path),
                format_diff_value(before_flat.get(key).unwrap_or(&Value::Null)),
                format_diff_value(after_flat.get(key).unwrap_or(&Value::Null))
            )
        })
        .collect()
}

fn flatten_for_diff(value: &Value, prefix: &str) -> Map<String, Value> {
    match value {
        Value::Object(map) if !map.is_empty() => {
            let mut out = Map::new();
            for (key, child) in map {
                let next = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                out.extend(flatten_for_diff(child, &next));
            }
            out
        }
        other => {
            let mut out = Map::new();
            out.insert(prefix.to_string(), other.clone());
            out
        }
    }
}

fn friendly_path(path: &str) -> String {
    if let Some(caps) = regex::Regex::new(r"^deploy\.multiRegionConfig\.([^.]+)\.numReplicas$")
        .ok()
        .and_then(|re| re.captures(path))
    {
        return format!("regions.{}", &caps[1]);
    }
    path.to_string()
}

fn format_diff_value(value: &Value) -> String {
    if value.is_null() {
        return "null".to_string();
    }
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

fn update(
    address: &str,
    field_name: &str,
    before: Value,
    after: Value,
    summary: String,
    details: Option<Vec<String>>,
    severity: &str,
) -> Value {
    let mut change = json!({
        "kind": "resource.update",
        "address": address,
        "field": field_name,
        "before": before,
        "after": after,
        "path": format!("resources.{address}.{field_name}"),
        "summary": summary,
        "severity": severity,
        "deployEffect": if field_name == "config" || field_name == "groupId" { "none" } else { "deploy" },
    });
    if let Some(details) = details.filter(|details| !details.is_empty()) {
        change["details"] = json!(details);
    }
    change
}

fn format_variable_diff_value(
    value: Option<&Value>,
    resources_by_address: &Map<String, Value>,
    reveal_values: bool,
) -> String {
    let Some(value) = value else {
        return "unset".to_string();
    };
    match field_str(value, "type") {
        Some("preserve") => "preserve()".into(),
        Some("reference") => {
            let resource = field_str(value, "resource").unwrap_or("");
            let name = resources_by_address
                .get(resource)
                .map(resource_name)
                .unwrap_or_else(|| resource.split('.').nth(1).unwrap_or(resource));
            format!("{name}.{}", field_str(value, "output").unwrap_or(""))
        }
        Some("sharedReference") => format!("shared.{}", field_str(value, "name").unwrap_or("")),
        _ if !reveal_values => REDACTED_VARIABLE_VALUE.into(),
        Some("literal") => format_diff_value(value.get("value").unwrap_or(&Value::Null)),
        _ => format_diff_value(&normalize_variable_for_diff(
            Some(value),
            resources_by_address,
        )),
    }
}

fn normalize_variable_for_diff(
    value: Option<&Value>,
    resources_by_address: &Map<String, Value>,
) -> Value {
    let Some(value) = value else {
        return Value::Null;
    };
    if field_str(value, "type") == Some("sharedReference") {
        return json!({
            "type": "literal",
            "value": format!("${{{{shared.{}}}}}", field_str(value, "name").unwrap_or("")),
        });
    }
    if field_str(value, "type") != Some("reference") {
        return value.clone();
    }
    let resource = field_str(value, "resource").unwrap_or("");
    let name = resources_by_address
        .get(resource)
        .map(resource_name)
        .unwrap_or_else(|| resource.split('.').nth(1).unwrap_or(resource));
    json!({
        "type": "literal",
        "value": format!("${{{{{}.{}}}}}", name, field_str(value, "output").unwrap_or("")),
    })
}

fn is_managed_by_repo_config(resource: &Value) -> bool {
    if !matches!(resource_type(resource), "service" | "database") {
        return false;
    }
    field_str(resource, "configFile").is_some_and(|path| {
        regex::Regex::new(r"railway\.(json|toml)$")
            .unwrap()
            .is_match(path)
    })
}

fn bucket_region(resource: &Value) -> Option<String> {
    field(resource, "config")
        .and_then(|config| field_str(config, "region"))
        .map(str::to_string)
}

fn database_region(resource: &Value) -> Option<String> {
    let regions = field(resource, "deploy")
        .and_then(|d| d.get("multiRegionConfig"))
        .and_then(Value::as_object)?;
    let present: Vec<_> = regions
        .iter()
        .filter(|(_, config)| !config.is_null())
        .collect();
    if present.len() != 1 {
        return None;
    }
    Some(present[0].0.clone())
}

fn is_equivalent_database_source(previous: &Value, after: &Value) -> bool {
    if resource_type(previous) != "database" || !after.is_object() {
        return false;
    }
    field_str(after, "type") == Some("image")
        && normalize_image_tag(field_str(after, "image").unwrap_or(""))
            == normalize_image_tag(field_str(previous, "image").unwrap_or(""))
}

fn is_preserved_variable(value: &Value) -> bool {
    field_str(value, "type") == Some("preserve")
}

fn is_unknown_current_variable(value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return false;
    };
    field_str(value, "type") == Some("preserve")
        || (field_str(value, "type") == Some("literal") && field_str(value, "value") == Some(""))
}

fn normalize_for_diff(field_name: &str, value: &Value) -> Value {
    if !value.is_object() {
        return value.clone();
    }
    let mut copy = value.as_object().cloned().unwrap();
    if field_name == "source" {
        if copy.get("checkSuites") == Some(&json!(false)) {
            copy.remove("checkSuites");
        }
        if copy.get("branch") == Some(&json!("main")) {
            copy.remove("branch");
        }
        copy.remove("commitSha");
        copy.remove("upstreamUrl");
        if copy.get("rootDirectory") == Some(&json!("")) {
            copy.remove("rootDirectory");
        }
        if let Some(image) = copy.get("image").and_then(Value::as_str) {
            copy.insert("image".into(), json!(normalize_image_tag(image)));
        }
    }
    if field_name == "build" {
        if copy.get("builder") == Some(&json!("RAILPACK")) {
            copy.remove("builder");
        }
        if copy.get("buildEnvironment") == Some(&json!("V3")) {
            copy.remove("buildEnvironment");
        }
        if copy.get("buildCommand") == Some(&json!("")) {
            copy.remove("buildCommand");
        }
        if copy.get("dockerfilePath") == Some(&json!("Dockerfile")) {
            copy.remove("dockerfilePath");
        }
    }
    if field_name == "deploy" {
        if copy.get("useLegacyStacker") == Some(&json!(false)) {
            copy.remove("useLegacyStacker");
        }
        if copy.get("ipv6EgressEnabled") == Some(&json!(false)) {
            copy.remove("ipv6EgressEnabled");
        }
        if copy.get("runtime") == Some(&json!("V2")) {
            copy.remove("runtime");
        }
        if let Some(multi) = copy.remove("multiRegionConfig") {
            let normalized = normalize_multi_region_config(&multi);
            if !is_default_multi_region_config(&normalized)
                && normalized.as_object().is_some_and(|obj| !obj.is_empty())
            {
                copy.insert("multiRegionConfig".into(), normalized);
            }
        }
    }
    if copy.is_empty() {
        Value::Null
    } else {
        Value::Object(copy)
    }
}

fn normalize_multi_region_config(value: &Value) -> Value {
    let Some(obj) = value.as_object() else {
        return value.clone();
    };
    Value::Object(
        obj.iter()
            .filter(|(_, config)| {
                if !config.is_object() {
                    return true;
                }
                !config.get("numReplicas").is_none_or(Value::is_null)
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}

fn is_default_multi_region_config(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    if obj.len() != 1 {
        return false;
    }
    let Some(config) = obj.values().next().and_then(Value::as_object) else {
        return false;
    };
    config.iter().all(|(key, child)| {
        (key == "numReplicas" && child == &json!(1))
            || (key == "stackerAssignment" && child.is_null())
    })
}

fn normalize_image_tag(image: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"^(?:railwayapp/|ghcr\.io/railwayapp-templates/)?(redis|mysql|mongo|postgres)(?:-ssl)?:(\d+)(?:\.\d+)*$")
            .unwrap()
    });
    if let Some(caps) = re.captures(image) {
        format!("{}:{}", &caps[1], &caps[2])
    } else {
        image.to_string()
    }
}
