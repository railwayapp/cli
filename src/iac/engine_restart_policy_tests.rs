use super::*;
use crate::iac::change_set::diff_graphs;

fn config() -> Value {
    json!({"services": {
        "svc-web": {"deploy": {"healthcheckPath": "/health"}},
        "svc-other": {"deploy": {"restartPolicyType": "NEVER", "restartPolicyMaxRetries": 0}}
    }})
}

fn desired() -> super::super::graph::RailwayGraph {
    serde_json::from_value(json!({"version": 1, "project": {"name": "synthetic"},
        "resources": [{"type":"service", "kind":"empty", "name":"web", "address":"service.web",
            "deploy":{"healthcheckPath":"/health", "restartPolicyType":"ON_FAILURE", "restartPolicyMaxRetries":10}}]
    })).unwrap()
}

fn options() -> EnvironmentConfigToGraphOptions {
    EnvironmentConfigToGraphOptions {
        project_name: Some("synthetic".into()),
        service_names_by_id: serde_json::from_value(json!({"svc-web":"web", "svc-other":"other"}))
            .unwrap(),
        ..Default::default()
    }
}

fn response() -> Value {
    json!({"environment":{"id":"env-one", "configEtag":"etag-one"}, "serviceInstance":{
        "serviceId":"svc-web", "environmentId":"env-one",
        "restartPolicyType":"ON_FAILURE", "restartPolicyMaxRetries":10
    }})
}

fn merge(config: &mut Value, data: &Value) -> Result<Value> {
    merge_reported_restart_policy(
        config,
        "svc-web",
        "env-one",
        "etag-one",
        &["restartPolicyType", "restartPolicyMaxRetries"],
        data,
    )
}

fn diff(config: &Value) -> super::super::change_set::ChangeSet {
    let graph = environment_config_to_graph(config, &options());
    diff_graphs(DiffOptions {
        current: &graph,
        desired: &desired(),
        reveal_values: false,
        partial: Some("synthetic"),
        owners: None,
    })
}

#[test]
fn reported_equal_policy_completes_only_the_declared_service() {
    let mut current = config();
    let untouched = current["services"]["svc-other"].clone();
    let before = diff(&current);
    assert_eq!(before.changes.len(), 1);
    assert_eq!(before.changes[0]["deployEffect"], "deploy");
    assert_eq!(
        restart_policy_targets(&current, &desired(), &options()).unwrap(),
        vec![(
            "svc-web".to_string(),
            vec!["restartPolicyType", "restartPolicyMaxRetries"]
        )]
    );
    let evidence = merge(&mut current, &response()).unwrap();
    assert_eq!(
        current["services"]["svc-web"]["deploy"],
        desired().resources[0]["deploy"]
    );
    assert_eq!(current["services"]["svc-other"], untouched);
    assert_eq!(evidence["source"], "serviceInstance");
    let after = diff(&current);
    assert!(after.changes.is_empty());
    assert!(after.diagnostics.is_empty());
    assert!(
        restart_policy_targets(&current, &desired(), &options())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_real_policy_difference_is_not_hidden() {
    let mut current = config();
    let mut native = response();
    native["serviceInstance"]["restartPolicyType"] = json!("ALWAYS");
    merge(&mut current, &native).unwrap();
    assert_eq!(
        current["services"]["svc-web"]["deploy"]["restartPolicyType"],
        "ALWAYS"
    );
    assert_ne!(
        current["services"]["svc-web"]["deploy"],
        desired().resources[0]["deploy"]
    );
    let changes = diff(&current);
    assert_eq!(changes.changes.len(), 1);
    assert_eq!(changes.changes[0]["deployEffect"], "deploy");
}

#[test]
fn zero_retries_are_preserved_without_inventing_defaults() {
    let mut current = config();
    let mut native = response();
    native["serviceInstance"]["restartPolicyMaxRetries"] = json!(0);
    merge(&mut current, &native).unwrap();
    assert_eq!(
        current["services"]["svc-web"]["deploy"]["restartPolicyMaxRetries"],
        0
    );
}

#[test]
fn existing_fields_must_agree_and_are_not_overwritten() {
    let mut current = config();
    current["services"]["svc-web"]["deploy"]["restartPolicyType"] = json!("ALWAYS");
    let before = current.clone();
    assert!(merge(&mut current, &response()).is_err());
    assert_eq!(current, before);
    let mut native = response();
    native["serviceInstance"]["restartPolicyType"] = json!("ALWAYS");
    merge(&mut current, &native).unwrap();
    assert_eq!(
        current["services"]["svc-web"]["deploy"]["restartPolicyType"],
        "ALWAYS"
    );
}

#[test]
fn explicit_null_in_serialized_config_fails_closed() {
    let mut current = config();
    current["services"]["svc-web"]["deploy"]["restartPolicyType"] = Value::Null;
    assert!(restart_policy_targets(&current, &desired(), &options()).is_err());
    let before = current.clone();
    assert!(merge(&mut current, &response()).is_err());
    assert_eq!(current, before);
}

#[test]
fn unknown_missing_null_ids_and_etag_drift_fail_without_mutating_config() {
    let mut cases = Vec::new();
    for (key, value) in [
        ("restartPolicyType", json!("UNKNOWN")),
        ("restartPolicyType", Value::Null),
        ("restartPolicyMaxRetries", Value::Null),
        ("restartPolicyMaxRetries", json!(-1)),
        ("restartPolicyMaxRetries", json!(1.5)),
        ("serviceId", json!("svc-wrong")),
        ("environmentId", json!("env-wrong")),
    ] {
        let mut native = response();
        native["serviceInstance"][key] = value;
        cases.push(native);
    }
    let mut missing = response();
    missing["serviceInstance"]
        .as_object_mut()
        .unwrap()
        .remove("restartPolicyType");
    cases.push(missing);
    let mut changed = response();
    changed["environment"]["configEtag"] = json!("etag-two");
    cases.push(changed);
    let mut wrong_env = response();
    wrong_env["environment"]["id"] = json!("env-wrong");
    cases.push(wrong_env);
    cases.push(json!({"errors":[{"message":"synthetic denial"}]}));
    for native in cases {
        let mut current = config();
        let before = current.clone();
        assert!(merge(&mut current, &native).is_err());
        assert_eq!(current, before);
    }
    let mut final_read = response();
    final_read["environment"]["configEtag"] = json!("etag-two");
    assert!(verify_restart_policy_etag(&final_read, "env-one", "etag-one").is_err());
}

#[test]
fn ambiguous_names_fail_closed_and_undeclared_services_are_not_selected() {
    let mut map = options();
    map.service_names_by_id
        .insert("svc-other".into(), json!("web"));
    assert!(restart_policy_targets(&config(), &desired(), &map).is_err());
    let mut no_policy = desired();
    no_policy.resources[0]["deploy"]
        .as_object_mut()
        .unwrap()
        .remove("restartPolicyType");
    no_policy.resources[0]["deploy"]
        .as_object_mut()
        .unwrap()
        .remove("restartPolicyMaxRetries");
    assert!(
        restart_policy_targets(&config(), &no_policy, &options())
            .unwrap()
            .is_empty()
    );
}
