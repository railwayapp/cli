use super::*;
use crate::{
    controllers::signals::{
        UpsertFlagResult, delete_signal, get_signal, list_signals, parse_expression,
        parse_value_for_query_type, resolve_scope_owner, set_signal_rule, unset_signal_rule,
        upsert_flag_default,
    },
    util::progress::create_spinner_if,
};

/// Manage feature flags
#[derive(Parser)]
#[command(subcommand_required = true, arg_required_else_help = true)]
#[clap(
    after_help = "Examples:\n\n  railway flag list\n  railway flag set checkout.v2 true\n  railway flag set theme \"blue\"\n  railway flag set checkout.v2 true --when 'plan == \"enterprise\"'\n  railway flag set checkout.v2 true --when \"bucket(key) < 0.25\"\n  railway flag delete checkout.v2\n  railway flag unset checkout.v2 --rule-id enterprise-on\n"
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Project containing the flags, as project:<id> (defaults to the project token or linked project)
    #[clap(long, global = true)]
    scope: Option<String>,

    /// Deprecated. Use --scope.
    #[clap(long, global = true, hide = true)]
    owner: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,
}

#[derive(Parser)]
enum Commands {
    /// List feature flags for the current project
    #[clap(visible_alias = "ls")]
    List(ListArgs),

    /// Set a flag's default value, or set a targeting rule with --when
    Set(SetArgs),

    /// Delete a feature flag
    #[clap(visible_alias = "rm", visible_alias = "remove")]
    Delete(DeleteArgs),

    /// Remove a rule from a flag
    Unset(UnsetArgs),
}

#[derive(Parser)]
struct ListArgs {
    /// Show rule IDs and empty rule sections
    #[clap(long)]
    full: bool,
}

#[derive(Parser)]
struct SetArgs {
    /// Flag name
    name: String,

    /// Default value, or rule value when --when is passed
    value: String,

    /// CEL expression subset, e.g. workspace_plan == "enterprise" or bucket(workspace_id) < 0.25
    /// (also accepts raw JSON)
    #[clap(long)]
    when: Option<String>,

    /// Stable rule id for --when (defaults to a hash of name + expression)
    #[clap(long)]
    rule_id: Option<String>,

    /// Flag type: bool, string, number, json (inferred from value when omitted)
    #[clap(long)]
    r#type: Option<String>,

    /// Allow replacing an existing flag's type (clears rules)
    #[clap(long)]
    force: bool,
}

#[derive(Parser)]
struct DeleteArgs {
    /// Flag name
    name: String,
}

#[derive(Parser)]
struct UnsetArgs {
    /// Flag name
    name: String,

    /// Rule id to remove
    #[clap(long)]
    rule_id: String,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let owner = resolve_scope_owner(&configs, args.scope.or(args.owner)).await?;

    match args.command {
        Commands::List(list_args) => {
            list_command(&client, &configs, owner, list_args, args.json).await
        }
        Commands::Set(set_args) => set_command(&client, &configs, owner, set_args, args.json).await,
        Commands::Delete(delete_args) => {
            delete_command(&client, &configs, owner, delete_args, args.json).await
        }
        Commands::Unset(unset_args) => {
            unset_command(&client, &configs, owner, unset_args, args.json).await
        }
    }
}

async fn list_command(
    client: &reqwest::Client,
    configs: &Configs,
    owner: String,
    args: ListArgs,
    json: bool,
) -> Result<()> {
    let signals = list_signals(client, configs, owner.clone()).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&signals)?);
        return Ok(());
    }

    if signals.is_empty() {
        eprintln!("No feature flags found for {owner}");
        return Ok(());
    }

    print_flags_tree(&signals, args.full);
    Ok(())
}

async fn upsert_command(
    client: &reqwest::Client,
    configs: &Configs,
    owner: String,
    name: String,
    raw_value: String,
    explicit_type: Option<&str>,
    force: bool,
    json: bool,
) -> Result<()> {
    let spinner = create_spinner_if(!json, format!("Updating {}...", name.bold()));
    let result = upsert_flag_default(
        client,
        configs,
        owner,
        name.clone(),
        &raw_value,
        explicit_type,
        force,
    )
    .await?;

    let message = match &result {
        UpsertFlagResult::Created(_) => format!("Created flag {}", name.bold()),
        UpsertFlagResult::Updated(_) => format!("Updated flag {}", name.bold()),
        UpsertFlagResult::Replaced(_) => {
            format!(
                "Replaced flag {} (type changed; rules cleared)",
                name.bold()
            )
        }
    };

    if let Some(sp) = spinner {
        sp.finish_with_message(message);
    } else {
        let output = match &result {
            UpsertFlagResult::Created(s) => serde_json::to_value(s)?,
            UpsertFlagResult::Updated(s) => serde_json::to_value(s)?,
            UpsertFlagResult::Replaced(s) => serde_json::to_value(s)?,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    }
    Ok(())
}

async fn set_command(
    client: &reqwest::Client,
    configs: &Configs,
    owner: String,
    args: SetArgs,
    json: bool,
) -> Result<()> {
    let Some(when) = args.when else {
        return upsert_command(
            client,
            configs,
            owner,
            args.name,
            args.value,
            args.r#type.as_deref(),
            args.force,
            json,
        )
        .await;
    };

    let expression = parse_expression(&when)?;
    let existing = get_signal(client, configs, owner.clone(), args.name.clone())
        .await?
        .with_context(|| {
            format!(
                "flag {} not found; create it with `railway flag set {} <value>`",
                args.name, args.name
            )
        })?;
    let value = parse_value_for_query_type(&args.value, &existing.type_)?;
    let rule_id = args
        .rule_id
        .unwrap_or_else(|| default_rule_id(&args.name, &when));

    let spinner = create_spinner_if(!json, format!("Setting rule on {}...", args.name.bold()));
    let signal = set_signal_rule(
        client,
        configs,
        owner,
        args.name.clone(),
        rule_id,
        expression,
        value,
    )
    .await?;

    if let Some(sp) = spinner {
        sp.finish_with_message(format!("Updated flag {}", args.name.bold()));
    } else {
        println!("{}", serde_json::to_string_pretty(&signal)?);
    }
    Ok(())
}

async fn delete_command(
    client: &reqwest::Client,
    configs: &Configs,
    owner: String,
    args: DeleteArgs,
    json: bool,
) -> Result<()> {
    let spinner = create_spinner_if(!json, format!("Deleting {}...", args.name.bold()));
    let signal = delete_signal(client, configs, owner, args.name.clone()).await?;

    if let Some(sp) = spinner {
        sp.finish_with_message(format!("Deleted flag {}", args.name.bold()));
    } else {
        println!("{}", serde_json::to_string_pretty(&signal)?);
    }
    Ok(())
}

async fn unset_command(
    client: &reqwest::Client,
    configs: &Configs,
    owner: String,
    args: UnsetArgs,
    json: bool,
) -> Result<()> {
    let spinner = create_spinner_if(!json, format!("Removing rule from {}...", args.name.bold()));
    let signal = unset_signal_rule(
        client,
        configs,
        owner,
        args.name.clone(),
        args.rule_id.clone(),
    )
    .await?;

    if let Some(sp) = spinner {
        sp.finish_with_message(format!("Updated flag {}", args.name.bold()));
    } else {
        println!("{}", serde_json::to_string_pretty(&signal)?);
    }
    Ok(())
}

fn default_rule_id(name: &str, when: &str) -> String {
    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
    };
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    when.hash(&mut hasher);
    format!("rule-{:x}", hasher.finish())
}

fn print_flags_tree(signals: &[crate::gql::signals::signals::SignalsSignals], full: bool) {
    for (index, signal) in signals.iter().enumerate() {
        if index > 0 {
            println!();
        }

        let rules = signal.rules.as_array().map(Vec::as_slice).unwrap_or(&[]);
        println!(
            "{} {} {}",
            signal.name.bold(),
            format!("({:?})", signal.type_).dimmed(),
            format!("v{}", signal.version).dimmed(),
        );
        println!(
            "  {} {}",
            "value".dimmed(),
            format_value_for_display(&signal.default)
        );

        if rules.is_empty() {
            if full {
                println!("  {} {}", "rules".dimmed(), "none".dimmed());
            }
            continue;
        }

        for (rule_index, rule) in rules.iter().enumerate() {
            let is_last = rule_index + 1 == rules.len();
            let branch = if is_last { "└─" } else { "├─" };
            let child_prefix = if is_last { "  " } else { "│ " };
            let rule_id = rule
                .get("id")
                .or_else(|| rule.get("ruleId"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("(no id)");
            let expression = rule
                .get("when")
                .or_else(|| rule.get("expression"))
                .map(format_radar_expression)
                .unwrap_or_else(|| "(no condition)".to_string());
            let value = rule_value(rule)
                .map(format_value_for_display)
                .unwrap_or_else(|| "(no value)".dimmed().to_string());

            println!("  {} {}", branch.dimmed(), expression.bold());
            if full {
                println!("  {}  {} {}", child_prefix.dimmed(), "id".dimmed(), rule_id);
            }
            println!(
                "  {}  {} {}",
                child_prefix.dimmed(),
                "value".dimmed(),
                value
            );
        }
    }
}

fn rule_value(rule: &serde_json::Value) -> Option<&serde_json::Value> {
    rule.get("source")
        .and_then(|source| {
            let source_type = source.get("type").and_then(serde_json::Value::as_str);
            if source_type == Some("literal") {
                source.get("value")
            } else {
                None
            }
        })
        .or_else(|| rule.get("value"))
        .or_else(|| rule.get("then"))
}

fn format_value_for_display(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone().green().to_string(),
        serde_json::Value::Number(value) => value.to_string().cyan().to_string(),
        serde_json::Value::Bool(value) => value.to_string().yellow().to_string(),
        serde_json::Value::Null => "null".magenta().to_string(),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            format!("`{}`", compact_json(value)).cyan().to_string()
        }
    }
}

fn compact_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
        }
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) | serde_json::Value::Null => {
            value.to_string()
        }
    }
}

fn format_radar_expression(value: &serde_json::Value) -> String {
    if let Some(items) = value.get("and").and_then(serde_json::Value::as_array) {
        return items
            .iter()
            .map(format_radar_expression)
            .collect::<Vec<_>>()
            .join(" AND ");
    }

    if let Some(items) = value.get("or").and_then(serde_json::Value::as_array) {
        return format!(
            "({})",
            items
                .iter()
                .map(format_radar_expression)
                .collect::<Vec<_>>()
                .join(" OR ")
        );
    }

    if let Some(inner) = value.get("not") {
        return format!("NOT ({})", format_radar_expression(inner));
    }

    let Some(attr) = value.get("attr").and_then(serde_json::Value::as_str) else {
        if let Some(bucket) = value.get("bucket").and_then(serde_json::Value::as_object) {
            let attr = bucket
                .get("attr")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("(missing-attr)");
            let salt = bucket
                .get("salt")
                .and_then(serde_json::Value::as_str)
                .map(|salt| format!(", salt: {salt}"))
                .unwrap_or_default();
            let op = value
                .get("op")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            let symbol = match op {
                "lt" => "<",
                "lte" => "<=",
                "gt" => ">",
                "gte" => ">=",
                _ => op,
            };
            let comparison = value
                .get("value")
                .map(format_radar_value)
                .unwrap_or_else(|| "(missing-value)".to_string());
            return format!("bucket(:{}{}) {} {}", attr, salt, symbol, comparison);
        }
        return compact_json(value);
    };
    let Some(op) = value.get("op").and_then(serde_json::Value::as_str) else {
        return compact_json(value);
    };

    match op {
        "in_list" | "not_in_list" => {
            let symbol = if op == "in_list" { "IN" } else { "NOT IN" };
            let list = value
                .get("list")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("(missing-list)");
            let suffix =
                if value.get("match").and_then(serde_json::Value::as_str) == Some("substring") {
                    " (substring)"
                } else {
                    ""
                };
            format!(":{} {} @{}{}", attr, symbol, list, suffix)
        }
        "matches" => {
            let pattern = value
                .get("value")
                .map(format_radar_value)
                .unwrap_or_else(|| "(missing-value)".to_string());
            format!(":{} matches /{}/", attr, pattern)
        }
        "contains" | "not_contains" => {
            let symbol = if op == "contains" {
                "contains"
            } else {
                "not contains"
            };
            let comparison = value
                .get("value")
                .map(format_radar_value)
                .unwrap_or_else(|| "(missing-value)".to_string());
            format!(":{} {} {:?}", attr, symbol, comparison)
        }
        _ => {
            let symbol = match op {
                "eq" => "==",
                "neq" => "!=",
                "gt" => ">",
                "lt" => "<",
                "gte" => ">=",
                "lte" => "<=",
                _ => op,
            };
            let comparison = value
                .get("value")
                .map(format_radar_value)
                .unwrap_or_else(|| "(missing-value)".to_string());
            format!(":{} {} {}", attr, symbol, comparison)
        }
    }
}

fn format_radar_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) | serde_json::Value::Null => {
            value.to_string()
        }
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => compact_json(value),
    }
}

#[cfg(test)]
mod flag_list_tests {
    use super::*;

    #[test]
    fn formats_json_defaults_compactly_for_tree_values() {
        let value = serde_json::json!({ "theme": "dark", "rollout": 10 });
        assert_eq!(compact_json(&value), r#"{"rollout":10,"theme":"dark"}"#);
    }

    #[test]
    fn formats_radar_rule_expression() {
        let expression = serde_json::json!({
            "and": [
                { "attr": "workspace_age_hours", "op": "lt", "value": 24 },
                { "attr": "workspace_plan", "op": "eq", "value": "free" },
            ],
        });
        assert_eq!(
            format_radar_expression(&expression),
            ":workspace_age_hours < 24 AND :workspace_plan == free"
        );
    }

    #[test]
    fn reads_literal_rule_source_value() {
        let rule = serde_json::json!({
            "id": "enterprise-on",
            "expression": { "attr": "plan", "op": "eq", "value": "enterprise" },
            "source": { "type": "literal", "value": true },
        });
        assert_eq!(rule_value(&rule), Some(&serde_json::Value::Bool(true)));
    }

    #[test]
    fn accepts_project_scope_after_subcommand() {
        let args = Args::try_parse_from(["railway flag", "list", "--scope", "project:project-id"])
            .expect("--scope should be accepted after a subcommand");

        assert_eq!(args.scope.as_deref(), Some("project:project-id"));
    }
}
