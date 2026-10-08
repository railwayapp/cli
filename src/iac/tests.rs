use serde_json::{Value, json};

use super::change_set::{DiffOptions, RAILWAY_CHANGE_SET_VERSION, diff_graphs, render_change_set};
use super::compiler::{
    CompileOptions, EnvironmentConfigToGraphOptions, IAC_PROJECT_FIELDS, IAC_RESOURCE_FIELDS,
    environment_config_to_graph, graph_to_environment_config, project_definition_to_graph,
    unknown_field_diagnostics,
};
use super::eval::{
    EvalContext, IAC_FEATURES, PrContext, evaluate_file, evaluate_file_with_context,
};
use super::graph::RAILWAY_GRAPH_VERSION;
use super::partial::IacPartials;

fn graph_from(resources: Vec<Value>) -> super::graph::RailwayGraph {
    project_definition_to_graph(&json!({ "name": "app", "resources": resources }))
}

fn service(name: &str, extra: Value) -> Value {
    let mut node = json!({
        "address": format!("service.{name}"),
        "type": "service",
        "kind": "empty",
        "name": name,
    });
    if let Some(obj) = extra.as_object() {
        for (key, value) in obj {
            node[key] = value.clone();
        }
    }
    if node
        .get("source")
        .and_then(|s| s.get("type"))
        .and_then(Value::as_str)
        == Some("github")
    {
        node["kind"] = json!("github");
    }
    if node
        .get("source")
        .and_then(|s| s.get("type"))
        .and_then(Value::as_str)
        == Some("image")
    {
        node["kind"] = json!("docker-image");
    }
    node
}

fn github(repo: &str) -> Value {
    json!({ "type": "github", "repo": repo, "branch": "main" })
}

fn image(name: &str) -> Value {
    json!({ "type": "image", "image": name })
}

fn postgres(name: &str, region: Option<&str>) -> Value {
    let mut node = json!({
        "address": format!("database.{name}"),
        "type": "database",
        "kind": "database",
        "engine": "postgres",
        "name": name,
        "image": "ghcr.io/railwayapp-templates/postgres-ssl:18",
        "output": "DATABASE_URL",
        "defaultMountPath": "/var/lib/postgresql/data",
        "source": image("ghcr.io/railwayapp-templates/postgres-ssl:18"),
    });
    if let Some(region) = region {
        node["deploy"] = json!({ "multiRegionConfig": { region: { "numReplicas": 1 } } });
    }
    node
}

fn redis(name: &str) -> Value {
    json!({
        "address": format!("database.{name}"),
        "type": "database",
        "kind": "database",
        "engine": "redis",
        "name": name,
        "image": "railwayapp/redis:8.2",
        "output": "REDIS_URL",
        "defaultMountPath": "/bitnami",
        "source": image("railwayapp/redis:8.2"),
    })
}

fn volume(name: &str, config: Value) -> Value {
    json!({
        "address": format!("volume.{name}"),
        "type": "volume",
        "name": name,
        "config": config,
    })
}

fn bucket(name: &str, region: &str) -> Value {
    json!({
        "address": format!("bucket.{name}"),
        "type": "bucket",
        "name": name,
        "config": { "region": region },
    })
}

fn env_config(config: Value) -> super::graph::RailwayGraph {
    environment_config_to_graph(
        &config,
        &EnvironmentConfigToGraphOptions {
            project_name: Some("app".into()),
            ..Default::default()
        },
    )
}

/// Import `config` with every service keyed by name and marked as deployed
/// from a template, which is how Railway-managed databases show up.
fn managed_db_config(config: Value) -> super::graph::RailwayGraph {
    let ids: Vec<&str> = config["services"]
        .as_object()
        .map(|services| services.keys().map(String::as_str).collect())
        .unwrap_or_default();
    environment_config_to_graph(
        &config,
        &EnvironmentConfigToGraphOptions {
            project_name: Some("app".into()),
            template_service_ids_by_id: super::compiler::map_from_str(
                &ids.iter().map(|id| (*id, "tpl")).collect::<Vec<_>>(),
            ),
            ..Default::default()
        },
    )
}

fn diff(
    current: &super::graph::RailwayGraph,
    desired: &super::graph::RailwayGraph,
) -> super::change_set::ChangeSet {
    diff_graphs(DiffOptions {
        current,
        desired,
        reveal_values: false,
        partial: None,
        owners: None,
    })
}

fn kinds(change_set: &super::change_set::ChangeSet) -> Vec<String> {
    change_set
        .changes
        .iter()
        .map(|change| {
            change
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

#[test]
fn emits_change_set_wire_version() {
    let current = graph_from(vec![]);
    let desired = graph_from(vec![service("web", json!({}))]);
    assert_eq!(RAILWAY_CHANGE_SET_VERSION, 1);
    assert_eq!(diff(&current, &desired).version, 1);
    assert_eq!(RAILWAY_GRAPH_VERSION, 1);
}

#[test]
fn numeric_replicas_are_count_only() {
    let graph = graph_from(vec![service(
        "web",
        json!({ "deploy": { "numReplicas": 1 } }),
    )]);
    let web = graph
        .resources
        .iter()
        .find(|r| r["address"] == "service.web")
        .unwrap();
    assert_eq!(web["deploy"], json!({ "numReplicas": 1 }));
    let config = graph_to_environment_config(&graph, &CompileOptions::default());
    assert_eq!(
        config["services"]["web"]["deploy"],
        json!({ "numReplicas": 1 })
    );
}

#[test]
fn count_only_replica_changes_keep_current_region() {
    let current = graph_from(vec![service(
        "web",
        json!({ "deploy": { "multiRegionConfig": { "us-east4": { "numReplicas": 1 } } } }),
    )]);
    let desired = graph_from(vec![service(
        "web",
        json!({ "deploy": { "numReplicas": 2 } }),
    )]);
    let change_set = diff(&current, &desired);
    assert_eq!(change_set.changes.len(), 1);
    assert_eq!(change_set.changes[0]["kind"], "resource.update");
    assert_eq!(change_set.changes[0]["field"], "deploy");
}

#[test]
fn dockerfile_build_defaults_do_not_drift() {
    let current = graph_from(vec![service(
        "backend",
        json!({ "build": { "builder": "DOCKERFILE", "dockerfilePath": "Dockerfile", "buildCommand": "" } }),
    )]);
    let desired = graph_from(vec![service(
        "backend",
        json!({ "build": { "builder": "DOCKERFILE" } }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn volume_upsize_is_safe_downsize_is_destructive() {
    let small = graph_from(vec![volume("data", json!({ "sizeMB": 1024 }))]);
    let large = graph_from(vec![volume("data", json!({ "sizeMB": 2048 }))]);
    assert_eq!(diff(&small, &large).changes[0]["severity"], "safe");
    assert_eq!(diff(&large, &small).changes[0]["severity"], "destructive");
}

#[test]
fn shared_variable_references_compile() {
    let graph = graph_from(vec![service(
        "web",
        json!({
            "variables": {
                "API_KEY": { "type": "sharedReference", "name": "API_KEY" },
                "DASHED": { "type": "sharedReference", "name": "DASHED-KEY" }
            }
        }),
    )]);
    let config = graph_to_environment_config(&graph, &CompileOptions::default());
    assert_eq!(
        config["services"]["web"]["variables"],
        json!({
            "API_KEY": { "value": "${{shared.API_KEY}}" },
            "DASHED": { "value": "${{shared.DASHED-KEY}}" }
        })
    );
}

#[test]
fn database_region_maps_to_service_and_volume() {
    let graph = graph_from(vec![postgres("db", Some("europe-west4"))]);
    let config = graph_to_environment_config(
        &graph,
        &CompileOptions {
            service_ids_by_name: super::compiler::map_from_str(&[("db", "service-id")]),
            volume_ids_by_service_name: super::compiler::map_from_str(&[("db", "volume-id")]),
            existing_service_ids: vec!["service-id".into()],
            ..Default::default()
        },
    );
    assert_eq!(
        config["services"]["service-id"]["deploy"]["multiRegionConfig"],
        json!({ "europe-west4": { "numReplicas": 1 } })
    );
    assert_eq!(config["volumes"]["volume-id"]["region"], "europe-west4");
}

#[test]
fn database_region_change_is_destructive() {
    let current = graph_from(vec![postgres("db", Some("us-west2"))]);
    let desired = graph_from(vec![postgres("db", Some("europe-west4"))]);
    let change = &diff(&current, &desired).changes[0];
    assert_eq!(change["severity"], "destructive");
    assert_eq!(change["summary"], "Move database db to europe-west4");
}

#[test]
fn imported_database_without_explicit_region_is_clean() {
    let current = environment_config_to_graph(
        &json!({
            "services": {
                "db-id": {
                    "source": { "image": "ghcr.io/railwayapp-templates/postgres-ssl:18" },
                    "deploy": {
                        "multiRegionConfig": { "us-east4-eqdc4a": { "numReplicas": 1 } },
                        "requiredMountPath": "/var/lib/postgresql/data"
                    },
                    "volumeMounts": { "vol-id": { "mountPath": "/var/lib/postgresql/data" } }
                }
            }
        }),
        &EnvironmentConfigToGraphOptions {
            project_name: Some("app".into()),
            service_names_by_id: super::compiler::map_from_str(&[("db-id", "postgres")]),
            template_service_ids_by_id: super::compiler::map_from_str(&[("db-id", "tpl-postgres")]),
            ..Default::default()
        },
    );
    let desired = graph_from(vec![postgres("postgres", None)]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn custom_domain_registration_is_diagnosed() {
    let current = env_config(json!({ "services": {} }));
    let desired = graph_from(vec![service(
        "web",
        json!({ "networking": { "customDomains": { "app.example.com": { "port": 8080 } } } }),
    )]);
    let result = diff(&current, &desired);
    assert!(!result.changes.iter().any(|c| c["kind"] == "domain.create"));
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("not supported"))
    );
}

#[test]
fn existing_custom_domains_are_plan_clean() {
    let current = environment_config_to_graph(
        &json!({ "services": { "web": {} } }),
        &EnvironmentConfigToGraphOptions {
            project_name: Some("app".into()),
            custom_domains_by_service_id: {
                let mut map = serde_json::Map::new();
                map.insert("web".into(), json!({ "app.example.com": {} }));
                map
            },
            ..Default::default()
        },
    );
    let desired = graph_from(vec![service(
        "web",
        json!({ "networking": { "customDomains": { "app.example.com": {} } } }),
    )]);
    let result = diff(&current, &desired);
    assert!(result.changes.is_empty());
    assert!(result.diagnostics.is_empty());
}

fn imported_postgres(networking: Option<Value>) -> super::graph::RailwayGraph {
    let mut service = json!({
        "source": { "image": "ghcr.io/railwayapp-templates/postgres-ssl:18" },
        "deploy": {
            "multiRegionConfig": { "us-east4-eqdc4a": { "numReplicas": 1 } },
            "requiredMountPath": "/var/lib/postgresql/data"
        },
        "volumeMounts": { "vol-id": { "mountPath": "/var/lib/postgresql/data" } }
    });
    if let Some(networking) = networking {
        service["networking"] = networking;
    }
    environment_config_to_graph(
        &json!({ "services": { "db-id": service } }),
        &EnvironmentConfigToGraphOptions {
            project_name: Some("app".into()),
            service_names_by_id: super::compiler::map_from_str(&[("db-id", "postgres")]),
            template_service_ids_by_id: super::compiler::map_from_str(&[("db-id", "tpl-postgres")]),
            ..Default::default()
        },
    )
}

fn with_networking(mut node: Value, networking: Value) -> Value {
    node["networking"] = networking;
    node
}

#[test]
fn empty_database_tcp_proxies_converge_when_no_proxy_exists() {
    let current = imported_postgres(None);
    let desired = graph_from(vec![with_networking(
        postgres("postgres", None),
        json!({ "tcpProxies": {} }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());

    let current = managed_db_config(json!({
        "services": {
            "cache": {
                "source": { "image": "railwayapp/redis:8.2" },
                "deploy": { "requiredMountPath": "/bitnami" }
            }
        }
    }));
    let desired = graph_from(vec![with_networking(
        redis("cache"),
        json!({ "tcpProxies": {} }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn redis_image_without_template_provenance_imports_as_service() {
    let current = env_config(json!({
        "services": { "cache": { "source": { "image": "redis:7" } } }
    }));
    let cache = &current.resources[0];
    assert_eq!(cache["address"], "service.cache");
    assert_eq!(cache["type"], "service");
    assert_eq!(cache["source"], image("redis:7"));

    let desired = graph_from(vec![service(
        "cache",
        json!({ "source": image("redis:7") }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn redis_image_with_template_provenance_imports_as_database() {
    let current = managed_db_config(json!({
        "services": { "cache": { "source": { "image": "railwayapp/redis:8.2" } } }
    }));
    let cache = &current.resources[0];
    assert_eq!(cache["address"], "database.cache");
    assert_eq!(cache["engine"], "redis");
}

#[test]
fn service_declared_over_managed_database_is_not_replaced() {
    let current = managed_db_config(json!({
        "services": { "cache": { "source": { "image": "railwayapp/redis:8.2" } } }
    }));
    let desired = graph_from(vec![service(
        "cache",
        json!({ "source": image("railwayapp/redis:8.2") }),
    )]);
    let result = diff(&current, &desired);
    assert!(result.changes.is_empty());
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].severity, "warning");
    assert_eq!(result.diagnostics[0].path, "resources.service.cache");

    // Edits on the mismatched pair still apply as the same service.
    let desired = graph_from(vec![service(
        "cache",
        json!({
            "source": image("railwayapp/redis:8.2"),
            "variables": { "MAXMEMORY": { "type": "literal", "value": "1gb" } }
        }),
    )]);
    let result = diff(&current, &desired);
    assert_eq!(result.diagnostics.len(), 1);
    assert_only_variable_set(&result, "MAXMEMORY", "service.cache");

    // And the other way round: a plain service declared as database().
    let current = env_config(json!({
        "services": { "cache": { "source": { "image": "railwayapp/redis:8.2" } } }
    }));
    let result = diff(&current, &graph_from(vec![redis("cache")]));
    assert!(result.changes.is_empty());
    assert_eq!(result.diagnostics.len(), 1);

    let mut cache = redis("cache");
    cache["variables"] = json!({ "MAXMEMORY": { "type": "literal", "value": "1gb" } });
    let result = diff(&current, &graph_from(vec![cache]));
    assert_eq!(result.diagnostics.len(), 1);
    assert_only_variable_set(&result, "MAXMEMORY", "database.cache");
}

fn assert_only_variable_set(result: &super::change_set::ChangeSet, variable: &str, address: &str) {
    assert_eq!(kinds(result), vec!["variable.set".to_string()]);
    assert_eq!(result.changes[0]["variable"], variable);
    assert_eq!(result.changes[0]["address"], address);
}

#[test]
fn imported_database_keeps_its_tcp_proxy() {
    let current = imported_postgres(Some(json!({ "tcpProxies": { "5432": {} } })));
    let database = current
        .resources
        .iter()
        .find(|resource| resource["address"] == "database.postgres")
        .unwrap();
    assert_eq!(
        database["networking"],
        json!({ "tcpProxies": { "5432": {} } })
    );

    let desired = graph_from(vec![with_networking(
        postgres("postgres", None),
        json!({ "tcpProxies": { "5432": {} } }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn unauthored_database_networking_is_not_drift() {
    let current = imported_postgres(Some(json!({ "tcpProxies": { "5432": {} } })));
    let desired = graph_from(vec![postgres("postgres", None)]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn empty_tcp_proxies_warn_instead_of_planning_a_removal_the_apply_skips() {
    let current = imported_postgres(Some(json!({ "tcpProxies": { "5432": {} } })));
    let desired = graph_from(vec![with_networking(
        postgres("postgres", None),
        json!({ "tcpProxies": {} }),
    )]);
    let change_set = diff(&current, &desired);
    assert!(change_set.changes.is_empty());
    let warning = &change_set.diagnostics[0];
    assert_eq!(warning.severity, "warning");
    assert_eq!(
        warning.path,
        "resources.database.postgres.networking.tcpProxies"
    );
    assert!(warning.message.contains(r#"{ "5432": null }"#));
}

#[test]
fn a_null_tcp_proxy_entry_plans_the_removal_and_converges() {
    let current = imported_postgres(Some(json!({ "tcpProxies": { "5432": {} } })));
    let desired = graph_from(vec![with_networking(
        postgres("postgres", None),
        json!({ "tcpProxies": { "5432": null } }),
    )]);
    let change_set = diff(&current, &desired);
    assert_eq!(kinds(&change_set), vec!["resource.update"]);
    let change = &change_set.changes[0];
    assert_eq!(change["field"], "networking");
    assert_eq!(change["summary"], "Update postgres networking");
    assert_eq!(change["before"], json!({ "tcpProxies": { "5432": {} } }));
    // The apply is sent the block as written, so the `null` entry survives to
    // the server — that entry is what deletes the proxy.
    assert_eq!(change["after"], json!({ "tcpProxies": { "5432": null } }));
    assert!(change_set.diagnostics.is_empty());

    // The proxy is gone; the same file has nothing left to do.
    let current = imported_postgres(None);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn omitted_networking_keys_keep_what_railway_has() {
    let current = imported_postgres(Some(json!({
        "privateNetworkEndpoint": "postgres",
        "tcpProxies": { "5432": {} }
    })));
    // tcpProxies left out: the apply never touches the proxy.
    let desired = graph_from(vec![with_networking(
        postgres("postgres", None),
        json!({ "privateNetworkEndpoint": "postgres" }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
    // privateNetworkEndpoint left out: the apply never touches the endpoint.
    let desired = graph_from(vec![with_networking(
        postgres("postgres", None),
        json!({ "tcpProxies": { "5432": {} } }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn service_tcp_empty_warns_about_a_proxy_it_cannot_remove() {
    let current = env_config(json!({
        "services": {
            "web": {
                "source": { "image": "ghcr.io/acme/web:1.2.3" },
                "networking": { "tcpProxies": { "8080": {} } }
            }
        }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({
            "source": image("ghcr.io/acme/web:1.2.3"),
            "networking": { "tcpProxies": {} }
        }),
    )]);
    let change_set = diff(&current, &desired);
    assert!(change_set.changes.is_empty());
    assert_eq!(change_set.diagnostics[0].severity, "warning");
    assert!(
        change_set.diagnostics[0]
            .message
            .contains(r#"{ "8080": null }"#)
    );
}

#[test]
fn public_database_declaration_plans_the_proxy() {
    let current = imported_postgres(None);
    let desired = graph_from(vec![with_networking(
        postgres("postgres", None),
        json!({ "tcpProxies": { "5432": {} } }),
    )]);
    let change_set = diff(&current, &desired);
    assert_eq!(kinds(&change_set), vec!["resource.update"]);
    assert_eq!(
        change_set.changes[0]["after"],
        json!({ "tcpProxies": { "5432": {} } })
    );
}

#[test]
fn empty_service_tcp_proxies_converge_when_no_proxy_exists() {
    let current = env_config(json!({
        "services": { "web": { "source": { "image": "ghcr.io/acme/web:1.2.3" } } }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({
            "source": image("ghcr.io/acme/web:1.2.3"),
            "networking": { "tcpProxies": {} }
        }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn round_tripped_config_plans_no_changes() {
    let desired = graph_from(vec![
        service(
            "web",
            json!({
                "source": github("railwayapp/demo"),
                "variables": { "PUBLIC_FLAG": { "type": "literal", "value": "on" } }
            }),
        ),
        service(
            "worker",
            json!({ "source": image("ghcr.io/acme/worker:1.2.3") }),
        ),
    ]);
    let current = environment_config_to_graph(
        &graph_to_environment_config(&desired, &CompileOptions::default()),
        &EnvironmentConfigToGraphOptions {
            project_name: Some("app".into()),
            ..Default::default()
        },
    );
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn tracing_round_trips_and_false_switches_are_not_drift() {
    let desired = graph_from(vec![service(
        "web",
        json!({
            "source": image("ghcr.io/acme/web:1"),
            "tracing": { "enabled": true, "autoInstrumentation": true }
        }),
    )]);
    let compiled = graph_to_environment_config(&desired, &CompileOptions::default());
    assert_eq!(
        compiled["services"]["web"]["tracing"],
        json!({ "enabled": true, "autoInstrumentation": true })
    );
    let current = environment_config_to_graph(
        &compiled,
        &EnvironmentConfigToGraphOptions {
            project_name: Some("app".into()),
            ..Default::default()
        },
    );
    assert!(diff(&current, &desired).changes.is_empty());

    // Railway serialises an untraced service without a block; an authored
    // `{ enabled: false }` means the same thing.
    let current = env_config(json!({
        "services": { "web": { "source": { "image": "ghcr.io/acme/web:1" } } }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": image("ghcr.io/acme/web:1"), "tracing": { "enabled": false } }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn tracing_changes_plan_an_update_with_the_right_deploy_effect() {
    let current = env_config(json!({
        "services": { "web": { "source": { "image": "ghcr.io/acme/web:1" } } }
    }));

    let enable = graph_from(vec![service(
        "web",
        json!({ "source": image("ghcr.io/acme/web:1"), "tracing": { "enabled": true } }),
    )]);
    let result = diff(&current, &enable);
    assert_eq!(kinds(&result), vec!["resource.update"]);
    assert_eq!(result.changes[0]["field"], "tracing");
    assert_eq!(result.changes[0]["after"], json!({ "enabled": true }));
    assert_eq!(result.changes[0]["deployEffect"], "deploy");

    let auto_only = graph_from(vec![service(
        "web",
        json!({
            "source": image("ghcr.io/acme/web:1"),
            "tracing": { "autoInstrumentation": true }
        }),
    )]);
    let result = diff(&current, &auto_only);
    assert_eq!(kinds(&result), vec!["resource.update"]);
    assert_eq!(result.changes[0]["deployEffect"], "none");

    let traced = env_config(json!({
        "services": {
            "web": {
                "source": { "image": "ghcr.io/acme/web:1" },
                "tracing": { "enabled": true, "autoInstrumentation": true }
            }
        }
    }));
    let untraced = graph_from(vec![service(
        "web",
        json!({ "source": image("ghcr.io/acme/web:1") }),
    )]);
    let result = diff(&traced, &untraced);
    assert_eq!(kinds(&result), vec!["resource.update"]);
    assert_eq!(result.changes[0]["field"], "tracing");
    assert!(result.changes[0]["after"].is_null());
    assert_eq!(result.changes[0]["deployEffect"], "deploy");
}

#[test]
fn new_service_carries_tracing_on_the_create() {
    let current = env_config(json!({ "services": {} }));
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": image("ghcr.io/acme/web:1"), "tracing": { "enabled": true } }),
    )]);
    let result = diff(&current, &desired);
    assert_eq!(kinds(&result), vec!["resource.create"]);
    assert_eq!(
        result.changes[0]["resource"]["tracing"],
        json!({ "enabled": true })
    );
}

#[test]
fn template_database_start_command_does_not_churn() {
    let current = managed_db_config(json!({
        "services": {
            "cache": {
                "source": { "image": "ghcr.io/railwayapp-templates/redis:8" },
                "deploy": {
                    "requiredMountPath": "/data",
                    "startCommand": "/bin/sh -c \"exec docker-entrypoint.sh redis-server --requirepass $REDIS_PASSWORD\""
                }
            }
        }
    }));
    let desired = graph_from(vec![redis("cache")]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn inline_volume_config_is_hoisted() {
    let graph = project_definition_to_graph(&json!({
        "name": "app",
        "resources": [service("web", json!({
            "volumeAttachments": {
                "data": {
                    "volume": "volume.data",
                    "mountPath": "/data",
                    "volumeConfig": { "sizeMB": 4096, "region": "europe-west4" }
                }
            }
        }))]
    }));
    let vol = graph
        .resources
        .iter()
        .find(|r| r["address"] == "volume.data")
        .unwrap();
    assert_eq!(
        vol["config"],
        json!({ "sizeMB": 4096, "region": "europe-west4" })
    );
    assert!(
        !serde_json::to_string(&graph)
            .unwrap()
            .contains("volumeConfig")
    );
}

#[test]
fn platform_volume_alerts_do_not_churn() {
    let current = environment_config_to_graph(
        &json!({
            "services": {
                "web": {
                    "source": { "image": "ghcr.io/acme/api:1.2.3" },
                    "volumeMounts": { "vol-1": { "mountPath": "/data" } }
                }
            },
            "volumes": {
                "vol-1": {
                    "sizeMB": 1024,
                    "region": "us-west2",
                    "alerts": { "usage": { "80": {}, "95": {}, "100": {} } },
                    "allowOnlineResize": true
                }
            }
        }),
        &EnvironmentConfigToGraphOptions {
            project_name: Some("app".into()),
            volume_names_by_id: super::compiler::map_from_str(&[("vol-1", "data")]),
            ..Default::default()
        },
    );
    let desired = project_definition_to_graph(&json!({
        "name": "app",
        "resources": [service("web", json!({
            "source": image("ghcr.io/acme/api:1.2.3"),
            "volumeAttachments": {
                "data": {
                    "volume": "volume.data",
                    "mountPath": "/data",
                    "volumeConfig": { "sizeMB": 1024, "region": "us-west2" }
                }
            }
        }))]
    }));
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn authored_database_start_command_still_diffs() {
    let node = |start: &str| {
        let mut db = postgres("db", None);
        db["deploy"] = json!({ "startCommand": start });
        graph_from(vec![db])
    };
    assert_eq!(
        diff(&node("old-start"), &node("new-start")).changes[0]["field"],
        "deploy"
    );
}

#[test]
fn never_deletes_database_realized_volume() {
    let current = environment_config_to_graph(
        &json!({
            "services": {
                "db": {
                    "source": { "image": "ghcr.io/railwayapp-templates/postgres-ssl:18" },
                    "deploy": { "requiredMountPath": "/var/lib/postgresql/data" },
                    "volumeMounts": { "vol-1": { "mountPath": "/var/lib/postgresql/data" } }
                }
            },
            "volumes": { "vol-1": { "sizeMB": 50000, "region": "us-west2" } }
        }),
        &EnvironmentConfigToGraphOptions {
            project_name: Some("app".into()),
            volume_names_by_id: super::compiler::map_from_str(&[("vol-1", "postgres-volume")]),
            template_service_ids_by_id: super::compiler::map_from_str(&[("db", "tpl-postgres")]),
            ..Default::default()
        },
    );
    let desired = graph_from(vec![postgres("db", None)]);
    let result = diff(&current, &desired);
    assert!(!kinds(&result).contains(&"resource.delete".to_string()));
}

#[test]
fn warns_instead_of_deleting_mounted_volume() {
    let current = environment_config_to_graph(
        &json!({
            "services": {
                "web": {
                    "source": { "image": "ghcr.io/acme/api:1.2.3" },
                    "volumeMounts": { "vol-1": { "mountPath": "/data" } }
                }
            },
            "volumes": { "vol-1": { "sizeMB": 1024, "region": "us-west2" } }
        }),
        &EnvironmentConfigToGraphOptions {
            project_name: Some("app".into()),
            volume_names_by_id: super::compiler::map_from_str(&[("vol-1", "data")]),
            ..Default::default()
        },
    );
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": image("ghcr.io/acme/api:1.2.3") }),
    )]);
    let result = diff(&current, &desired);
    assert!(!kinds(&result).contains(&"resource.delete".to_string()));
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("never deleted"))
    );
}

#[test]
fn removing_a_service_is_destructive() {
    let current = env_config(json!({
        "services": {
            "web": { "source": { "repo": "railwayapp/demo" } },
            "api": { "source": { "repo": "railwayapp/api" } }
        }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": github("railwayapp/demo") }),
    )]);
    let deletion = diff(&current, &desired)
        .changes
        .into_iter()
        .find(|c| c["kind"] == "resource.delete")
        .unwrap();
    assert_eq!(deletion["address"], "service.api");
    assert_eq!(deletion["severity"], "destructive");
}

#[test]
fn does_not_delete_resources_owned_by_another_partial() {
    let current = env_config(json!({
        "services": {
            "api": { "source": { "repo": "acme/api" } },
            "worker": { "source": { "repo": "acme/worker" } }
        }
    }));
    let desired = graph_from(vec![service(
        "api",
        json!({ "source": github("acme/api") }),
    )]);
    let mut owners = IacPartials::new();
    owners.insert("service.api".into(), "api".into());
    owners.insert("service.worker".into(), "worker".into());
    let result = diff_graphs(DiffOptions {
        current: &current,
        desired: &desired,
        reveal_values: false,
        partial: Some("api"),
        owners: Some(&owners),
    });
    assert!(!kinds(&result).contains(&"resource.delete".to_string()));
    assert_eq!(result.declared, vec!["service.api"]);
}

#[test]
fn errors_when_partial_declares_foreign_resource() {
    let current =
        env_config(json!({ "services": { "api": { "source": { "repo": "acme/api" } } } }));
    let desired = graph_from(vec![service(
        "api",
        json!({ "source": github("acme/api") }),
    )]);
    let mut owners = IacPartials::new();
    owners.insert("service.api".into(), "api".into());
    let result = diff_graphs(DiffOptions {
        current: &current,
        desired: &desired,
        reveal_values: false,
        partial: Some("worker"),
        owners: Some(&owners),
    });
    assert!(result.changes.is_empty());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("already managed by partial \"api\""))
    );
}

#[test]
fn errors_when_nameless_file_meets_named_partials() {
    let current =
        env_config(json!({ "services": { "api": { "source": { "repo": "acme/api" } } } }));
    let desired = graph_from(vec![service(
        "api",
        json!({ "source": github("acme/api") }),
    )]);
    let mut owners = IacPartials::new();
    owners.insert("service.api".into(), "api".into());
    let result = diff_graphs(DiffOptions {
        current: &current,
        desired: &desired,
        reveal_values: false,
        partial: None,
        owners: Some(&owners),
    });
    assert!(result.diagnostics.iter().any(|d| d.path == "partial"));
}

#[test]
fn whole_project_owner_still_deletes() {
    let current = env_config(json!({
        "services": {
            "web": { "source": { "repo": "railwayapp/demo" } },
            "api": { "source": { "repo": "railwayapp/api" } }
        }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": github("railwayapp/demo") }),
    )]);
    let mut owners = IacPartials::new();
    owners.insert("service.web".into(), "*".into());
    owners.insert("service.api".into(), "*".into());
    let result = diff_graphs(DiffOptions {
        current: &current,
        desired: &desired,
        reveal_values: false,
        partial: None,
        owners: Some(&owners),
    });
    assert!(
        result
            .changes
            .iter()
            .any(|c| c["address"] == "service.api" && c["kind"] == "resource.delete")
    );
}

#[test]
fn deletes_a_partials_own_omitted_service() {
    let current = env_config(json!({
        "services": {
            "api": { "source": { "repo": "acme/api" } },
            "extra": { "source": { "repo": "acme/extra" } }
        }
    }));
    let desired = graph_from(vec![service(
        "api",
        json!({ "source": github("acme/api") }),
    )]);
    let mut owners = IacPartials::new();
    owners.insert("service.api".into(), "api".into());
    owners.insert("service.extra".into(), "api".into());
    let result = diff_graphs(DiffOptions {
        current: &current,
        desired: &desired,
        reveal_values: false,
        partial: Some("api"),
        owners: Some(&owners),
    });
    assert!(
        result
            .changes
            .iter()
            .any(|c| c["address"] == "service.extra" && c["kind"] == "resource.delete")
    );
}

#[test]
fn variable_removal_is_destructive() {
    let current = env_config(json!({
        "services": { "web": { "source": { "repo": "r" }, "variables": { "OLD": { "value": "1" } } } }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({
            "source": github("r"),
            "variables": { "NEW": { "type": "literal", "value": "2" } }
        }),
    )]);
    let changes = diff(&current, &desired).changes;
    assert!(changes.iter().any(|c| c["kind"] == "variable.delete"
        && c["variable"] == "OLD"
        && c["severity"] == "destructive"));
    assert!(
        changes.iter().any(|c| c["kind"] == "variable.set"
            && c["variable"] == "NEW"
            && c["severity"] == "safe")
    );
}

#[test]
fn imported_variables_preserve_sealed_and_inline_decrypted() {
    let graph = env_config(json!({
        "services": {
            "web": {
                "source": { "repo": "r" },
                "variables": {
                    "PUBLIC_URL": { "value": "https://example.com", "isSealed": false },
                    "SECRET": { "value": "should-not-leak", "isSealed": true },
                    "MASKED": { "value": "" }
                }
            }
        }
    }));
    let vars = graph
        .resources
        .iter()
        .find(|r| r["name"] == "web")
        .and_then(|r| r.get("variables"))
        .cloned()
        .unwrap();
    assert_eq!(vars["PUBLIC_URL"]["type"], "literal");
    assert_eq!(vars["PUBLIC_URL"]["value"], "https://example.com");
    assert_eq!(vars["SECRET"]["type"], "preserve");
    assert_eq!(vars["MASKED"]["type"], "preserve");
}

#[test]
fn preserve_variable_never_plans() {
    let current = env_config(json!({
        "services": { "web": { "source": { "repo": "r" }, "variables": { "SECRET": { "value": "existing" } } } }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({
            "source": github("r"),
            "variables": { "SECRET": { "type": "preserve" } }
        }),
    )]);
    assert!(
        diff(&current, &desired)
            .changes
            .iter()
            .all(|c| { !c["kind"].as_str().unwrap_or("").starts_with("variable") })
    );
}

fn graph_with_policy(resources: Vec<Value>, policy: Value) -> super::graph::RailwayGraph {
    project_definition_to_graph(&json!({
        "name": "app",
        "variables": policy,
        "resources": resources,
    }))
}

fn variable_ops(change_set: &super::change_set::ChangeSet) -> Vec<String> {
    change_set
        .changes
        .iter()
        .filter(|change| {
            change["kind"]
                .as_str()
                .unwrap_or("")
                .starts_with("variable")
        })
        .map(|change| {
            format!(
                "{} {}:{}",
                change["kind"].as_str().unwrap_or(""),
                change["address"].as_str().unwrap_or(""),
                change["variable"].as_str().unwrap_or("")
            )
        })
        .collect()
}

#[test]
fn ignore_patterns_are_removed_from_both_sides_before_diff() {
    let current = env_config(json!({
        "services": {
            "web": { "source": { "repo": "r" }, "variables": {
                "DOPPLER_TOKEN": { "value": "a" },
                "doppler_token": { "value": "case" },
                "NOT_DOPPLER_TOKEN": { "value": "full" },
                "ONLY_API": { "value": "web" },
                "KEEP": { "value": "1" }
            }},
            "api": { "source": { "repo": "r" }, "variables": {
                "DOPPLER_TOKEN": { "value": "b" },
                "ONLY_API": { "value": "api" }
            }}
        }
    }));
    let desired = project_definition_to_graph(&super::eval::normalize_project(json!({
        "name": "app",
        "variables": { "managed": true, "ignore": ["DOPPLER_*", "api/ONLY_*"] },
        "resources": [
            service("web", json!({
                "source": github("r"),
                "env": {
                    "KEEP": { "type": "literal", "value": "1" },
                    "DOPPLER_NEW": { "type": "literal", "value": "nope" },
                    "PUBLIC": "on"
                }
            })),
            service("api", json!({ "source": github("r"), "variables": {} })),
        ]
    })));
    assert!(!desired.variables.default);
    assert_eq!(
        desired.variables.ignore,
        vec!["DOPPLER_*".to_string(), "api/ONLY_*".to_string()]
    );
    let ops = variable_ops(&diff(&current, &desired));
    assert!(
        !ops.iter()
            .any(|op| { op.ends_with(":DOPPLER_TOKEN") || op.ends_with(":DOPPLER_NEW") })
    );
    assert!(
        !ops.iter()
            .any(|op| op == "variable.delete service.api:ONLY_API")
    );
    assert!(
        ops.iter()
            .any(|op| op == "variable.delete service.web:ONLY_API")
    );
    assert!(
        ops.iter()
            .any(|op| op == "variable.delete service.web:doppler_token")
    );
    assert!(
        ops.iter()
            .any(|op| op == "variable.delete service.web:NOT_DOPPLER_TOKEN")
    );
    assert!(ops.iter().any(|op| op == "variable.set service.web:PUBLIC"));
    assert!(!ops.iter().any(|op| op.contains("KEEP")));
}

#[test]
fn unmanaged_variables_plan_no_sets_or_deletes() {
    let current = env_config(json!({
        "services": {
            "web": { "source": { "repo": "r" }, "variables": { "A": { "value": "1" }, "B": { "value": "2" } } },
            "quiet": { "source": { "repo": "r" }, "variables": { "C": { "value": "3" } } }
        }
    }));
    let desired = graph_with_policy(
        vec![
            service(
                "web",
                json!({
                    "source": github("r"),
                    "variables": {
                        "A": { "type": "literal", "value": "changed" },
                        "NEW": { "type": "literal", "value": "x" }
                    }
                }),
            ),
            service("quiet", json!({ "source": github("r") })),
        ],
        json!({ "managed": false, "ignore": [] }),
    );
    let result = diff(&current, &desired);
    assert!(variable_ops(&result).is_empty());
    let errors: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == "error")
        .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path, "resources.service.web.variables");
    assert_eq!(
        errors[0].message,
        "Variables are unmanaged (project variables.managed is false). Remove env from web or set managed: true."
    );
}

#[test]
fn omitted_env_errors_instead_of_deleting_explicit_empty_stays_authoritative() {
    let current = managed_db_config(json!({
        "services": {
            "web": { "source": { "repo": "r" }, "variables": { "B": { "value": "2" }, "A": { "value": "1" } } },
            "api": { "source": { "repo": "r" }, "variables": { "OLD": { "value": "1" }, "KEEP": { "value": "1" } } },
            "quiet": { "source": { "repo": "r" }, "variables": { "DOPPLER_TOKEN": { "value": "x" } } },
            "pg": {
                "source": { "image": "ghcr.io/railwayapp-templates/postgres-ssl:18" },
                "variables": { "POSTGRES_PASSWORD": { "value": "s" }, "CUSTOM": { "value": "x" } }
            }
        }
    }));
    let pg = current
        .resources
        .iter()
        .find(|resource| resource["name"] == "pg")
        .unwrap();
    assert!(pg.get("variables").is_none());
    let desired = graph_with_policy(
        vec![
            service("web", json!({ "source": github("r") })),
            service("api", json!({ "source": github("r"), "env": {} })),
            service("quiet", json!({ "source": github("r") })),
            postgres("pg", None),
        ],
        json!({ "managed": true, "ignore": ["DOPPLER_*"] }),
    );
    let result = diff(&current, &desired);
    let ops = variable_ops(&result);
    assert!(ops.iter().any(|op| op == "variable.delete service.api:OLD"));
    assert!(
        ops.iter()
            .any(|op| op == "variable.delete service.api:KEEP")
    );
    assert!(ops.iter().all(|op| !op.contains("service.web")));
    assert!(ops.iter().all(|op| !op.contains("service.quiet")));
    assert!(ops.iter().all(|op| !op.contains("pg")));
    let web = result
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.path == "resources.service.web.variables")
        .unwrap();
    assert_eq!(web.severity, "error");
    assert_eq!(
        web.message,
        "web declares no env but Railway has 2 variables (A, B). Declare them, mark them preserve(), or add them to variables.ignore."
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.path != "resources.service.quiet.variables")
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.path.contains("database.pg"))
    );
}

#[test]
fn declared_ignored_key_is_an_error_and_not_set() {
    let current = env_config(json!({ "services": { "web": { "source": { "repo": "r" } } } }));
    let desired = graph_with_policy(
        vec![service(
            "web",
            json!({
                "source": github("r"),
                "variables": {
                    "DOPPLER_TOKEN": { "type": "literal", "value": "x" },
                    "SECRET": { "type": "literal", "value": "y" },
                    "OK": { "type": "literal", "value": "z" }
                }
            }),
        )],
        json!({ "managed": true, "ignore": ["DOPPLER_*", "web/SECRET"] }),
    );
    let result = diff(&current, &desired);
    let ops = variable_ops(&result);
    assert!(
        ops.iter()
            .all(|op| !op.contains("DOPPLER") && !op.contains("SECRET"))
    );
    assert!(ops.iter().any(|op| op == "variable.set service.web:OK"));
    let messages: Vec<_> = result
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert!(messages.contains(&"web.DOPPLER_TOKEN is declared and also ignored by \"DOPPLER_*\"."));
    assert!(messages.contains(&"web.SECRET is declared and also ignored by \"web/SECRET\"."));
}

#[test]
fn malformed_ignore_patterns_error() {
    let current = env_config(json!({ "services": { "web": { "source": { "repo": "r" } } } }));
    let desired = graph_with_policy(
        vec![service(
            "web",
            json!({ "source": github("r"), "variables": {} }),
        )],
        json!({ "managed": true, "ignore": ["", "a/b/c", "web/", "ghost/*"] }),
    );
    let errors: Vec<_> = diff(&current, &desired)
        .diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.severity == "error")
        .map(|diagnostic| diagnostic.message)
        .collect();
    assert!(
        errors
            .iter()
            .any(|message| message == "variables.ignore contains an empty pattern.")
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("a/b/c") && message.contains("more than one"))
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("web/") && message.contains("empty key"))
    );
    assert!(errors.iter().any(|message| {
        message.contains("ghost") && message.contains("neither in the file nor on Railway")
    }));
}

#[test]
fn unused_ignore_pattern_warns() {
    let current = env_config(json!({
        "services": { "web": { "source": { "repo": "r" }, "variables": {
            "KEEP": { "value": "1" },
            "OTHER": { "value": "1" }
        } } }
    }));
    let desired = graph_with_policy(
        vec![service(
            "web",
            json!({
                "source": github("r"),
                "variables": { "OTHER": { "type": "preserve" } }
            }),
        )],
        json!({ "managed": true, "ignore": ["UNUSED_*", "KEEP"] }),
    );
    let result = diff(&current, &desired);
    let warnings: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == "warning")
        .collect();
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        warnings[0].message,
        "variables.ignore pattern \"UNUSED_*\" matches no variable."
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != "error")
    );
    assert!(variable_ops(&result).is_empty());
}

#[test]
fn variable_policy_report_and_header_line() {
    let bare = graph_from(vec![service("web", json!({}))]);
    assert!(bare.variables.default && bare.variables.managed && bare.variables.ignore.is_empty());
    let current = env_config(json!({
        "services": {
            "web": { "source": { "repo": "r" }, "variables": {
                "DOPPLER_A": { "value": "1" },
                "DOPPLER_B": { "value": "2" },
                "OTHER": { "value": "3" }
            }},
            "metabase": { "source": { "repo": "r" }, "variables": {
                "MB_DB": { "value": "1" },
                "SITE": { "value": "1" }
            }}
        }
    }));
    let default_report = super::change_set::variable_policy_report(&current, &bare);
    assert_eq!(
        super::graph::format_variable_policy_line(&default_report, 5),
        "variables: managed (default)"
    );

    let desired = graph_with_policy(
        vec![
            service(
                "web",
                json!({
                    "source": github("r"),
                    "variables": { "OTHER": { "type": "preserve" } }
                }),
            ),
            service(
                "metabase",
                json!({
                    "source": github("r"),
                    "variables": {
                        "MB_DB": { "type": "preserve" },
                        "SITE": { "type": "preserve" }
                    }
                }),
            ),
        ],
        json!({ "managed": true, "ignore": ["DOPPLER_*", "metabase/*"] }),
    );
    let report = super::change_set::variable_policy_report(&current, &desired);
    assert!(!report.default);
    assert_eq!(report.ignored_count, 4);
    assert_eq!(report.by_pattern["DOPPLER_*"], 2);
    assert_eq!(report.by_pattern["metabase/*"], 2);
    assert_eq!(
        super::graph::format_variable_policy_line(&report, 5),
        "variables: managed \u{00b7} 4 ignored on Railway (DOPPLER_* 2, metabase/* 2)"
    );
    let value = serde_json::to_value(&report).unwrap();
    let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "byPattern".to_string(),
            "default".to_string(),
            "ignore".to_string(),
            "ignoredCount".to_string(),
            "managed".to_string(),
        ]
    );

    let explicit = graph_with_policy(
        vec![service("web", json!({ "source": github("r") }))],
        json!({ "managed": true, "ignore": [] }),
    );
    assert_eq!(
        super::graph::format_variable_policy_line(
            &super::change_set::variable_policy_report(&current, &explicit),
            5
        ),
        "variables: managed"
    );

    let unmanaged = graph_with_policy(
        vec![service("web", json!({ "source": github("r") }))],
        json!({ "managed": false }),
    );
    assert_eq!(
        super::graph::format_variable_policy_line(
            &super::change_set::variable_policy_report(&current, &unmanaged),
            5
        ),
        "variables: unmanaged \u{00b7} 5 on Railway not managed"
    );
}

#[test]
fn redacts_variable_values_in_plan_output() {
    let secret = "sk-super-secret-value-123";
    let current = env_config(json!({ "services": { "web": { "source": { "repo": "r" } } } }));
    let desired = graph_from(vec![service(
        "web",
        json!({
            "source": github("r"),
            "variables": { "API_KEY": { "type": "literal", "value": secret } }
        }),
    )]);
    let change_set = diff(&current, &desired);
    let change = change_set
        .changes
        .iter()
        .find(|c| c["variable"] == "API_KEY")
        .unwrap();
    let rendered = format!(
        "{}{:?}{:?}",
        render_change_set(&change_set),
        change.get("summary"),
        change.get("details")
    );
    assert!(!rendered.contains(secret));
    assert!(rendered.contains("«hidden»"));
    assert_eq!(change["after"]["value"], secret);
}

#[test]
fn reveal_values_prints_secrets() {
    let secret = "sk-super-secret-value-123";
    let current = env_config(json!({ "services": { "web": { "source": { "repo": "r" } } } }));
    let desired = graph_from(vec![service(
        "web",
        json!({
            "source": github("r"),
            "variables": { "API_KEY": { "type": "literal", "value": secret } }
        }),
    )]);
    let change_set = diff_graphs(DiffOptions {
        current: &current,
        desired: &desired,
        reveal_values: true,
        partial: None,
        owners: None,
    });
    let details = change_set.changes[0]["details"][0].as_str().unwrap();
    assert!(details.contains(secret));
    assert!(!details.contains("«hidden»"));
}

#[test]
fn registry_credentials_first_apply_plans_and_redacts() {
    let password = "hunter2-secret";
    let current = env_config(
        json!({ "services": { "web": { "source": { "image": "ghcr.io/acme/api:1.2.3" } } } }),
    );
    let desired = graph_from(vec![service(
        "web",
        json!({
            "source": image("ghcr.io/acme/api:1.2.3"),
            "deploy": { "registryCredentials": { "username": "robot", "password": password } }
        }),
    )]);
    let changes = diff(&current, &desired).changes;
    assert_eq!(changes[0]["field"], "deploy");
    assert!(!format!("{:?}", changes[0]["details"]).contains(password));
}

#[test]
fn masked_registry_credentials_do_not_churn() {
    let current = env_config(json!({
        "services": { "web": {
            "source": { "image": "ghcr.io/acme/api:1.2.3" },
            "deploy": { "registryCredentials": { "username": "*****", "password": "*****" } }
        } }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({
            "source": image("ghcr.io/acme/api:1.2.3"),
            "deploy": { "registryCredentials": { "username": "robot", "password": "hunter2-secret" } }
        }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn null_registry_credentials_do_not_churn() {
    let current = env_config(json!({
        "services": { "web": {
            "source": { "image": "ghcr.io/acme/api:1.2.3" },
            "deploy": { "registryCredentials": null }
        } }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({
            "source": image("ghcr.io/acme/api:1.2.3"),
            "deploy": { "registryCredentials": { "username": "robot", "password": "hunter2-secret" } }
        }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn omitting_credentials_does_not_plan_removal() {
    let current = env_config(json!({
        "services": { "web": {
            "source": { "image": "ghcr.io/acme/api:1.2.3" },
            "deploy": { "registryCredentials": { "username": "*****", "password": "*****" } }
        } }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": image("ghcr.io/acme/api:1.2.3") }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn decrypted_credential_rotation_plans_without_leaking() {
    let password = "hunter2-secret";
    let current = env_config(json!({
        "services": { "web": {
            "source": { "image": "ghcr.io/acme/api:1.2.3" },
            "deploy": { "registryCredentials": { "username": "robot", "password": "old-password" } }
        } }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({
            "source": image("ghcr.io/acme/api:1.2.3"),
            "deploy": { "registryCredentials": { "username": "robot", "password": password } }
        }),
    )]);
    let changes = diff(&current, &desired).changes;
    let rendered = format!(
        "{:?}{:?}",
        changes[0].get("summary"),
        changes[0].get("details")
    );
    assert!(rendered.contains("registryCredentials"));
    assert!(!rendered.contains(password));
    assert!(!rendered.contains("old-password"));
}

#[test]
fn rotate_one_field_when_remote_is_masked() {
    let password = "hunter2-secret";
    let current = env_config(json!({
        "services": { "web": {
            "source": { "image": "ghcr.io/acme/api:1.2.3" },
            "deploy": { "registryCredentials": { "username": "*****", "password": "*****" } }
        } }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({
            "source": image("ghcr.io/acme/api:1.2.3"),
            "deploy": { "registryCredentials": { "username": "*****", "password": password } }
        }),
    )]);
    assert_eq!(
        diff(&current, &desired).changes[0]["after"],
        json!({ "registryCredentials": { "password": password } })
    );
}

#[test]
fn image_to_github_clears_credentials() {
    let current = env_config(json!({
        "services": { "web": {
            "source": { "image": "ghcr.io/acme/api:1.2.3" },
            "deploy": { "registryCredentials": { "username": "*****", "password": "*****" } }
        } }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": github("railwayapp/api") }),
    )]);
    let changes = diff(&current, &desired).changes;
    assert!(changes.iter().any(|c| c["field"] == "source"));
    assert!(changes
        .iter()
        .any(|c| c["field"] == "deploy" && c["after"] == json!({ "registryCredentials": null })));
}

#[test]
fn stale_auto_updates_on_github_source_do_not_drift() {
    let current = env_config(json!({
        "services": { "web": { "source": { "repo": "acme/api", "branch": "main", "autoUpdates": { "type": "patch", "schedule": [] } } } }
    }));
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": github("acme/api") }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn refuses_repo_config_owned_service() {
    let current = env_config(json!({
        "services": { "web": { "source": { "repo": "r" }, "configFile": "railway.json" } }
    }));
    let desired = graph_from(vec![service("web", json!({ "source": github("r") }))]);
    let result = diff(&current, &desired);
    assert!(result.changes.is_empty());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("railway.json"))
    );
}

#[test]
fn restores_volume_group_membership() {
    let graph = environment_config_to_graph(
        &json!({
            "groups": { "group-id": { "name": "Storage" } },
            "volumes": { "volume-id": { "region": "us-west2", "sizeMB": 1024 } }
        }),
        &EnvironmentConfigToGraphOptions {
            project_name: Some("app".into()),
            volume_names_by_id: super::compiler::map_from_str(&[("volume-id", "data")]),
            volume_group_ids_by_id: super::compiler::map_from_str(&[("volume-id", "group-id")]),
            ..Default::default()
        },
    );
    assert!(
        graph
            .resources
            .iter()
            .any(|r| r["address"] == "volume.data" && r["groupId"] == "Storage")
    );
}

#[test]
fn omits_unreferenced_canvas_groups() {
    let graph = env_config(json!({
        "groups": {
            "production": { "name": "Production" },
            "test": { "name": "Test-only Sandbox" }
        },
        "services": { "web": { "source": { "image": "nginx:latest" }, "groupId": "production" } }
    }));
    assert!(
        graph
            .resources
            .iter()
            .any(|r| r["address"] == "group.Production")
    );
    assert!(
        !graph
            .resources
            .iter()
            .any(|r| r["address"] == "group.Test-only Sandbox")
    );
}

#[test]
fn bucket_creation_requires_region() {
    let current = graph_from(vec![]);
    for resource in [
        json!({ "type": "bucket", "name": "assets" }),
        json!({ "type": "bucket", "name": "assets", "config": {} }),
        json!({ "type": "bucket", "name": "assets", "config": { "region": null } }),
    ] {
        let desired = graph_from(vec![resource]);
        let result = diff(&current, &desired);
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == "error"
                    && diagnostic.path == "resources.bucket.assets.config.region"
                    && diagnostic.message.contains("missing a region")
            }),
            "expected a missing-region diagnostic, got {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn bucket_creation_rejects_invalid_regions() {
    let current = graph_from(vec![]);
    for region in [
        json!(""),
        json!("auto"),
        json!("us-east4-eqdc4a"),
        json!("IAD"),
        json!(" iad "),
        json!(42),
        json!(false),
        json!([]),
        json!({ "region": "iad" }),
    ] {
        let desired = graph_from(vec![json!({
            "type": "bucket", "name": "assets", "config": { "region": region }
        })]);
        let result = diff(&current, &desired);
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == "error"
                    && diagnostic.path == "resources.bucket.assets.config.region"
                    && diagnostic.message.contains("sjc, iad, ams, sin")
            }),
            "expected an invalid-region diagnostic for {region}, got {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn bucket_creation_accepts_storage_regions() {
    let current = graph_from(vec![]);
    for region in ["sjc", "iad", "ams", "sin"] {
        let desired = graph_from(vec![bucket("assets", region)]);
        let result = diff(&current, &desired);
        assert!(result.diagnostics.is_empty(), "region {region}");
        assert_eq!(kinds(&result), ["resource.create"]);
    }
}

#[test]
fn bucket_creation_error_remains_with_a_co_created_service() {
    let current = graph_from(vec![]);
    let desired = graph_from(vec![
        json!({ "type": "bucket", "name": "assets" }),
        service("web", json!({ "source": image("nginx:latest") })),
    ]);
    let result = diff(&current, &desired);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == "error" && diagnostic.path == "resources.bucket.assets.config.region"
    }));
    assert!(result.changes.iter().any(|change| {
        change["kind"] == "resource.create" && change["address"] == "service.web"
    }));
}

#[test]
fn bucket_existing_unchanged_region_does_not_need_create_validation() {
    let current = env_config(json!({ "buckets": { "assets": { "region": "legacy-region" } } }));
    let desired = graph_from(vec![bucket("assets", "legacy-region")]);
    let result = diff(&current, &desired);
    assert!(result.diagnostics.is_empty());
    assert!(result.changes.is_empty());
}

#[test]
fn bucket_region_change_is_an_error() {
    let current = env_config(json!({ "buckets": { "assets": { "region": "sjc" } } }));
    let desired = graph_from(vec![bucket("assets", "ams")]);
    assert!(
        diff(&current, &desired)
            .diagnostics
            .iter()
            .any(|d| d.message.contains("region"))
    );
}

#[test]
fn evaluates_typescript_default_export() {
    let dir = tempfile_dir("railway-iac-ts-");
    let file = dir.join("railway.ts");
    std::fs::write(
        &file,
        r#"
export const partial = "api";
export default () => ({
  name: "app",
  resources: [{ address: "service.api", type: "service", name: "api" }],
});
"#,
    )
    .unwrap();
    let evaluated = evaluate_file(&file).expect("node should evaluate railway.ts");
    assert_eq!(evaluated.partial.as_deref(), Some("api"));
    assert!(
        evaluated
            .graph
            .resources
            .iter()
            .any(|r| r["address"] == "service.api")
    );
}

#[test]
fn evaluates_python_partial_and_graph() {
    if which::which("python3").is_err() {
        return;
    }
    let dir = tempfile_dir("railway-iac-py-");
    let file = dir.join("railway.py");
    std::fs::write(
        &file,
        r#"
PARTIAL = "api"
def main(ctx=None):
    return {"name": "app", "resources": [{"type": "service", "name": "api", "start": "echo api"}]}
"#,
    )
    .unwrap();
    let evaluated = evaluate_file(&file).expect("python3 should evaluate railway.py");
    assert_eq!(evaluated.partial.as_deref(), Some("api"));
    assert!(
        evaluated
            .graph
            .resources
            .iter()
            .any(|r| r["address"] == "service.api")
    );
}

#[test]
fn evaluates_go_partial_and_graph() {
    if which::which("go").is_err() {
        return;
    }
    let dir = tempfile_dir("railway-iac-go-");
    let file = dir.join("railway.go");
    std::fs::write(dir.join("go.mod"), "module railway-eval\n\ngo 1.22\n").unwrap();
    std::fs::write(
        &file,
        r#"
package main

const Partial = "api"

type graph map[string]any

func (g graph) Graph() map[string]any { return g }

func Railway() graph {
	return graph{
		"name": "app",
		"resources": []any{
			map[string]any{"type": "service", "name": "api", "start": "echo api"},
		},
	}
}
"#,
    )
    .unwrap();
    let evaluated = evaluate_file(&file).expect("go should evaluate railway.go");
    assert_eq!(evaluated.partial.as_deref(), Some("api"));
    assert!(
        evaluated
            .graph
            .resources
            .iter()
            .any(|r| r["address"] == "service.api")
    );
}

fn eval_context_production() -> EvalContext {
    EvalContext {
        command: Some("plan".into()),
        project_id: Some("proj_123".into()),
        project_name: Some("acme".into()),
        environment_id: Some("env_123".into()),
        environment: Some("production".into()),
        environment_name: Some("production".into()),
        pr: None,
    }
}

fn eval_context_pr() -> EvalContext {
    EvalContext {
        environment: Some("railway-cli-pr-123".into()),
        environment_name: Some("railway-cli-pr-123".into()),
        pr: Some(PrContext {
            number: 123,
            branch: Some("feat/x".into()),
            base: Some("dev".into()),
        }),
        ..eval_context_production()
    }
}

#[test]
fn evaluates_typescript_with_cli_context() {
    let dir = tempfile_dir("railway-iac-ts-ctx-");
    let file = dir.join("railway.ts");
    std::fs::write(
        &file,
        r#"
export default (ctx) => ({
  name: "app",
  resources: [{
    address: "service.api",
    type: "service",
    name: ctx.isEnvironment("production") ? "prod-api" : "dev-api",
  }],
});
"#,
    )
    .unwrap();
    let evaluated = evaluate_file_with_context(&file, &eval_context_production())
        .expect("node should evaluate railway.ts with context");
    assert!(
        evaluated
            .graph
            .resources
            .iter()
            .any(|r| r["name"] == "prod-api")
    );
}

#[test]
fn evaluates_python_with_cli_context() {
    if which::which("python3").is_err() {
        return;
    }
    let dir = tempfile_dir("railway-iac-py-ctx-");
    let file = dir.join("railway.py");
    std::fs::write(
        &file,
        r#"
def main(ctx=None):
    name = "prod-api" if ctx.is_environment("production") else "dev-api"
    return {"name": "app", "resources": [{"type": "service", "name": name, "start": "echo api"}]}
"#,
    )
    .unwrap();
    let evaluated = evaluate_file_with_context(&file, &eval_context_production())
        .expect("python3 should evaluate railway.py with context");
    assert!(
        evaluated
            .graph
            .resources
            .iter()
            .any(|r| r["name"] == "prod-api")
    );
}

#[test]
fn evaluates_go_with_cli_context() {
    if which::which("go").is_err() {
        return;
    }
    let dir = tempfile_dir("railway-iac-go-ctx-");
    let file = dir.join("railway.go");
    std::fs::write(dir.join("go.mod"), "module railway-eval\n\ngo 1.22\n").unwrap();
    std::fs::write(
        &file,
        r#"
package main

type graph map[string]any

func (g graph) Graph() map[string]any { return g }

type evalCtx struct {
	Environment     string
	EnvironmentName string
}

func Railway(ctx evalCtx) graph {
	name := "dev-api"
	if ctx.Environment == "production" {
		name = "prod-api"
	}
	return graph{
		"name": "app",
		"resources": []any{
			map[string]any{"type": "service", "name": name, "start": "echo api"},
		},
	}
}
"#,
    )
    .unwrap();
    let evaluated = evaluate_file_with_context(&file, &eval_context_production())
        .expect("go should evaluate railway.go with context");
    assert!(
        evaluated
            .graph
            .resources
            .iter()
            .any(|r| r["name"] == "prod-api")
    );
}

#[test]
fn eval_context_pr_environment_json() {
    let value = eval_context_pr().to_json();
    assert_eq!(
        value["pr"],
        json!({
            "number": 123,
            "branch": "feat/x",
            "base": "dev"
        })
    );
    assert!(value.get("ephemeral").is_none());
    assert!(value.get("baseEnvironment").is_none());
}

#[test]
fn eval_context_non_pr_environment_json() {
    let value = eval_context_production().to_json();
    assert!(value["pr"].is_null());
    assert!(value.get("ephemeral").is_none());
    assert!(value.get("baseEnvironment").is_none());
}

#[test]
fn evaluates_typescript_branching_on_pr_context() {
    let dir = tempfile_dir("railway-iac-ts-pr-");
    let file = dir.join("railway.ts");
    std::fs::write(
        &file,
        r#"
export default (ctx) => ({
  name: "app",
  resources: [{
    address: "service.api",
    type: "service",
    name: ctx.pr ? `${ctx.pr.base}:${ctx.pr.number}:${ctx.pr.branch}` : "no-pr",
  }],
});
"#,
    )
    .unwrap();
    let with_pr = evaluate_file_with_context(&file, &eval_context_pr())
        .expect("node should evaluate railway.ts with ctx.pr");
    assert!(
        with_pr
            .graph
            .resources
            .iter()
            .any(|r| r["name"] == "dev:123:feat/x")
    );
    let without_pr = evaluate_file_with_context(&file, &eval_context_production())
        .expect("node should evaluate railway.ts without ctx.pr");
    assert!(
        without_pr
            .graph
            .resources
            .iter()
            .any(|r| r["name"] == "no-pr")
    );
}

#[test]
fn eval_context_advertises_iac_features() {
    let value = eval_context_production().to_json();
    assert_eq!(value["features"], json!(IAC_FEATURES));
    assert_eq!(
        value["features"],
        json!([
            "variables-policy",
            "environments",
            "pr-context",
            "environment-owned-branch"
        ])
    );
}

#[test]
fn unknown_project_key_is_an_error() {
    let diagnostics = unknown_field_diagnostics(&json!({
        "name": "app",
        "notAField": true,
        "resources": []
    }));
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity, "error");
    assert_eq!(diagnostics[0].path, "project.notAField");
    assert_eq!(
        diagnostics[0].message,
        format!(
            "Unknown field \"notAField\" on project. This CLI (v{}) does not support it; upgrade the Railway CLI.",
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[test]
fn unknown_resource_key_is_an_error() {
    let diagnostics = unknown_field_diagnostics(&json!({
        "name": "app",
        "resources": [{
            "address": "service.web",
            "type": "service",
            "name": "web",
            "notAField": true
        }]
    }));
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity, "error");
    assert_eq!(diagnostics[0].path, "resources.service.web.notAField");
    assert_eq!(
        diagnostics[0].message,
        format!(
            "Unknown field \"notAField\" on service.web. This CLI (v{}) does not support it; upgrade the Railway CLI.",
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[test]
fn accepts_every_known_project_and_resource_key() {
    let mut project = serde_json::Map::new();
    for key in IAC_PROJECT_FIELDS {
        project.insert((*key).to_string(), json!({}));
    }
    let mut resource = serde_json::Map::new();
    for key in IAC_RESOURCE_FIELDS {
        resource.insert((*key).to_string(), json!({}));
    }
    resource.insert("address".into(), json!("service.web"));
    resource.insert("type".into(), json!("service"));
    resource.insert("name".into(), json!("web"));
    project.insert("resources".into(), json!([Value::Object(resource)]));
    let diagnostics = unknown_field_diagnostics(&Value::Object(project));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let fixtures = json!({
        "name": "app",
        "environments": ["production"],
        "variables": { "managed": false },
        "resources": [
            service("web", json!({
                "source": github("acme/web"),
                "start": "npm start",
                "build": "npm run build",
                "variables": { "PORT": { "type": "literal", "value": "8080" } },
                "volumeAttachments": {
                    "data": {
                        "volume": "volume.data",
                        "mountPath": "/data",
                        "volumeConfig": { "sizeMB": 1024 }
                    }
                }
            })),
            postgres("db", Some("us-west2")),
            volume("data", json!({ "sizeMB": 1024, "region": "us-west2" })),
            bucket("assets", "sjc"),
            {
                "address": "group.api",
                "type": "group",
                "name": "api",
                "color": "blue",
                "icon": "box",
                "isCollapsed": false
            }
        ]
    });
    let diagnostics = unknown_field_diagnostics(&fixtures);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

fn tempfile_dir(prefix: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("{prefix}{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn legacy_runner_only_when_explicitly_requested() {
    assert!(!super::use_legacy_ts_runner(None));
    assert!(super::use_legacy_ts_runner(Some("railway-iac-ts")));
}

fn github_source(repo: &str, branch: Option<&str>, root: Option<&str>) -> Value {
    let mut source = json!({ "type": "github", "repo": repo });
    if let Some(branch) = branch {
        source["branch"] = json!(branch);
    }
    if let Some(root) = root {
        source["rootDirectory"] = json!(root);
    }
    source
}

fn follow_with<'a>(
    in_pr_environment: bool,
    pr_branch: Option<&'a str>,
    pr_repo: Option<&'a str>,
    defaults: &'a std::collections::BTreeMap<String, String>,
) -> super::change_set::BranchFollow<'a> {
    super::change_set::BranchFollow {
        in_pr_environment,
        pr_branch,
        pr_repo,
        default_branches: defaults,
    }
}

fn diff_following(
    current: &super::graph::RailwayGraph,
    desired: &super::graph::RailwayGraph,
    follow: &super::change_set::BranchFollow<'_>,
) -> super::change_set::ChangeSet {
    super::change_set::diff_graphs_following(
        DiffOptions {
            current,
            desired,
            reveal_values: false,
            partial: None,
            owners: None,
        },
        Some(follow),
    )
}

#[test]
fn branchless_desired_source_does_not_change_a_remote_branch() {
    let current = graph_from(vec![service(
        "web",
        json!({ "source": github_source("acme/api", Some("develop"), None) }),
    )]);
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": github_source("acme/api", None, None) }),
    )]);
    assert!(diff(&current, &desired).changes.is_empty());
}

#[test]
fn branchless_source_change_omits_the_branch_key() {
    let current = graph_from(vec![service(
        "web",
        json!({ "source": github_source("acme/api", Some("develop"), Some("web")) }),
    )]);
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": github_source("acme/api", None, Some("api")) }),
    )]);
    let change = diff(&current, &desired)
        .changes
        .into_iter()
        .find(|change| change["field"] == "source")
        .unwrap();
    assert!(change["after"].get("branch").is_none());
    assert!(!change["after"].to_string().contains("branch"));
    assert_eq!(change["after"]["rootDirectory"], "api");
}

#[test]
fn new_branchless_service_plans_the_repo_default_branch() {
    let defaults = std::collections::BTreeMap::from([("acme/api".into(), "main".into())]);
    let follow = follow_with(false, None, None, &defaults);
    let current = graph_from(vec![]);
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": github_source("acme/api", None, None) }),
    )]);
    let change_set = diff_following(&current, &desired, &follow);
    assert!(change_set.diagnostics.is_empty());
    let create = &change_set.changes[0];
    assert_eq!(create["kind"], "resource.create");
    assert_eq!(create["resource"]["source"]["branch"], "main");
    assert_eq!(create["details"][0], "branch: main (repo default)");
    assert_eq!(
        create["summary"],
        "Create service web (branch: main (repo default))"
    );
}

#[test]
fn new_branchless_service_in_a_pr_environment_uses_the_pr_branch() {
    let defaults = std::collections::BTreeMap::from([
        ("acme/api".into(), "main".into()),
        ("other/lib".into(), "trunk".into()),
    ]);
    let follow = follow_with(true, Some("feat/login"), Some("Acme/API"), &defaults);
    let current = graph_from(vec![]);
    let desired = graph_from(vec![
        service(
            "web",
            json!({ "source": github_source("acme/api", None, None) }),
        ),
        service(
            "lib",
            json!({ "source": github_source("other/lib", None, None) }),
        ),
    ]);
    let change_set = diff_following(&current, &desired, &follow);
    assert!(change_set.diagnostics.is_empty());
    let web = change_set
        .changes
        .iter()
        .find(|change| change["address"] == "service.web")
        .unwrap();
    let lib = change_set
        .changes
        .iter()
        .find(|change| change["address"] == "service.lib")
        .unwrap();
    assert_eq!(web["resource"]["source"]["branch"], "feat/login");
    assert_eq!(web["details"][0], "branch: feat/login (PR branch)");
    assert_eq!(
        web["summary"],
        "Create service web (branch: feat/login (PR branch))"
    );
    assert_eq!(lib["resource"]["source"]["branch"], "trunk");
    assert_eq!(lib["details"][0], "branch: trunk (repo default)");
}

#[test]
fn new_branchless_service_errors_when_the_default_branch_is_unknown() {
    let defaults = std::collections::BTreeMap::new();
    let follow = follow_with(false, None, None, &defaults);
    let current = graph_from(vec![]);
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": github_source("acme/api", None, None) }),
    )]);
    let change_set = diff_following(&current, &desired, &follow);
    assert!(change_set.changes.is_empty());
    assert_eq!(
        change_set.diagnostics[0].message,
        "web is new and its github() source has no branch. Set branch for the first deploy; it can be removed afterwards."
    );
}

#[test]
fn pr_environment_rejects_a_pin_that_differs_from_the_deployed_branch() {
    let defaults = std::collections::BTreeMap::new();
    let follow = follow_with(true, Some("feat/login"), Some("acme/api"), &defaults);
    let current = graph_from(vec![service(
        "web",
        json!({ "source": github_source("acme/api", Some("feat/login"), None) }),
    )]);
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": github_source("acme/api", Some("main"), None) }),
    )]);
    let change_set = diff_following(&current, &desired, &follow);
    assert_eq!(
        change_set.diagnostics[0].message,
        "web pins branch \"main\" but this PR environment deploys \"feat/login\". Remove branch to follow the environment."
    );
}

#[test]
fn pr_environment_allows_another_repo_to_change_branch() {
    let defaults = std::collections::BTreeMap::new();
    let follow = follow_with(true, Some("feat/login"), Some("acme/api"), &defaults);
    let current = graph_from(vec![service(
        "lib",
        json!({ "source": github_source("other/lib", Some("main"), None) }),
    )]);
    let desired = graph_from(vec![service(
        "lib",
        json!({ "source": github_source("other/lib", Some("develop"), None) }),
    )]);
    let change_set = diff_following(&current, &desired, &follow);
    assert!(change_set.diagnostics.is_empty());
    assert!(
        change_set
            .changes
            .iter()
            .any(|change| change["field"] == "source")
    );
}

#[test]
fn pr_environment_allows_a_pin_matching_the_deployed_branch() {
    let defaults = std::collections::BTreeMap::new();
    let follow = follow_with(true, Some("feat/login"), Some("acme/api"), &defaults);
    let current = graph_from(vec![service(
        "web",
        json!({ "source": github_source("acme/api", Some("feat/login"), None) }),
    )]);
    let desired = graph_from(vec![service(
        "web",
        json!({ "source": github_source("acme/api", Some("feat/login"), None) }),
    )]);
    let change_set = diff_following(&current, &desired, &follow);
    assert!(change_set.diagnostics.is_empty());
    assert!(change_set.changes.is_empty());
}
