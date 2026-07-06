use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;

use crate::{
    client::post_graphql,
    commands::Configs,
    gql::{
        queries,
        signals::{
            Signal, SignalCreate, SignalDefaultSet, SignalReplace, SignalRuleSet, SignalRuleUnset,
            Signals, signal, signal_create, signal_default_set, signal_replace, signal_rule_set,
            signal_rule_unset, signals,
        },
    },
};

pub type SignalType = signal_create::SignalType;

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

pub async fn get_signal(
    client: &Client,
    configs: &Configs,
    owner: String,
    name: String,
) -> Result<Option<signal::SignalSignal>> {
    let vars = signal::Variables { owner, name };
    Ok(
        post_graphql::<Signal, _>(client, configs.get_backboard(), vars)
            .await?
            .signal,
    )
}

pub async fn create_signal(
    client: &Client,
    configs: &Configs,
    owner: String,
    name: String,
    signal_type: SignalType,
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

pub async fn set_signal_default(
    client: &Client,
    configs: &Configs,
    owner: String,
    name: String,
    default: Value,
) -> Result<signal_default_set::SignalDefaultSetSignalDefaultSet> {
    let vars = signal_default_set::Variables {
        input: signal_default_set::SignalDefaultSetInput {
            owner,
            name,
            default,
        },
    };
    Ok(
        post_graphql::<SignalDefaultSet, _>(client, configs.get_backboard(), vars)
            .await?
            .signal_default_set,
    )
}

pub async fn replace_signal(
    client: &Client,
    configs: &Configs,
    owner: String,
    name: String,
    signal_type: SignalType,
    default: Value,
) -> Result<signal_replace::SignalReplaceSignalReplace> {
    let vars = signal_replace::Variables {
        input: signal_replace::SignalReplaceInput {
            owner,
            name,
            type_: to_replace_type(signal_type),
            default,
        },
    };
    Ok(
        post_graphql::<SignalReplace, _>(client, configs.get_backboard(), vars)
            .await?
            .signal_replace,
    )
}

pub async fn upsert_flag_default(
    client: &Client,
    configs: &Configs,
    owner: String,
    name: String,
    raw_value: &str,
    explicit_type: Option<&str>,
    force: bool,
) -> Result<UpsertFlagResult> {
    let (inferred_type, value) = infer_flag_value(raw_value, explicit_type)?;
    let existing = get_signal(client, configs, owner.clone(), name.clone()).await?;

    let signal = match existing {
        None => {
            let created = create_signal(client, configs, owner, name, inferred_type, value).await?;
            UpsertFlagResult::Created(created)
        }
        Some(existing) => {
            if !types_compatible(&existing.type_, &inferred_type) {
                if !force {
                    bail!(
                        "{name} is a {} flag; value requires {}. Pass --force with --type to redefine.",
                        format_query_signal_type(&existing.type_),
                        format_signal_type(&inferred_type),
                    );
                }
                let explicit = explicit_type.context(
                    "--type is required with --force when replacing an existing flag's type",
                )?;
                let forced_type = parse_signal_type(explicit)?;
                if !value_matches_type(&forced_type, &value) {
                    bail!(
                        "value is not valid for type {}",
                        format_signal_type(&forced_type),
                    );
                }
                let replaced =
                    replace_signal(client, configs, owner, name, forced_type, value).await?;
                UpsertFlagResult::Replaced(replaced)
            } else if !query_type_matches_value(&existing.type_, &value) {
                bail!(
                    "value is not valid for {} flag {name}",
                    format_query_signal_type(&existing.type_),
                );
            } else {
                let updated = set_signal_default(client, configs, owner, name, value).await?;
                UpsertFlagResult::Updated(updated)
            }
        }
    };

    Ok(signal)
}

pub enum UpsertFlagResult {
    Created(signal_create::SignalCreateSignalCreate),
    Updated(signal_default_set::SignalDefaultSetSignalDefaultSet),
    Replaced(signal_replace::SignalReplaceSignalReplace),
}

impl UpsertFlagResult {
    pub fn name(&self) -> &str {
        match self {
            UpsertFlagResult::Created(s) => &s.name,
            UpsertFlagResult::Updated(s) => &s.name,
            UpsertFlagResult::Replaced(s) => &s.name,
        }
    }
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
    let existing = get_signal(client, configs, owner.clone(), name.clone())
        .await?
        .with_context(|| {
            format!("flag {name} not found; create it with `railway flag {name} <value>`")
        })?;
    if !query_type_matches_value(&existing.type_, &value) {
        bail!(
            "rule value is not valid for {} flag {name}",
            format_query_signal_type(&existing.type_),
        );
    }

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

pub fn infer_flag_value(raw: &str, explicit_type: Option<&str>) -> Result<(SignalType, Value)> {
    if let Some(type_name) = explicit_type {
        let signal_type = parse_signal_type(type_name)?;
        let value = parse_value_for_type(raw, &signal_type)?;
        return Ok((signal_type, value));
    }

    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return Ok((infer_type_from_json_value(&value)?, value));
    }

    match raw {
        "true" => return Ok((SignalType::BOOL, Value::Bool(true))),
        "false" => return Ok((SignalType::BOOL, Value::Bool(false))),
        other if other.parse::<f64>().is_ok() => {
            let number = serde_json::Number::from_f64(other.parse::<f64>().unwrap())
                .context("invalid numeric flag value")?;
            return Ok((SignalType::NUMBER, Value::Number(number)));
        }
        other => return Ok((SignalType::STRING, Value::String(other.to_string()))),
    }
}

fn infer_type_from_json_value(value: &Value) -> Result<SignalType> {
    match value {
        Value::Bool(_) => Ok(SignalType::BOOL),
        Value::Number(_) => Ok(SignalType::NUMBER),
        Value::String(_) => Ok(SignalType::STRING),
        Value::Array(_) | Value::Object(_) => Ok(SignalType::JSON),
        Value::Null => bail!("null is not a valid flag value"),
    }
}

pub fn parse_value_for_type(raw: &str, signal_type: &SignalType) -> Result<Value> {
    let value = match signal_type {
        SignalType::STRING => Value::String(raw.to_string()),
        _ => parse_signal_value(raw)?,
    };
    if value_matches_type(&signal_type, &value) {
        Ok(value)
    } else {
        bail!(
            "value is not valid for type {}",
            format_signal_type(signal_type),
        )
    }
}

pub fn value_matches_type(signal_type: &SignalType, value: &Value) -> bool {
    match signal_type {
        SignalType::BOOL => value.is_boolean(),
        SignalType::STRING => value.is_string(),
        SignalType::NUMBER => value.is_number(),
        SignalType::JSON => !value.is_null(),
        other => {
            let _ = other;
            false
        }
    }
}

pub fn format_signal_type(signal_type: &SignalType) -> &'static str {
    match signal_type {
        SignalType::BOOL => "bool",
        SignalType::STRING => "string",
        SignalType::NUMBER => "number",
        SignalType::JSON => "json",
        _ => "unknown",
    }
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

pub fn parse_signal_type(raw: &str) -> Result<SignalType> {
    match raw.to_ascii_lowercase().as_str() {
        "bool" | "boolean" => Ok(SignalType::BOOL),
        "string" => Ok(SignalType::STRING),
        "number" => Ok(SignalType::NUMBER),
        "json" => Ok(SignalType::JSON),
        other => bail!("unsupported flag type: {other}"),
    }
}

pub fn parse_expression(raw: &str) -> Result<Value> {
    serde_json::from_str(raw)
        .context("expression must be valid JSON (Radar clause or bucket compare)")
}

fn from_query_type(signal_type: &signal::SignalType) -> SignalType {
    match signal_type {
        signal::SignalType::BOOL => SignalType::BOOL,
        signal::SignalType::STRING => SignalType::STRING,
        signal::SignalType::NUMBER => SignalType::NUMBER,
        signal::SignalType::JSON => SignalType::JSON,
        signal::SignalType::Other(other) => SignalType::Other(other.clone()),
    }
}

fn to_replace_type(signal_type: SignalType) -> signal_replace::SignalType {
    match signal_type {
        SignalType::BOOL => signal_replace::SignalType::BOOL,
        SignalType::STRING => signal_replace::SignalType::STRING,
        SignalType::NUMBER => signal_replace::SignalType::NUMBER,
        SignalType::JSON => signal_replace::SignalType::JSON,
        SignalType::Other(other) => signal_replace::SignalType::Other(other),
    }
}

fn types_compatible(query_type: &signal::SignalType, create_type: &SignalType) -> bool {
    match (query_type, create_type) {
        (signal::SignalType::BOOL, SignalType::BOOL)
        | (signal::SignalType::STRING, SignalType::STRING)
        | (signal::SignalType::NUMBER, SignalType::NUMBER)
        | (signal::SignalType::JSON, SignalType::JSON) => true,
        (signal::SignalType::Other(a), SignalType::Other(b)) => a == b,
        _ => false,
    }
}

fn query_type_matches_value(query_type: &signal::SignalType, value: &Value) -> bool {
    value_matches_type(&from_query_type(query_type), value)
}

pub fn parse_value_for_query_type(raw: &str, query_type: &signal::SignalType) -> Result<Value> {
    parse_value_for_type(raw, &from_query_type(query_type))
}

pub fn format_query_signal_type(signal_type: &signal::SignalType) -> &'static str {
    match signal_type {
        signal::SignalType::BOOL => "bool",
        signal::SignalType::STRING => "string",
        signal::SignalType::NUMBER => "number",
        signal::SignalType::JSON => "json",
        signal::SignalType::Other(_) => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_bool_from_literals() {
        let (t, v) = infer_flag_value("true", None).unwrap();
        assert!(matches!(t, SignalType::BOOL));
        assert_eq!(v, Value::Bool(true));
    }

    #[test]
    fn infers_string_from_bare_word() {
        let (t, v) = infer_flag_value("blue", None).unwrap();
        assert!(matches!(t, SignalType::STRING));
        assert_eq!(v, Value::String("blue".into()));
    }

    #[test]
    fn infers_number_from_integer() {
        let (t, v) = infer_flag_value("42", None).unwrap();
        assert!(matches!(t, SignalType::NUMBER));
        assert!(v.is_number());
    }

    #[test]
    fn infers_json_from_object_literal() {
        let (t, _) = infer_flag_value(r#"{"dark":true}"#, None).unwrap();
        assert!(matches!(t, SignalType::JSON));
    }

    #[test]
    fn explicit_type_overrides_inference() {
        let (t, v) = infer_flag_value("42", Some("string")).unwrap();
        assert!(matches!(t, SignalType::STRING));
        assert_eq!(v, Value::String("42".into()));
    }
}
