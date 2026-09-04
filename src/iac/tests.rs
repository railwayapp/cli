use serde_json::{Value, json};

use super::change_set::{DiffOptions, RAILWAY_CHANGE_SET_VERSION, diff_graphs, render_change_set};
use super::compiler::{
    CompileOptions, EnvironmentConfigToGraphOptions, environment_config_to_graph,
    graph_to_environment_config, project_definition_to_graph,
};
use super::eval::{EvalContext, evaluate_file, evaluate_file_with_context};
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

    let current = env_config(json!({
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
fn template_database_start_command_does_not_churn() {
    let current = env_config(json!({
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
