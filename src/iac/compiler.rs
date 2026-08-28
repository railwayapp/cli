use serde_json::{Map, Value, json};

use super::graph::{
    Edge, EnvironmentNode, ProjectNode, RAILWAY_GRAPH_VERSION, RailwayGraph, resource_addr,
    resource_address, resource_name, resource_type,
};
use super::json::{field, field_str, prune_empty};

#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    pub service_ids_by_name: Map<String, Value>,
    pub existing_service_ids: Vec<String>,
    pub volume_ids_by_service_name: Map<String, Value>,
    pub volume_ids_by_name: Map<String, Value>,
    pub bucket_ids_by_name: Map<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct EnvironmentConfigToGraphOptions {
    pub project_name: Option<String>,
    pub service_names_by_id: Map<String, Value>,
    pub volume_names_by_id: Map<String, Value>,
    pub volume_group_ids_by_id: Map<String, Value>,
    pub bucket_names_by_id: Map<String, Value>,
    pub bucket_group_ids_by_id: Map<String, Value>,
    pub custom_domains_by_service_id: Map<String, Value>,
}

pub fn project_definition_to_graph(definition: &Value) -> RailwayGraph {
    let mut resources = flatten_resources(
        definition
            .get("resources")
            .or_else(|| definition.get("services")),
    );
    let mut seen: std::collections::HashSet<String> = resources.iter().map(resource_addr).collect();

    for resource in resources.clone() {
        if !matches!(resource_type(&resource), "service" | "database") {
            continue;
        }
        let Some(attachments) = field(&resource, "volumeAttachments").and_then(Value::as_object)
        else {
            continue;
        };
        for attachment in attachments.values() {
            let Some(volume) = field_str(attachment, "volume") else {
                continue;
            };
            if seen.contains(volume) {
                continue;
            }
            let volume_name = volume.split('.').skip(1).collect::<Vec<_>>().join(".");
            let mut node = json!({
                "address": volume,
                "type": "volume",
                "name": volume_name,
            });
            if let Some(config) = attachment.get("volumeConfig") {
                node["config"] = config.clone();
            }
            seen.insert(volume.to_string());
            resources.push(node);
        }
    }

    let mut edges = Vec::new();
    for resource in &resources {
        if !matches!(resource_type(resource), "service" | "database") {
            continue;
        }
        let from = resource_addr(resource);
        if let Some(attachments) = field(resource, "volumeAttachments").and_then(Value::as_object) {
            for attachment in attachments.values() {
                if let (Some(volume), Some(mount)) = (
                    field_str(attachment, "volume"),
                    field_str(attachment, "mountPath"),
                ) {
                    edges.push(Edge {
                        from: from.clone(),
                        to: volume.to_string(),
                        kind: "mount".to_string(),
                        key: Some(mount.to_string()),
                    });
                }
            }
        }
        if let Some(variables) = field(resource, "variables").and_then(Value::as_object) {
            for (key, value) in variables {
                if field_str(value, "type") == Some("reference") {
                    if let Some(target) = field_str(value, "resource") {
                        edges.push(Edge {
                            from: from.clone(),
                            to: target.to_string(),
                            kind: "variable".to_string(),
                            key: Some(key.clone()),
                        });
                    }
                }
            }
        }
    }

    let resources = resources
        .into_iter()
        .map(strip_runtime_helpers)
        .map(drop_attachment_volume_config)
        .collect();

    RailwayGraph {
        version: RAILWAY_GRAPH_VERSION,
        project: ProjectNode {
            name: field_str(definition, "name")
                .unwrap_or("imported-project")
                .to_string(),
        },
        environments: definition
            .get("environments")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(|name| {
                        name.as_str().map(|name| EnvironmentNode {
                            name: name.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        resources,
        edges,
    }
}

fn flatten_resources(value: Option<&Value>) -> Vec<Value> {
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        Value::Array(items) => items
            .iter()
            .flat_map(|item| flatten_resources(Some(item)))
            .collect(),
        other => vec![other.clone()],
    }
}

fn drop_attachment_volume_config(mut resource: Value) -> Value {
    let Some(attachments) = resource
        .get_mut("volumeAttachments")
        .and_then(Value::as_object_mut)
    else {
        return resource;
    };
    for attachment in attachments.values_mut() {
        if let Some(obj) = attachment.as_object_mut() {
            obj.remove("volumeConfig");
        }
    }
    resource
}

fn strip_runtime_helpers(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(strip_runtime_helpers).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, child)| (key, strip_runtime_helpers(child)))
                .collect(),
        ),
        other => other,
    }
}

pub fn graph_to_environment_config(graph: &RailwayGraph, options: &CompileOptions) -> Value {
    let mut config = json!({ "services": {} });
    let names_by_id: Map<String, Value> = graph
        .resources
        .iter()
        .map(|resource| (resource_addr(resource), json!(resource_name(resource))))
        .collect();
    let existing: std::collections::HashSet<String> =
        options.existing_service_ids.iter().cloned().collect();

    for resource in &graph.resources {
        match resource_type(resource) {
            "service" | "database" => {
                let name = resource_name(resource);
                let service_key = options
                    .service_ids_by_name
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or(name)
                    .to_string();
                let is_new = !existing.contains(&service_key);
                let service = if resource_type(resource) == "database" {
                    let volume_id = options
                        .volume_ids_by_service_name
                        .get(name)
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    database_to_environment_config(resource, is_new, volume_id.as_deref())
                } else {
                    service_to_environment_config(
                        resource,
                        &names_by_id,
                        is_new,
                        &options.volume_ids_by_name,
                    )
                };
                config["services"][service_key.clone()] = service;

                if resource_type(resource) == "database" {
                    if let Some(volume_id) = options
                        .volume_ids_by_service_name
                        .get(name)
                        .and_then(Value::as_str)
                    {
                        let mut volume = json!({ "isCreated": true });
                        if let Some(region) = database_region(resource) {
                            volume["region"] = json!(region);
                        }
                        config
                            .as_object_mut()
                            .unwrap()
                            .entry("volumes")
                            .or_insert_with(|| json!({}))[volume_id] = volume;
                    }
                }
            }
            "volume" => {
                let volumes = config
                    .as_object_mut()
                    .unwrap()
                    .entry("volumes")
                    .or_insert_with(|| json!({}));
                let existing_id = options
                    .volume_ids_by_name
                    .get(resource_name(resource))
                    .and_then(Value::as_str);
                let key = existing_id.unwrap_or_else(|| resource_name(resource));
                let mut volume = resource.get("config").cloned().unwrap_or_else(|| json!({}));
                if existing_id.is_none() {
                    if let Some(obj) = volume.as_object_mut() {
                        obj.insert("isCreated".to_string(), json!(true));
                    }
                }
                volumes[key] = volume;
            }
            "bucket" => {
                let buckets = config
                    .as_object_mut()
                    .unwrap()
                    .entry("buckets")
                    .or_insert_with(|| json!({}));
                let existing_id = options
                    .bucket_ids_by_name
                    .get(resource_name(resource))
                    .and_then(Value::as_str);
                let key = existing_id.unwrap_or_else(|| resource_name(resource));
                let mut bucket = resource.get("config").cloned().unwrap_or_else(|| json!({}));
                if existing_id.is_none() {
                    if let Some(obj) = bucket.as_object_mut() {
                        obj.insert("isCreated".to_string(), json!(true));
                    }
                }
                if let Some(group_id) = field_str(resource, "groupId") {
                    bucket["groupId"] = json!(group_id);
                }
                buckets[key] = bucket;
            }
            "group" => {
                let groups = config
                    .as_object_mut()
                    .unwrap()
                    .entry("groups")
                    .or_insert_with(|| json!({}));
                groups[resource_name(resource)] = prune_empty(json!({
                    "isCreated": true,
                    "name": resource_name(resource),
                    "color": resource.get("color").cloned().unwrap_or(Value::Null),
                    "icon": resource.get("icon").cloned().unwrap_or(Value::Null),
                    "isCollapsed": resource.get("isCollapsed").cloned().unwrap_or(Value::Null),
                }));
            }
            _ => {}
        }
    }

    prune_empty(config)
}

fn database_region(database: &Value) -> Option<String> {
    let regions = field(database, "deploy")
        .and_then(|deploy| deploy.get("multiRegionConfig"))
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

fn database_deploy(database: &Value, required_mount_path: &str) -> Value {
    let mut deploy = json!({ "requiredMountPath": required_mount_path });
    if let Some(multi) =
        field(database, "deploy").and_then(|deploy| deploy.get("multiRegionConfig"))
    {
        deploy["multiRegionConfig"] = multi.clone();
    }
    deploy
}

fn database_to_environment_config(
    database: &Value,
    is_new: bool,
    volume_id: Option<&str>,
) -> Value {
    let engine = field_str(database, "engine").unwrap_or("postgres");
    let image = field_str(database, "image").unwrap_or("postgres:16");
    if engine != "postgres" {
        let mut config = json!({
            "source": { "image": image },
        });
        if is_new {
            config["isCreated"] = json!(true);
        }
        if let Some(mount) = field_str(database, "defaultMountPath") {
            config["deploy"] = database_deploy(database, mount);
            if let Some(volume_id) = volume_id {
                config["volumeMounts"] = json!({ volume_id: { "mountPath": mount } });
            }
        }
        return prune_empty(config);
    }

    let mut config = json!({
        "source": { "image": image },
        "deploy": database_deploy(database, "/var/lib/postgresql/data"),
        "variables": {
            "PGDATA": { "value": "/var/lib/postgresql/data/pgdata" },
            "PGHOST": { "value": "${{RAILWAY_PRIVATE_DOMAIN}}" },
            "PGPORT": { "value": "5432" },
            "PGUSER": { "value": "${{POSTGRES_USER}}" },
            "PGDATABASE": { "value": "${{POSTGRES_DB}}" },
            "PGPASSWORD": { "value": "${{POSTGRES_PASSWORD}}" },
            "POSTGRES_DB": { "value": "railway" },
            "DATABASE_URL": { "value": "postgresql://${{PGUSER}}:${{POSTGRES_PASSWORD}}@${{RAILWAY_PRIVATE_DOMAIN}}:5432/${{PGDATABASE}}" },
            "POSTGRES_USER": { "value": "postgres" },
            "SSL_CERT_DAYS": { "value": "820" },
            "POSTGRES_PASSWORD": { "generator": "secret(32, \"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\")" },
            "DATABASE_PUBLIC_URL": { "value": "postgresql://${{PGUSER}}:${{POSTGRES_PASSWORD}}@${{RAILWAY_TCP_PROXY_DOMAIN}}:${{RAILWAY_TCP_PROXY_PORT}}/${{PGDATABASE}}" },
            "RAILWAY_DEPLOYMENT_DRAINING_SECONDS": { "value": "60" },
        },
        "networking": { "tcpProxies": { "5432": {} } },
    });
    if is_new {
        config["isCreated"] = json!(true);
    }
    if let Some(volume_id) = volume_id {
        config["volumeMounts"] = json!({ volume_id: { "mountPath": "/var/lib/postgresql/data" } });
    }
    prune_empty(config)
}

fn service_to_environment_config(
    service: &Value,
    resource_names_by_id: &Map<String, Value>,
    is_new: bool,
    volume_ids_by_name: &Map<String, Value>,
) -> Value {
    let mut config = if is_new {
        json!({ "isCreated": true })
    } else {
        json!({})
    };
    if let Some(source) = field(service, "source") {
        let source_type = field_str(source, "type");
        let mut out = json!({});
        if source_type == Some("github") {
            if let Some(repo) = source.get("repo") {
                out["repo"] = repo.clone();
            }
            if let Some(branch) = source.get("branch") {
                out["branch"] = branch.clone();
            }
        }
        if source_type == Some("image") {
            if let Some(image) = source.get("image") {
                out["image"] = image.clone();
            }
        }
        for key in [
            "rootDirectory",
            "commitSha",
            "upstreamUrl",
            "checkSuites",
            "autoUpdates",
        ] {
            if let Some(value) = source.get(key) {
                out[key] = value.clone();
            }
        }
        let out = prune_empty(out);
        if out.as_object().is_some_and(|obj| !obj.is_empty()) {
            config["source"] = out;
        }
    }
    if let Some(build) = field(service, "build") {
        config["build"] = build.clone();
    }
    if let Some(deploy) = field(service, "deploy") {
        config["deploy"] = deploy.clone();
    }
    if let Some(variables) = field(service, "variables") {
        config["variables"] = variables_to_environment_config(variables, resource_names_by_id);
    }
    if let Some(networking) = field(service, "networking") {
        config["networking"] = networking.clone();
    }
    let mut mounts = Map::new();
    if let Some(attachments) = field(service, "volumeAttachments").and_then(Value::as_object) {
        for attachment in attachments.values() {
            let Some(volume) = field_str(attachment, "volume") else {
                continue;
            };
            let volume_name = resource_names_by_id
                .get(volume)
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| volume.split('.').skip(1).collect::<Vec<_>>().join("."));
            let volume_id = volume_ids_by_name
                .get(&volume_name)
                .and_then(Value::as_str)
                .unwrap_or(&volume_name);
            let mut mount = json!({ "mountPath": attachment.get("mountPath") });
            if let Some(schedules) = attachment.get("backupSchedules") {
                mount["backupSchedules"] = schedules.clone();
            }
            mounts.insert(volume_id.to_string(), prune_empty(mount));
        }
    }
    if let Some(existing) = field(service, "volumeMounts").and_then(Value::as_object) {
        for (key, value) in existing {
            mounts.insert(key.clone(), value.clone());
        }
    }
    if !mounts.is_empty() {
        config["volumeMounts"] = Value::Object(mounts);
    }
    for key in [
        "configFile",
        "parentServiceId",
        "groupId",
        "clusterRole",
        "replicaConfig",
        "clusterDisplay",
    ] {
        if let Some(value) = field(service, key) {
            config[key] = value.clone();
        }
    }
    prune_empty(config)
}

fn variables_to_environment_config(
    variables: &Value,
    resource_names_by_id: &Map<String, Value>,
) -> Value {
    let Some(map) = variables.as_object() else {
        return json!({});
    };
    let mut out = Map::new();
    for (key, value) in map {
        if field_str(value, "type") == Some("preserve") {
            continue;
        }
        out.insert(key.clone(), variable_to_config(value, resource_names_by_id));
    }
    Value::Object(out)
}

fn variable_to_config(value: &Value, resource_names_by_id: &Map<String, Value>) -> Value {
    match field_str(value, "type") {
        Some("literal") => {
            let mut copy = value.clone();
            if let Some(obj) = copy.as_object_mut() {
                obj.remove("type");
            }
            copy
        }
        Some("raw") => value.get("value").cloned().unwrap_or(json!({})),
        Some("sharedReference") => {
            let name = field_str(value, "name").unwrap_or("");
            json!({ "value": format!("${{{{shared.{name}}}}}") })
        }
        Some("reference") => {
            let resource = field_str(value, "resource").unwrap_or("");
            let name = resource_names_by_id
                .get(resource)
                .and_then(Value::as_str)
                .unwrap_or(resource);
            let output = field_str(value, "output").unwrap_or("");
            json!({ "value": format!("${{{{{name}.{output}}}}}") })
        }
        _ => value.clone(),
    }
}

pub fn environment_config_to_graph(
    config: &Value,
    options: &EnvironmentConfigToGraphOptions,
) -> RailwayGraph {
    let mut resources = Vec::new();
    let groups = config
        .get("groups")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let group_names_by_id: Map<String, Value> = groups
        .iter()
        .map(|(id, group)| {
            (
                id.clone(),
                json!(group.get("name").and_then(Value::as_str).unwrap_or(id)),
            )
        })
        .collect();

    let mut referenced = std::collections::HashSet::new();
    if let Some(services) = config.get("services").and_then(Value::as_object) {
        for service in services.values() {
            if let Some(group_id) = field_str(service, "groupId") {
                referenced.insert(group_id.to_string());
            }
        }
    }
    for group_id in options
        .volume_group_ids_by_id
        .values()
        .filter_map(Value::as_str)
    {
        referenced.insert(group_id.to_string());
    }
    if let Some(buckets) = config.get("buckets").and_then(Value::as_object) {
        for (bucket_id, bucket) in buckets {
            let group_id = options
                .bucket_group_ids_by_id
                .get(bucket_id)
                .and_then(Value::as_str)
                .or_else(|| field_str(bucket, "groupId"));
            if let Some(group_id) = group_id {
                referenced.insert(group_id.to_string());
            }
        }
    }
    for group_id in referenced.clone() {
        let mut parent = groups
            .get(&group_id)
            .and_then(|group| field_str(group, "groupId"))
            .map(str::to_string);
        while let Some(parent_id) = parent {
            if referenced.contains(&parent_id) {
                break;
            }
            referenced.insert(parent_id.clone());
            parent = groups
                .get(&parent_id)
                .and_then(|group| field_str(group, "groupId"))
                .map(str::to_string);
        }
    }

    for (group_id, group) in &groups {
        if group.get("isDeleted").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if !referenced.contains(group_id) {
            continue;
        }
        let name = group
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(group_id);
        let mut node = prune_empty(json!({
            "address": resource_address("group", name),
            "type": "group",
            "name": name,
            "color": group.get("color").cloned().unwrap_or(Value::Null),
            "icon": group.get("icon").cloned().unwrap_or(Value::Null),
            "isCollapsed": group.get("isCollapsed").cloned().unwrap_or(Value::Null),
        }));
        if let Some(parent) = field_str(group, "groupId") {
            let parent_name = group_names_by_id
                .get(parent)
                .and_then(Value::as_str)
                .unwrap_or(parent);
            node["groupId"] = json!(parent_name);
        }
        resources.push(node);
    }

    if let Some(services) = config.get("services").and_then(Value::as_object) {
        for (service_id, service) in services {
            if service.is_null() || service.get("isDeleted").and_then(Value::as_bool) == Some(true)
            {
                continue;
            }
            let name = options
                .service_names_by_id
                .get(service_id)
                .and_then(Value::as_str)
                .unwrap_or(service_id);
            let image_name = service
                .get("source")
                .and_then(|source| field_str(source, "image"));
            let looks_like_database = image_name.is_some_and(|image| {
                image.contains("postgres")
                    || image.contains("mysql")
                    || image.contains("redis")
                    || image.contains("mongo")
            });
            if looks_like_database {
                let image = image_name.unwrap_or("postgres:16");
                let engine = if image.contains("mysql") {
                    "mysql"
                } else if image.contains("redis") {
                    "redis"
                } else if image.contains("mongo") {
                    "mongo"
                } else {
                    "postgres"
                };
                let output = match engine {
                    "redis" => "REDIS_URL",
                    "mysql" => "MYSQL_URL",
                    "mongo" => "MONGO_URL",
                    _ => "DATABASE_URL",
                };
                let mut node = prune_empty(json!({
                    "address": resource_address("database", name),
                    "type": "database",
                    "kind": "database",
                    "engine": engine,
                    "name": name,
                    "image": image,
                    "output": output,
                    "defaultMountPath": if service.get("volumeMounts").and_then(Value::as_object).is_some_and(|m| !m.is_empty()) {
                        service.get("deploy").and_then(|d| d.get("requiredMountPath")).cloned().unwrap_or(Value::Null)
                    } else {
                        Value::Null
                    },
                    "deploy": service.get("deploy").cloned().unwrap_or(Value::Null),
                    "volumeMounts": service.get("volumeMounts").cloned().unwrap_or(Value::Null),
                }));
                if let Some(group_id) = field_str(service, "groupId") {
                    node["groupId"] = json!(
                        group_names_by_id
                            .get(group_id)
                            .and_then(Value::as_str)
                            .unwrap_or(group_id)
                    );
                }
                resources.push(node);
                continue;
            }

            let kind = if service.get("source").and_then(|s| s.get("repo")).is_some() {
                "github"
            } else if service.get("source").and_then(|s| s.get("image")).is_some() {
                "docker-image"
            } else if service
                .get("deploy")
                .and_then(|d| d.get("cronSchedule"))
                .is_some()
            {
                "function"
            } else {
                "empty"
            };
            let mut node = json!({
                "address": resource_address("service", name),
                "type": "service",
                "kind": kind,
                "name": name,
            });
            if let Some(source) = service.get("source") {
                let mut src = source.clone();
                if let Some(obj) = src.as_object_mut() {
                    obj.insert(
                        "type".to_string(),
                        json!(if source.get("image").is_some() {
                            "image"
                        } else {
                            "github"
                        }),
                    );
                }
                node["source"] = src;
            }
            if let Some(build) = service.get("build") {
                node["build"] = build.clone();
            }
            if let Some(deploy) = service.get("deploy") {
                node["deploy"] = deploy.clone();
            }
            if let Some(variables) = service.get("variables") {
                node["variables"] = variables_from_environment_config(variables);
            }
            let mut networking = service.get("networking").cloned().unwrap_or(json!({}));
            if let Some(domains) = options.custom_domains_by_service_id.get(service_id) {
                networking["customDomains"] = domains.clone();
            } else if let Some(existing) = service
                .get("networking")
                .and_then(|n| n.get("customDomains"))
            {
                networking["customDomains"] = existing.clone();
            }
            let networking = prune_empty(networking);
            if networking.as_object().is_some_and(|obj| !obj.is_empty())
                || options
                    .custom_domains_by_service_id
                    .contains_key(service_id)
            {
                node["networking"] = networking;
            }
            let attachments = volume_attachments_from_environment_config(
                service.get("volumeMounts"),
                &options.volume_names_by_id,
            );
            if let Some(attachments) = attachments {
                node["volumeAttachments"] = attachments;
            }
            if let Some(config_file) = service.get("configFile") {
                node["configFile"] = config_file.clone();
            }
            if let Some(group_id) = field_str(service, "groupId") {
                node["groupId"] = json!(
                    group_names_by_id
                        .get(group_id)
                        .and_then(Value::as_str)
                        .unwrap_or(group_id)
                );
            }
            resources.push(node);
        }
    }

    if let Some(volumes) = config.get("volumes").and_then(Value::as_object) {
        for (volume_id, volume) in volumes {
            if volume.is_null() || volume.get("isDeleted").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let name = options
                .volume_names_by_id
                .get(volume_id)
                .and_then(Value::as_str)
                .unwrap_or(volume_id);
            let mut node = json!({
                "address": resource_address("volume", name),
                "type": "volume",
                "name": name,
                "config": volume,
            });
            if let Some(group_id) = options
                .volume_group_ids_by_id
                .get(volume_id)
                .and_then(Value::as_str)
            {
                node["groupId"] = json!(
                    group_names_by_id
                        .get(group_id)
                        .and_then(Value::as_str)
                        .unwrap_or(group_id)
                );
            }
            resources.push(node);
        }
    }

    if let Some(buckets) = config.get("buckets").and_then(Value::as_object) {
        for (bucket_id, bucket) in buckets {
            if bucket.is_null() || bucket.get("isDeleted").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let name = options
                .bucket_names_by_id
                .get(bucket_id)
                .and_then(Value::as_str)
                .unwrap_or(bucket_id);
            let mut node = json!({
                "address": resource_address("bucket", name),
                "type": "bucket",
                "name": name,
                "config": bucket,
            });
            let group_id = options
                .bucket_group_ids_by_id
                .get(bucket_id)
                .and_then(Value::as_str)
                .or_else(|| field_str(bucket, "groupId"));
            if let Some(group_id) = group_id {
                node["groupId"] = json!(
                    group_names_by_id
                        .get(group_id)
                        .and_then(Value::as_str)
                        .unwrap_or(group_id)
                );
            }
            resources.push(node);
        }
    }

    project_definition_to_graph(&json!({
        "name": options.project_name.clone().unwrap_or_else(|| "imported-project".to_string()),
        "resources": resources,
    }))
}

fn variables_from_environment_config(variables: &Value) -> Value {
    let Some(map) = variables.as_object() else {
        return json!({});
    };
    let mut out = Map::new();
    for (key, value) in map {
        if value.is_null() {
            continue;
        }
        let sealed = value.get("isSealed").and_then(Value::as_bool) == Some(true);
        let literal = value.get("value").and_then(Value::as_str);
        let masked = literal.is_none() || literal == Some("") || literal == Some("*****") || sealed;
        out.insert(
            key.clone(),
            if masked {
                json!({ "type": "preserve" })
            } else {
                json!({ "type": "literal", "value": literal })
            },
        );
    }
    Value::Object(out)
}

fn volume_attachments_from_environment_config(
    volume_mounts: Option<&Value>,
    volume_names_by_id: &Map<String, Value>,
) -> Option<Value> {
    let mounts = volume_mounts.and_then(Value::as_object)?;
    let mut attachments = Map::new();
    for (volume_id, mount) in mounts {
        let Some(path) = field_str(mount, "mountPath") else {
            continue;
        };
        let name = volume_names_by_id
            .get(volume_id)
            .and_then(Value::as_str)
            .unwrap_or(volume_id);
        let mut attachment = json!({
            "volume": resource_address("volume", name),
            "mountPath": path,
        });
        if let Some(schedules) = mount.get("backupSchedules") {
            attachment["backupSchedules"] = schedules.clone();
        }
        attachments.insert(name.to_string(), attachment);
    }
    if attachments.is_empty() {
        None
    } else {
        Some(Value::Object(attachments))
    }
}

pub fn compose_patch(current_config: &Value, desired_config: &Value) -> Value {
    prune_empty(add_deletion_markers(current_config, desired_config))
}

fn add_deletion_markers(current_config: &Value, desired_config: &Value) -> Value {
    let mut next = desired_config.clone();
    if let Some(volumes) = current_config.get("volumes").and_then(Value::as_object) {
        for (volume_id, current_volume) in volumes {
            if current_volume.is_null()
                || current_volume.get("isDeleted").and_then(Value::as_bool) == Some(true)
            {
                continue;
            }
            if desired_config
                .get("volumes")
                .and_then(|volumes| volumes.get(volume_id))
                .is_some()
            {
                continue;
            }
            next.as_object_mut()
                .unwrap()
                .entry("volumes")
                .or_insert_with(|| json!({}))[volume_id.clone()] = json!({ "isDeleted": true });
        }
    }
    if let Some(services) = current_config.get("services").and_then(Value::as_object) {
        for (service_id, current_service) in services {
            if current_service.is_null()
                || current_service.get("isDeleted").and_then(Value::as_bool) == Some(true)
            {
                continue;
            }
            let desired_service = desired_config
                .get("services")
                .and_then(|s| s.get(service_id));
            if desired_service.is_none() {
                next.as_object_mut()
                    .unwrap()
                    .entry("services")
                    .or_insert_with(|| json!({}))[service_id.clone()] =
                    json!({ "isDeleted": true });
                continue;
            }
            if let Some(vars) = current_service.get("variables").and_then(Value::as_object) {
                for (variable_name, current_variable) in vars {
                    if current_variable.is_null() {
                        continue;
                    }
                    if desired_service
                        .and_then(|s| s.get("variables"))
                        .and_then(|v| v.get(variable_name))
                        .is_some()
                    {
                        continue;
                    }
                    let services = next
                        .as_object_mut()
                        .unwrap()
                        .entry("services")
                        .or_insert_with(|| json!({}));
                    let service = services
                        .as_object_mut()
                        .unwrap()
                        .entry(service_id.clone())
                        .or_insert_with(|| json!({}));
                    service
                        .as_object_mut()
                        .unwrap()
                        .entry("variables")
                        .or_insert_with(|| json!({}))[variable_name.clone()] = Value::Null;
                }
            }
        }
    }
    next
}

pub fn map_from_str(map: &[(&str, &str)]) -> Map<String, Value> {
    map.iter()
        .map(|(k, v)| ((*k).to_string(), json!(*v)))
        .collect()
}
