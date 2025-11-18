use crate::{
    client::post_graphql,
    commands::{Configs, queries},
};
use anyhow::Result;
use reqwest::Client;
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

#[derive(Clone)]
pub struct Variable {
    pub key: String,
    pub value: String,
}

impl FromStr for Variable {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let s = s.splitn(2, '=').collect::<Vec<&str>>();
        if s.len() != 2 || s.iter().any(|v| v.is_empty()) {
            anyhow::bail!("Invalid variable format: {}", s.join("="))
        }
        Ok(Self {
            key: s[0].to_string(),
            value: s[1].to_string(),
        })
    }
}
