use crate::{
    client::post_graphql,
    commands::{Configs, mutations, queries},
};
use anyhow::{Result, bail};
use reqwest::Client;
use std::fmt::Display;
use std::{collections::BTreeMap, str::FromStr};

/// Every variable the service sees at deploy time, sealed ones included.
///
/// A sealed variable comes back with a `None` value: it is set on the service,
/// but its value cannot be read back by anyone, including the account owner.
/// Callers that show the user (or an agent) what is configured want this, so a
/// sealed variable is not mistaken for a missing one. Callers that need to put
/// variables into a process environment want [`get_service_variables`].
pub async fn get_service_variables_including_sealed(
    client: &Client,
    configs: &Configs,
    project_id: String,
    environment_id: String,
    service_id: String,
) -> Result<BTreeMap<String, Option<String>>> {
    let vars = queries::variables_for_service_deployment::Variables {
        project_id,
        environment_id,
        service_id,
    };
    let response = post_graphql::<queries::VariablesForServiceDeployment, _>(
        client,
        configs.get_backboard(),
        vars,
    )
    .await?;

    Ok(response.variables_for_service_deployment)
}

/// The variables that have a readable value, for injecting into a process
/// environment. Sealed variables are dropped — there is no value to inject.
pub async fn get_service_variables(
    client: &Client,
    configs: &Configs,
    project_id: String,
    environment_id: String,
    service_id: String,
) -> Result<BTreeMap<String, String>> {
    let variables = get_service_variables_including_sealed(
        client,
        configs,
        project_id,
        environment_id,
        service_id,
    )
    .await?
    .into_iter()
    .filter_map(|(key, value)| value.map(|value| (key, value)))
    .collect();

    Ok(variables)
}

/// Stands in for a sealed variable's value, which nobody can read back.
///
/// `variable list` prints it in place of the value; `variable edit` round-trips it,
/// so leaving the line alone keeps the variable sealed and untouched. Both spell it
/// the same way on purpose — the editor's placeholder is what the table showed.
pub const SEALED_TOKEN: &str = "<sealed>";

/// One user-owned variable entry for the bulk editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditVariableEntry {
    /// Plaintext, empty string, or [`SEALED_TOKEN`].
    pub value: String,
    pub is_sealed: bool,
}

/// Snapshot used by `variable edit`: editable user vars plus read-only Railway-provided vars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditSnapshot {
    pub editable: BTreeMap<String, EditVariableEntry>,
    pub read_only: BTreeMap<String, String>,
}

/// Keys reserved for Railway-provided variables. User upserts/deletes are rejected client-side.
pub fn is_railway_reserved_key(key: &str) -> bool {
    key.starts_with("RAILWAY_")
}

pub async fn get_service_variables_for_edit(
    client: &Client,
    configs: &Configs,
    project_id: String,
    environment_id: String,
    service_id: String,
) -> Result<EditSnapshot> {
    let vars = queries::service_variables_for_edit::Variables {
        project_id: project_id.clone(),
        environment_id: environment_id.clone(),
        service_id: service_id.clone(),
    };
    let response =
        post_graphql::<queries::ServiceVariablesForEdit, _>(client, configs.get_backboard(), vars)
            .await?;

    let sealed_by_name: BTreeMap<String, bool> = response
        .environment
        .variables
        .edges
        .into_iter()
        .filter_map(|edge| {
            let node = edge.node;
            if node.service_id.as_deref() != Some(service_id.as_str()) {
                return None;
            }
            Some((node.name, node.is_sealed))
        })
        .collect();

    let mut editable = BTreeMap::new();
    for (name, value) in response.user_variables {
        if is_railway_reserved_key(&name) {
            continue;
        }
        let is_sealed = sealed_by_name.get(&name).copied().unwrap_or(false);
        let entry_value = match value {
            Some(v) => v,
            None if is_sealed => SEALED_TOKEN.to_string(),
            None => continue,
        };
        editable.insert(
            name,
            EditVariableEntry {
                value: entry_value,
                is_sealed,
            },
        );
    }

    let mut read_only = BTreeMap::new();
    for (name, value) in response.deployment_variables {
        if !is_railway_reserved_key(&name) {
            continue;
        }
        if let Some(value) = value {
            read_only.insert(name, value);
        }
    }

    Ok(EditSnapshot {
        editable,
        read_only,
    })
}

/// Upsert and/or delete user variables. Callers must have already filtered
/// Railway-reserved keys and skipped sealed tokens that should be preserved.
pub async fn apply_service_variable_changes(
    client: &Client,
    configs: &Configs,
    project_id: String,
    environment_id: String,
    service_id: String,
    upserts: BTreeMap<String, String>,
    deletes: Vec<String>,
    skip_deploys: bool,
) -> Result<()> {
    if !upserts.is_empty() {
        let vars = mutations::variable_collection_upsert::Variables {
            project_id: project_id.clone(),
            environment_id: environment_id.clone(),
            service_id: service_id.clone(),
            variables: upserts,
            skip_deploys: skip_deploys.then_some(true),
        };
        post_graphql::<mutations::VariableCollectionUpsert, _>(
            client,
            configs.get_backboard(),
            vars,
        )
        .await?;
    }

    for name in deletes {
        let vars = mutations::variable_delete::Variables {
            project_id: project_id.clone(),
            environment_id: environment_id.clone(),
            name,
            service_id: Some(service_id.clone()),
        };
        post_graphql::<mutations::VariableDelete, _>(client, configs.get_backboard(), vars).await?;
    }

    Ok(())
}

/// Reject apply attempts that touch Railway-reserved keys.
pub fn reject_reserved_keys(
    changes: &[crate::controllers::variable_edit::VarChange],
) -> Result<()> {
    for change in changes {
        if is_railway_reserved_key(&change.key) {
            bail!(
                "Cannot modify Railway-provided variable `{}`. Remove it from the editable section — Railway-provided variables are shown read-only.",
                change.key
            );
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct Variable {
    pub key: String,
    pub value: String,
}

impl FromStr for Variable {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let s = s.splitn(2, '=').collect::<Vec<&str>>();
        // Only the KEY must be non-empty: `KEY=` sets the variable to the
        // empty string, matching what the dashboard allows. Rejecting it
        // here failed the whole invocation at argv-parse time — with
        // multiple --set flags, one `KEY=` killed every other pair too,
        // with the error visible only on stderr.
        if s.len() != 2 || s[0].is_empty() {
            anyhow::bail!("Invalid variable format: {}", s.join("="))
        }
        Ok(Self {
            key: s[0].to_string(),
            value: s[1].to_string(),
        })
    }
}

#[cfg(test)]
mod variable_from_str_tests {
    use super::*;

    #[test]
    fn key_value_parses() {
        let v: Variable = "FOO=bar".parse().unwrap();
        assert_eq!((v.key.as_str(), v.value.as_str()), ("FOO", "bar"));
    }

    #[test]
    fn empty_value_sets_the_empty_string() {
        let v: Variable = "REPLICA_OF=".parse().unwrap();
        assert_eq!((v.key.as_str(), v.value.as_str()), ("REPLICA_OF", ""));
    }

    #[test]
    fn value_may_contain_equals_signs() {
        let v: Variable = "URL=postgres://u:p@h/db?a=b".parse().unwrap();
        assert_eq!(v.value, "postgres://u:p@h/db?a=b");
    }

    #[test]
    fn empty_key_is_rejected() {
        assert!("=value".parse::<Variable>().is_err());
    }

    #[test]
    fn missing_equals_is_rejected() {
        assert!("JUSTAKEY".parse::<Variable>().is_err());
    }
}

impl Display for Variable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}={}", self.key, self.value)
    }
}

#[cfg(test)]
mod sealed_variable_tests {
    use super::*;
    use crate::testkit::MockBackboard;
    use serde_json::json;

    /// The API reports a sealed variable as a present key with a null value:
    /// it is set, but nobody can read it back.
    fn response() -> serde_json::Value {
        json!({
            "variablesForServiceDeployment": {
                "DATABASE_URL": "postgres://user:pw@host:5432/db",
                "STRIPE_SECRET_KEY": null,
            }
        })
    }

    #[tokio::test]
    async fn sealed_variables_keep_their_name_and_lose_only_their_value() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub("VariablesForServiceDeployment", response());

        let variables = get_service_variables_including_sealed(
            &reqwest::Client::new(),
            &server.configs(&dir),
            "proj-1".to_string(),
            "env-1".to_string(),
            "svc-1".to_string(),
        )
        .await
        .unwrap();

        assert_eq!(
            variables.get("DATABASE_URL"),
            Some(&Some("postgres://user:pw@host:5432/db".to_string()))
        );
        // The name is here, so a caller knows the variable is already set...
        assert_eq!(variables.get("STRIPE_SECRET_KEY"), Some(&None));
        // ...and the value is not.
        assert_eq!(variables.len(), 2);
    }

    #[tokio::test]
    async fn process_environment_drops_sealed_variables() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub("VariablesForServiceDeployment", response());

        let variables = get_service_variables(
            &reqwest::Client::new(),
            &server.configs(&dir),
            "proj-1".to_string(),
            "env-1".to_string(),
            "svc-1".to_string(),
        )
        .await
        .unwrap();

        // There is nothing to inject into a process for a sealed variable.
        assert_eq!(
            variables,
            BTreeMap::from([(
                "DATABASE_URL".to_string(),
                "postgres://user:pw@host:5432/db".to_string()
            )])
        );
    }
}

#[cfg(test)]
mod edit_snapshot_tests {
    use super::*;
    use crate::controllers::variable_edit::VarChange;
    use crate::testkit::MockBackboard;
    use serde_json::json;

    fn edit_query_payload() -> serde_json::Value {
        json!({
            "userVariables": {
                "DATABASE_URL": "postgres://user:pw@host:5432/db",
                "LOG_LEVEL": "info",
                "STRIPE_SECRET_KEY": null,
                "RAILWAY_SERVICE_NAME": "api",
            },
            "deploymentVariables": {
                "DATABASE_URL": "postgres://user:pw@host:5432/db",
                "LOG_LEVEL": "info",
                "STRIPE_SECRET_KEY": null,
                "RAILWAY_SERVICE_NAME": "api",
                "RAILWAY_PROJECT_NAME": "demo",
            },
            "environment": {
                "variables": {
                    "edges": [
                        {
                            "node": {
                                "name": "DATABASE_URL",
                                "serviceId": "svc-1",
                                "isSealed": false
                            }
                        },
                        {
                            "node": {
                                "name": "LOG_LEVEL",
                                "serviceId": "svc-1",
                                "isSealed": false
                            }
                        },
                        {
                            "node": {
                                "name": "STRIPE_SECRET_KEY",
                                "serviceId": "svc-1",
                                "isSealed": true
                            }
                        },
                        {
                            "node": {
                                "name": "SHARED_SECRET",
                                "serviceId": null,
                                "isSealed": true
                            }
                        },
                        {
                            "node": {
                                "name": "OTHER_SERVICE",
                                "serviceId": "svc-other",
                                "isSealed": true
                            }
                        }
                    ]
                }
            }
        })
    }

    #[tokio::test]
    async fn fetch_maps_unrendered_user_vars_and_hides_railway_provided() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub("ServiceVariablesForEdit", edit_query_payload());

        let snapshot = get_service_variables_for_edit(
            &reqwest::Client::new(),
            &server.configs(&dir),
            "proj-1".to_string(),
            "env-1".to_string(),
            "svc-1".to_string(),
        )
        .await
        .unwrap();

        assert_eq!(
            snapshot
                .editable
                .get("DATABASE_URL")
                .map(|e| e.value.as_str()),
            Some("postgres://user:pw@host:5432/db")
        );
        assert!(!snapshot.editable["DATABASE_URL"].is_sealed);
        assert_eq!(
            snapshot
                .editable
                .get("STRIPE_SECRET_KEY")
                .map(|e| e.value.as_str()),
            Some(SEALED_TOKEN)
        );
        assert!(snapshot.editable["STRIPE_SECRET_KEY"].is_sealed);
        assert!(!snapshot.editable.contains_key("RAILWAY_SERVICE_NAME"));
        assert!(!snapshot.editable.contains_key("SHARED_SECRET"));
        assert!(!snapshot.editable.contains_key("OTHER_SERVICE"));

        assert_eq!(
            snapshot
                .read_only
                .get("RAILWAY_SERVICE_NAME")
                .map(String::as_str),
            Some("api")
        );
        assert_eq!(
            snapshot
                .read_only
                .get("RAILWAY_PROJECT_NAME")
                .map(String::as_str),
            Some("demo")
        );
        assert!(!snapshot.read_only.contains_key("DATABASE_URL"));
        assert!(!snapshot.read_only.contains_key("STRIPE_SECRET_KEY"));
    }

    #[tokio::test]
    async fn apply_sends_upsert_and_delete_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockBackboard::spawn();
        server.stub(
            "VariableCollectionUpsert",
            json!({ "variableCollectionUpsert": true }),
        );
        server.stub("VariableDelete", json!({ "variableDelete": true }));

        apply_service_variable_changes(
            &reqwest::Client::new(),
            &server.configs(&dir),
            "proj-1".to_string(),
            "env-1".to_string(),
            "svc-1".to_string(),
            BTreeMap::from([("LOG_LEVEL".into(), "debug".into())]),
            vec!["FEATURE_OLD".into()],
            true,
        )
        .await
        .unwrap();

        let upserts = server.variables_for("VariableCollectionUpsert");
        assert_eq!(upserts.len(), 1);
        assert_eq!(upserts[0]["projectId"], "proj-1");
        assert_eq!(upserts[0]["environmentId"], "env-1");
        assert_eq!(upserts[0]["serviceId"], "svc-1");
        assert_eq!(upserts[0]["variables"]["LOG_LEVEL"], "debug");
        assert_eq!(upserts[0]["skipDeploys"], true);

        let deletes = server.variables_for("VariableDelete");
        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0]["name"], "FEATURE_OLD");
        assert_eq!(deletes[0]["serviceId"], "svc-1");
    }

    #[test]
    fn reject_reserved_keys_blocks_railway_prefix() {
        let err = reject_reserved_keys(&[VarChange {
            kind: crate::controllers::variable_edit::VarChangeKind::Set,
            key: "RAILWAY_SERVICE_NAME".into(),
            before: None,
            after: Some("hacked".into()),
        }])
        .unwrap_err();
        assert!(err.to_string().contains("RAILWAY_SERVICE_NAME"));
    }
}
