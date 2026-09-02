use crate::{
    client::post_graphql,
    commands::{Configs, queries},
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
