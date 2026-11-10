use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;

use crate::{
    client::post_graphql,
    commands::Configs,
    gql::signals::{
        Signal, SignalCreate, SignalDefaultSet, SignalDelete, SignalReplace, SignalRuleSet,
        SignalRuleUnset, Signals, signal, signal_create, signal_default_set, signal_delete,
        signal_replace, signal_rule_set, signal_rule_unset, signals,
    },
};

pub type SignalType = signal_create::SignalType;

pub async fn resolve_scope_owner(configs: &Configs, explicit: Option<String>) -> Result<String> {
    if let Some(scope) = explicit {
        return parse_scope(&scope);
    }
    if let Ok(from_env) = std::env::var("RAILWAY_FLAGS_SCOPE") {
        return parse_scope(&from_env);
    }
    if let Ok(from_env) = std::env::var("RAILWAY_SIGNALS_OWNER") {
        return parse_scope(&from_env);
    }

    let linked = configs.get_linked_project().await.context(
        "Could not determine a project. Set RAILWAY_TOKEN to a project token, link a project with \
         `railway link`, or pass --scope project:<id>.",
    )?;
    Ok(format!("project:{}", linked.project))
}

fn parse_scope(raw: &str) -> Result<String> {
    let scope = raw.trim();
    if scope.starts_with("workspace:") || scope.starts_with("project:") {
        return Ok(scope.to_string());
    }
    bail!("scope must be workspace:<id> or project:<id>");
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

pub async fn delete_signal(
    client: &Client,
    configs: &Configs,
    owner: String,
    name: String,
) -> Result<signal_delete::SignalDeleteSignalDelete> {
    let vars = signal_delete::Variables {
        input: signal_delete::SignalDeleteInput { owner, name },
    };
    Ok(
        post_graphql::<SignalDelete, _>(client, configs.get_backboard(), vars)
            .await?
            .signal_delete,
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
            format!("flag {name} not found; create it with `railway flag set {name} <value>`")
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
        "true" => return Ok((SignalType::bool, Value::Bool(true))),
        "false" => return Ok((SignalType::bool, Value::Bool(false))),
        other if other.parse::<f64>().is_ok() => {
            let number = serde_json::Number::from_f64(other.parse::<f64>().unwrap())
                .context("invalid numeric flag value")?;
            return Ok((SignalType::number, Value::Number(number)));
        }
        other => return Ok((SignalType::string, Value::String(other.to_string()))),
    }
}

fn infer_type_from_json_value(value: &Value) -> Result<SignalType> {
    match value {
        Value::Bool(_) => Ok(SignalType::bool),
        Value::Number(_) => Ok(SignalType::number),
        Value::String(_) => Ok(SignalType::string),
        Value::Array(_) | Value::Object(_) => Ok(SignalType::json),
        Value::Null => bail!("null is not a valid flag value"),
    }
}

pub fn parse_value_for_type(raw: &str, signal_type: &SignalType) -> Result<Value> {
    let value = match signal_type {
        SignalType::string => Value::String(raw.to_string()),
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
        SignalType::bool => value.is_boolean(),
        SignalType::string => value.is_string(),
        SignalType::number => value.is_number(),
        SignalType::json => !value.is_null(),
        other => {
            let _ = other;
            false
        }
    }
}

pub fn format_signal_type(signal_type: &SignalType) -> &'static str {
    match signal_type {
        SignalType::bool => "bool",
        SignalType::string => "string",
        SignalType::number => "number",
        SignalType::json => "json",
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
        "bool" | "boolean" => Ok(SignalType::bool),
        "string" => Ok(SignalType::string),
        "number" => Ok(SignalType::number),
        "json" => Ok(SignalType::json),
        other => bail!("unsupported flag type: {other}"),
    }
}

pub fn parse_expression(raw: &str) -> Result<Value> {
    if raw.trim_start().starts_with('{') || raw.trim_start().starts_with('[') {
        return serde_json::from_str(raw)
            .context("expression must be valid JSON (Radar clause or bucket compare)");
    }

    parse_cel_expression(raw).with_context(
        || "expression must use CEL syntax like `workspace_plan == \"enterprise\"` or valid JSON",
    )
}

fn parse_cel_expression(raw: &str) -> Result<Value> {
    let raw = trim_wrapping_parens(raw.trim());

    if let Some(inner) = raw
        .strip_prefix("!")
        .or_else(|| raw.strip_prefix("NOT "))
        .map(str::trim)
    {
        return Ok(serde_json::json!({ "not": parse_cel_expression(inner)? }));
    }

    if let Some(parts) = split_top_level(raw, "||").or_else(|| split_top_level(raw, " OR ")) {
        return Ok(serde_json::json!({
            "or": parts
                .iter()
                .map(|part| parse_cel_expression(part))
                .collect::<Result<Vec<_>>>()?,
        }));
    }

    if let Some(parts) = split_top_level(raw, "&&").or_else(|| split_top_level(raw, " AND ")) {
        return Ok(serde_json::json!({
            "and": parts
                .iter()
                .map(|part| parse_cel_expression(part))
                .collect::<Result<Vec<_>>>()?,
        }));
    }

    parse_cel_clause(raw)
}

fn parse_cel_clause(raw: &str) -> Result<Value> {
    const OPS: [(&str, &str); 9] = [
        (" not contains ", "not_contains"),
        (" contains ", "contains"),
        (" matches ", "matches"),
        (">=", "gte"),
        ("<=", "lte"),
        ("!=", "neq"),
        ("==", "eq"),
        (">", "gt"),
        ("<", "lt"),
    ];

    let raw = raw.trim();
    if raw.starts_with("bucket(") {
        return parse_bucket_clause(raw);
    }

    if let Some(clause) = parse_receiver_call_clause(raw)? {
        return Ok(clause);
    }

    if let Some((attr, rest)) = split_once_operator(raw, " NOT IN ") {
        return parse_list_clause(attr, rest, "not_in_list");
    }

    if let Some((attr, rest)) = split_once_operator(raw, " IN ") {
        return parse_list_clause(attr, rest, "in_list");
    }

    if let Some((attr, list)) = split_once_operator(raw, " in ") {
        return parse_in_clause(attr, list);
    }

    for (token, op) in OPS {
        if let Some((attr, value)) = split_once_operator(raw, token) {
            let attr = parse_expression_attr(attr)?;
            let value = if op == "matches" {
                parse_regex_value(value.trim())?
            } else {
                parse_expression_value(value.trim())?
            };
            if attr.is_empty() {
                bail!("expression attribute is required");
            }
            return Ok(serde_json::json!({
                "attr": attr,
                "op": op,
                "value": value,
            }));
        }
    }

    bail!("unsupported expression syntax");
}

fn parse_receiver_call_clause(raw: &str) -> Result<Option<Value>> {
    let Some(open_paren) = raw.find('(') else {
        return Ok(None);
    };
    let Some(dot_index) = raw[..open_paren].rfind('.') else {
        return Ok(None);
    };
    let receiver = &raw[..dot_index];
    let method = &raw[dot_index + 1..open_paren];
    let Some(arg) = raw[open_paren + 1..].trim().strip_suffix(')') else {
        return Ok(None);
    };

    let attr = parse_expression_attr(receiver)?;
    match method.trim() {
        "contains" => Ok(Some(serde_json::json!({
            "attr": attr,
            "op": "contains",
            "value": parse_expression_value(arg.trim())?,
        }))),
        "matches" => Ok(Some(serde_json::json!({
            "attr": attr,
            "op": "matches",
            "value": parse_expression_value(arg.trim())?,
        }))),
        other => bail!("unsupported CEL receiver function: {other}"),
    }
}

fn parse_in_clause(attr: &str, list: &str) -> Result<Value> {
    let attr = parse_expression_attr(attr)?;
    let list = list.trim();

    if list.starts_with('[') {
        let values: Vec<Value> =
            serde_json::from_str(list).context("CEL list literals must be valid JSON arrays")?;
        let clauses = values
            .into_iter()
            .map(|value| {
                serde_json::json!({
                    "attr": attr,
                    "op": "eq",
                    "value": value,
                })
            })
            .collect::<Vec<_>>();
        return Ok(serde_json::json!({ "or": clauses }));
    }

    let list = list
        .strip_prefix('@')
        .unwrap_or(list)
        .trim()
        .trim_matches('"')
        .trim_matches('\'');
    if list.is_empty() {
        bail!("CEL `in` expression requires a list");
    }

    Ok(serde_json::json!({
        "attr": attr,
        "op": "in_list",
        "list": list,
    }))
}

fn parse_bucket_clause(raw: &str) -> Result<Value> {
    const OPS: [(&str, &str); 4] = [(">=", "gte"), ("<=", "lte"), (">", "gt"), ("<", "lt")];

    for (token, op) in OPS {
        if let Some((bucket, threshold)) = split_once_operator(raw, token) {
            let (attr, salt) = parse_bucket_ref(bucket)?;
            let threshold = threshold
                .trim()
                .parse::<f64>()
                .context("bucket threshold must be a number between 0 and 1")?;
            if !(0.0..=1.0).contains(&threshold) {
                bail!("bucket threshold must be between 0 and 1");
            }

            let mut bucket = serde_json::json!({ "attr": attr });
            if let Some(salt) = salt {
                bucket["salt"] = Value::String(salt);
            }

            return Ok(serde_json::json!({
                "bucket": bucket,
                "op": op,
                "value": threshold,
            }));
        }
    }

    bail!("bucket expression must compare with <, <=, >, or >=");
}

fn parse_bucket_ref(raw: &str) -> Result<(String, Option<String>)> {
    let inner = raw
        .trim()
        .strip_prefix("bucket(")
        .and_then(|value| value.strip_suffix(')'))
        .context("bucket expression must look like `bucket(attr) < 0.25`")?;
    let mut parts = inner.splitn(2, ',');
    let attr = parse_expression_attr(parts.next().unwrap_or_default())?;
    let salt = parts
        .next()
        .map(str::trim)
        .map(|part| {
            let part = part.strip_prefix("salt:").unwrap_or(part).trim();
            parse_bucket_salt(part)
        })
        .transpose()?;

    Ok((attr, salt))
}

fn parse_bucket_salt(raw: &str) -> Result<String> {
    if raw.is_empty() {
        bail!("bucket salt is required");
    }

    let unquoted = raw
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .or_else(|| {
            raw.strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        });
    Ok(unquoted.unwrap_or(raw).to_string())
}

fn parse_list_clause(attr: &str, rest: &str, op: &str) -> Result<Value> {
    let attr = parse_expression_attr(attr)?;
    let rest = rest.trim();
    let (list, match_mode) = if let Some(list) = rest.strip_suffix(" (substring)") {
        (list, Some("substring"))
    } else {
        (rest, None)
    };
    let list = list
        .trim()
        .strip_prefix('@')
        .context("Radar list references must start with @")?;

    let mut clause = serde_json::json!({
        "attr": attr,
        "op": op,
        "list": list,
    });
    if let Some(match_mode) = match_mode {
        clause["match"] = Value::String(match_mode.to_string());
    }
    Ok(clause)
}

fn parse_expression_attr(raw: &str) -> Result<String> {
    let attr = raw.trim().strip_prefix(':').unwrap_or(raw.trim()).trim();
    if attr.is_empty() {
        bail!("expression attribute is required");
    }
    if !attr
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.'))
    {
        bail!("expression attribute must be a CEL identifier path");
    }
    Ok(attr.to_string())
}

fn split_once_operator<'a>(raw: &'a str, token: &str) -> Option<(&'a str, &'a str)> {
    let index = raw.find(token)?;
    let before = &raw[..index];
    let after = &raw[index + token.len()..];
    Some((before, after))
}

fn split_top_level<'a>(raw: &'a str, token: &str) -> Option<Vec<&'a str>> {
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut parts = Vec::new();
    let mut index = 0usize;

    while index < raw.len() {
        let rest = &raw[index..];
        if rest.starts_with('(') {
            depth += 1;
            index += 1;
            continue;
        }
        if rest.starts_with(')') {
            depth = depth.saturating_sub(1);
            index += 1;
            continue;
        }
        if depth == 0 && rest.starts_with(token) {
            parts.push(raw[start..index].trim());
            index += token.len();
            start = index;
            continue;
        }
        index += 1;
    }

    if parts.is_empty() {
        None
    } else {
        parts.push(raw[start..].trim());
        Some(parts)
    }
}

fn trim_wrapping_parens(raw: &str) -> &str {
    let Some(inner) = raw.strip_prefix('(').and_then(|v| v.strip_suffix(')')) else {
        return raw;
    };

    let mut depth = 0usize;
    for (index, ch) in raw.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && index != raw.len() - 1 {
                    return raw;
                }
            }
            _ => {}
        }
    }

    inner.trim()
}

fn parse_expression_value(raw: &str) -> Result<Value> {
    if raw.is_empty() {
        bail!("expression value is required");
    }

    let unquoted = raw
        .strip_prefix('\'')
        .and_then(|v| v.strip_suffix('\''))
        .or_else(|| raw.strip_prefix('"').and_then(|v| v.strip_suffix('"')));

    if let Some(value) = unquoted {
        return Ok(Value::String(value.to_string()));
    }

    parse_signal_value(raw)
}

fn parse_regex_value(raw: &str) -> Result<Value> {
    let pattern = raw
        .strip_prefix('/')
        .and_then(|v| v.strip_suffix('/'))
        .context("matches expressions must use /regex/ syntax")?;
    Ok(Value::String(pattern.to_string()))
}

fn from_query_type(signal_type: &signal::SignalType) -> SignalType {
    match signal_type {
        signal::SignalType::bool => SignalType::bool,
        signal::SignalType::string => SignalType::string,
        signal::SignalType::number => SignalType::number,
        signal::SignalType::json => SignalType::json,
        signal::SignalType::Other(other) => SignalType::Other(other.clone()),
    }
}

fn to_replace_type(signal_type: SignalType) -> signal_replace::SignalType {
    match signal_type {
        SignalType::bool => signal_replace::SignalType::bool,
        SignalType::string => signal_replace::SignalType::string,
        SignalType::number => signal_replace::SignalType::number,
        SignalType::json => signal_replace::SignalType::json,
        SignalType::Other(other) => signal_replace::SignalType::Other(other),
    }
}

fn types_compatible(query_type: &signal::SignalType, create_type: &SignalType) -> bool {
    match (query_type, create_type) {
        (signal::SignalType::bool, SignalType::bool)
        | (signal::SignalType::string, SignalType::string)
        | (signal::SignalType::number, SignalType::number)
        | (signal::SignalType::json, SignalType::json) => true,
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
        signal::SignalType::bool => "bool",
        signal::SignalType::string => "string",
        signal::SignalType::number => "number",
        signal::SignalType::json => "json",
        signal::SignalType::Other(_) => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_bool_from_literals() {
        let (t, v) = infer_flag_value("true", None).unwrap();
        assert!(matches!(t, SignalType::bool));
        assert_eq!(v, Value::Bool(true));
    }

    #[test]
    fn infers_string_from_bare_word() {
        let (t, v) = infer_flag_value("blue", None).unwrap();
        assert!(matches!(t, SignalType::string));
        assert_eq!(v, Value::String("blue".into()));
    }

    #[test]
    fn infers_number_from_integer() {
        let (t, v) = infer_flag_value("42", None).unwrap();
        assert!(matches!(t, SignalType::number));
        assert!(v.is_number());
    }

    #[test]
    fn infers_json_from_object_literal() {
        let (t, _) = infer_flag_value(r#"{"dark":true}"#, None).unwrap();
        assert!(matches!(t, SignalType::json));
    }

    #[test]
    fn explicit_type_overrides_inference() {
        let (t, v) = infer_flag_value("42", Some("string")).unwrap();
        assert!(matches!(t, SignalType::string));
        assert_eq!(v, Value::String("42".into()));
    }

    #[test]
    fn parses_simple_when_expression() {
        let expr = parse_expression(r#"workspace_plan == "enterprise""#).unwrap();
        assert_eq!(
            expr,
            serde_json::json!({
                "attr": "workspace_plan",
                "op": "eq",
                "value": "enterprise",
            })
        );
    }

    #[test]
    fn parses_numeric_when_expression() {
        let expr = parse_expression("project_cpu_p50 >= 10").unwrap();
        assert_eq!(
            expr,
            serde_json::json!({
                "attr": "project_cpu_p50",
                "op": "gte",
                "value": 10,
            })
        );
    }

    #[test]
    fn parses_radar_list_expression() {
        let expr = parse_expression("source_repo in banned_repo_tokens").unwrap();
        assert_eq!(
            expr,
            serde_json::json!({
                "attr": "source_repo",
                "op": "in_list",
                "list": "banned_repo_tokens",
            })
        );
    }

    #[test]
    fn parses_compound_radar_expression() {
        let expr =
            parse_expression(r#"workspace_age_hours < 24 && user_risk_level == "risky""#).unwrap();
        assert_eq!(
            expr,
            serde_json::json!({
                "and": [
                    { "attr": "workspace_age_hours", "op": "lt", "value": 24 },
                    { "attr": "user_risk_level", "op": "eq", "value": "risky" },
                ],
            })
        );
    }

    #[test]
    fn parses_cel_not_expression() {
        let expr = parse_expression(r#"!(workspace_plan == "enterprise")"#).unwrap();
        assert_eq!(
            expr,
            serde_json::json!({
                "not": { "attr": "workspace_plan", "op": "eq", "value": "enterprise" },
            })
        );
    }

    #[test]
    fn parses_cel_receiver_function_expression() {
        let expr = parse_expression(r#"source_repo.contains("miner")"#).unwrap();
        assert_eq!(
            expr,
            serde_json::json!({
                "attr": "source_repo",
                "op": "contains",
                "value": "miner",
            })
        );
    }

    #[test]
    fn parses_cel_list_literal_in_expression() {
        let expr = parse_expression(r#"workspace_plan in ["enterprise", "pro"]"#).unwrap();
        assert_eq!(
            expr,
            serde_json::json!({
                "or": [
                    { "attr": "workspace_plan", "op": "eq", "value": "enterprise" },
                    { "attr": "workspace_plan", "op": "eq", "value": "pro" },
                ],
            })
        );
    }

    #[test]
    fn parses_bucket_when_expression() {
        let expr = parse_expression("bucket(workspace_id) < 0.25").unwrap();
        assert_eq!(
            expr,
            serde_json::json!({
                "bucket": { "attr": "workspace_id" },
                "op": "lt",
                "value": 0.25,
            })
        );
    }

    #[test]
    fn parses_bucket_when_expression_with_salt() {
        let expr = parse_expression(r#"bucket(workspace_id, "checkout.v2") >= 0.5"#).unwrap();
        assert_eq!(
            expr,
            serde_json::json!({
                "bucket": { "attr": "workspace_id", "salt": "checkout.v2" },
                "op": "gte",
                "value": 0.5,
            })
        );
    }

    #[test]
    fn rejects_bucket_threshold_outside_unit_range() {
        assert!(parse_expression("bucket(workspace_id) < 25").is_err());
    }

    #[test]
    fn still_accepts_legacy_radar_when_expression() {
        let expr = parse_expression(":workspace_plan == enterprise").unwrap();
        assert_eq!(
            expr,
            serde_json::json!({
                "attr": "workspace_plan",
                "op": "eq",
                "value": "enterprise",
            })
        );
    }

    #[test]
    fn still_accepts_json_when_expression() {
        let expr = parse_expression(r#"{"attr":"plan","op":"eq","value":"enterprise"}"#).unwrap();
        assert_eq!(
            expr,
            serde_json::json!({
                "attr": "plan",
                "op": "eq",
                "value": "enterprise",
            })
        );
    }
}
