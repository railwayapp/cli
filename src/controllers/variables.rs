use crate::{
    client::post_graphql,
    commands::{Configs, queries},
};
use anyhow::Result;
use reqwest::Client;
use std::fmt::Display;
use std::{collections::BTreeMap, str::FromStr};

pub async fn get_service_variables(
    client: &Client,
    configs: &Configs,
    project_id: String,
    environment_id: String,
    service_id: String,
) -> Result<BTreeMap<String, String>> {
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

    let variables = response
        .variables_for_service_deployment
        .into_iter()
        .filter_map(|var| {
            if let Some(value) = var.1 {
                Some((var.0, value))
            } else {
                None
            }
        })
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
