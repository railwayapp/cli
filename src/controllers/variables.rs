use crate::{
    client::post_graphql,
    commands::{Configs, queries},
};
use anyhow::Result;
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
