use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;

use crate::{
    client::post_graphql,
    commands::Configs,
    gql::{
        queries,
        signals::{
            SignalCreate, SignalRuleSet, SignalRuleUnset, Signals,
            signal_create, signal_rule_set, signal_rule_unset, signals,
        },
    },
};

pub async fn resolve_owner(
    client: &Client,
    configs: &Configs,
    explicit: Option<String>,
) -> Result<String> {
    if let Some(owner) = explicit {
        return Ok(owner);
    }
    if let Ok(from_env) = std::env::var("RAILWAY_SIGNALS_OWNER") {
        return Ok(from_env);
    }

    let linked = configs
        .get_linked_project()
        .await
        .context("No linked project. Link one with `railway link` or pass --owner.")?;
    let vars = queries::project::Variables {
        id: linked.project.clone(),
    };
    let project = post_graphql::<queries::Project, _>(client, configs.get_backboard(), vars)
        .await?
        .project;
    let workspace_id = project
        .workspace_id
        .context("Linked project has no workspace id; pass --owner explicitly.")?;
    Ok(format!("workspace:{workspace_id}"))
}

pub async fn list_signals(
    client: &Client,
    configs: &Configs,
    owner: String,
) -> Result<Vec<signals::SignalsSignals>> {
    let vars = signals::Variables { owner };
    Ok(
        post_graphql::<Signals, _>(client, configs.get_backboard(), vars)
            .await?
            .signals,
    )
}

pub async fn create_signal(
    client: &Client,
    configs: &Configs,
    owner: String,
    name: String,
    signal_type: signal_create::SignalType,
    default: Value,
) -> Result<signal_create::SignalCreateSignalCreate> {
    let vars = signal_create::Variables {
        input: signal_create::SignalCreateInput {
            owner,
            name,
            type_: signal_type,
            default,
            writable_by: None,
        },
    };
    Ok(
        post_graphql::<SignalCreate, _>(client, configs.get_backboard(), vars)
            .await?
            .signal_create,
    )
}

pub async fn set_signal_rule(
    client: &Client,
    configs: &Configs,
    owner: String,
    name: String,
    rule_id: String,
    expression: Value,
    value: Value,
) -> Result<signal_rule_set::SignalRuleSetSignalRuleSet> {
    let vars = signal_rule_set::Variables {
        input: signal_rule_set::SignalRuleSetInput {
            owner,
            name,
            rule_id,
            expression,
            value,
        },
    };
    Ok(
        post_graphql::<SignalRuleSet, _>(client, configs.get_backboard(), vars)
            .await?
            .signal_rule_set,
    )
}

pub async fn unset_signal_rule(
    client: &Client,
    configs: &Configs,
    owner: String,
    name: String,
    rule_id: String,
) -> Result<signal_rule_unset::SignalRuleUnsetSignalRuleUnset> {
    let vars = signal_rule_unset::Variables {
        input: signal_rule_unset::SignalRuleUnsetInput {
            owner,
            name,
            rule_id,
        },
    };
    Ok(
        post_graphql::<SignalRuleUnset, _>(client, configs.get_backboard(), vars)
            .await?
            .signal_rule_unset,
    )
}

pub fn parse_signal_value(raw: &str) -> Result<Value> {
    if let Ok(value) = serde_json::from_str(raw) {
        return Ok(value);
    }
    match raw {
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        other if other.parse::<f64>().is_ok() => Ok(Value::Number(
            serde_json::Number::from_f64(other.parse::<f64>().unwrap())
                .context("invalid numeric signal value")?,
        )),
        other => Ok(Value::String(other.to_string())),
    }
}

pub fn parse_signal_type(raw: &str) -> Result<signal_create::SignalType> {
    match raw.to_ascii_uppercase().as_str() {
        "BOOL" | "BOOLEAN" => Ok(signal_create::SignalType::BOOL),
        "STRING" => Ok(signal_create::SignalType::STRING),
        "NUMBER" => Ok(signal_create::SignalType::NUMBER),
        "JSON" => Ok(signal_create::SignalType::JSON),
        other => bail!("unsupported signal type: {other}"),
    }
}

pub fn parse_expression(raw: &str) -> Result<Value> {
    serde_json::from_str(raw).context("expression must be valid JSON (Radar clause or bucket compare)")
}
