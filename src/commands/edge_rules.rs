use std::fs;
use std::io::{self, IsTerminal, Read, Write};

use anyhow::anyhow;
use serde::Serialize;
use serde_json::{Value, json};

use crate::controllers::edge::{ResourceRef, fetch_domains, has_public_domain, print_field};
use crate::controllers::project::{ServiceContext, resolve_service_context};
use crate::controllers::variable_edit::{
    create_private_file, edit_file_and_cleanup, temp_edit_path,
};
use crate::errors::RailwayError;
use crate::util::prompt::prompt_confirm_with_default;

use super::*;

const DOCS_URL: &str = "https://docs.railway.com/networking/edge-rules";

type RemoteDiagnostic =
    queries::validate_service_edge_rules::ValidateServiceEdgeRulesValidateServiceEdgeRules;

/// Manage edge rules for a service
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway edge-rules list --service web\n  railway edge-rules get > rules.json\n  railway edge-rules validate --file rules.json\n  railway edge-rules set --file rules.json\n  cat rules.json | railway edge-rules set --json\n  railway edge-rules edit\n  railway edge-rules disable 2\n  railway edge-rules remove rul_abc123 --yes\n  railway edge-rules clear --yes\n\nAutomation notes:\n  Edge rules are service-scoped and require an applied public domain and a plan with an edge-rule allowance.\n  `set` replaces the whole ruleset. The API validates it first and writes nothing when there are diagnostics; `validate` runs the same check without saving.\n  Rules run in priority order (lowest first). RULE arguments accept a rule ID (rul_...) or its position from `list`.\n  Ruleset JSON format: https://docs.railway.com/networking/edge-rules"
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Service name or ID (defaults to linked service)
    #[clap(short, long, global = true)]
    service: Option<String>,

    /// Environment to use (defaults to linked environment)
    #[clap(short, long, global = true)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID", global = true)]
    project: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,
}

#[derive(Parser)]
enum Commands {
    /// List edge rules in evaluation order
    List,

    /// Print the raw ruleset JSON
    Get,

    /// Replace the whole ruleset from a file or stdin
    Set(RulesetInputArgs),

    /// Validate a ruleset without saving it
    Validate(RulesetInputArgs),

    /// Edit the ruleset JSON in $EDITOR
    Edit(EditArgs),

    /// Remove all edge rules
    Clear(ConfirmArgs),

    /// Enable a rule
    Enable(RuleArgs),

    /// Disable a rule without removing it
    Disable(RuleArgs),

    /// Remove a single rule
    Remove(RemoveArgs),
}

#[derive(Parser)]
struct RulesetInputArgs {
    /// Ruleset JSON file, or - for stdin (piped stdin is read when omitted)
    #[clap(short, long, value_name = "PATH")]
    file: Option<String>,
}

#[derive(Parser)]
struct EditArgs {
    /// Save without asking for confirmation
    #[clap(short = 'y', long)]
    yes: bool,
}

#[derive(Parser)]
struct ConfirmArgs {
    /// Skip the confirmation prompt
    #[clap(short = 'y', long)]
    yes: bool,
}

#[derive(Parser)]
struct RuleArgs {
    /// Rule ID (rul_...) or its 1-based position from `list`
    #[clap(value_name = "RULE")]
    rule: String,
}

#[derive(Parser)]
struct RemoveArgs {
    /// Rule ID (rul_...) or its 1-based position from `list`
    #[clap(value_name = "RULE")]
    rule: String,

    /// Skip the confirmation prompt
    #[clap(short = 'y', long)]
    yes: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let Args {
        command,
        service,
        environment,
        project,
        json,
    } = args;

    crate::util::reporter::set_mode(json);
    let ctx = resolve_service_context(project, service, environment).await?;

    match command {
        Commands::List => list(&ctx, json).await,
        Commands::Get => get(&ctx).await,
        Commands::Set(input) => set(&ctx, input.file.as_deref(), json).await,
        Commands::Validate(input) => validate(&ctx, input.file.as_deref(), json).await,
        Commands::Edit(edit_args) => edit(&ctx, edit_args.yes, json).await,
        Commands::Clear(confirm) => clear(&ctx, confirm.yes, json).await,
        Commands::Enable(rule) => set_rule_enabled(&ctx, &rule.rule, true, json).await,
        Commands::Disable(rule) => set_rule_enabled(&ctx, &rule.rule, false, json).await,
        Commands::Remove(remove_args) => {
            remove(&ctx, &remove_args.rule, remove_args.yes, json).await
        }
    }
}

async fn list(ctx: &ServiceContext, json: bool) -> Result<()> {
    let domains = fetch_domains(ctx).await?;
    let available = has_public_domain(&domains);
    let ruleset = if available {
        fetch_ruleset(ctx).await?
    } else {
        None
    };
    let status = build_status(ctx, available, ruleset);

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        print_status(&status);
    }
    Ok(())
}

async fn get(ctx: &ServiceContext) -> Result<()> {
    let ruleset = fetch_ruleset(ctx).await?.unwrap_or_else(empty_ruleset);
    println!("{}", serde_json::to_string_pretty(&ruleset)?);
    Ok(())
}

async fn set(ctx: &ServiceContext, file: Option<&str>, json: bool) -> Result<()> {
    let ruleset = read_ruleset_input(file)?;
    let domains = fetch_domains(ctx).await?;
    ensure_available(&domains)?;

    reject_invalid(validate_remote(ctx, &ruleset).await?, json)?;
    let stored = write_ruleset(ctx, Some(&ruleset)).await?;

    let status = build_status(ctx, true, stored);
    let message = format!(
        "Saved {} for service {} in environment {}.",
        count_label(status.rule_count),
        ctx.service_name.bold(),
        ctx.environment_name.bold()
    );
    emit(status, "set", json, &message)
}

async fn validate(ctx: &ServiceContext, file: Option<&str>, json: bool) -> Result<()> {
    let ruleset = read_ruleset_input(file)?;
    let diagnostics = validate_remote(ctx, &ruleset).await?;

    if diagnostics.is_empty() {
        let rule_count = rules_of(&ruleset).len();
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "valid": true,
                    "ruleCount": rule_count,
                    "diagnostics": [],
                }))?
            );
        } else {
            println!(
                "{} {}",
                "Ruleset is valid:".green().bold(),
                count_label(rule_count)
            );
        }
        return Ok(());
    }

    reject_invalid(diagnostics, json)
}

async fn edit(ctx: &ServiceContext, yes: bool, json: bool) -> Result<()> {
    if !io::stdin().is_terminal()
        && std::env::var_os("EDITOR").is_none()
        && std::env::var_os("VISUAL").is_none()
    {
        bail!(
            "edge-rules edit requires a TTY (or set $EDITOR). For automation, use `railway edge-rules set --file <path>`."
        );
    }

    let domains = fetch_domains(ctx).await?;
    ensure_available(&domains)?;
    let current = fetch_ruleset(ctx).await?.unwrap_or_else(empty_ruleset);
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();

    let mut text = serde_json::to_string_pretty(&current)?;
    text.push('\n');

    loop {
        let path = temp_edit_path("edge-rules", &ctx.service_name, "json");
        create_private_file(&path)?
            .write_all(text.as_bytes())
            .with_context(|| format!("Failed to write {}", path.display()))?;

        eprintln!(
            "{} {}",
            "Editing".dimmed(),
            path.display().to_string().cyan()
        );
        eprintln!(
            "{}",
            "Opening $EDITOR — save and quit to validate, abort to cancel.".dimmed()
        );
        let edited = edit_file_and_cleanup(&path)?;

        let failure = match parse_ruleset_text(&edited) {
            Ok(ruleset) if ruleset == current => {
                println!("No changes.");
                return Ok(());
            }
            Ok(ruleset) => {
                let diagnostics = validate_remote(ctx, &ruleset).await?;
                if diagnostics.is_empty() {
                    return save_edited(ctx, ruleset, yes, json).await;
                }
                if !json {
                    eprintln!("{}", format_diagnostics(&diagnostics));
                }
                invalid_rules_error(diagnostics)
            }
            Err(error) => {
                if !json {
                    eprintln!("{} {error:#}", "error:".red().bold());
                }
                error
            }
        };

        if !interactive || !prompt_confirm_with_default("Re-open the editor to fix it?", true)? {
            return Err(failure);
        }
        text = edited;
    }
}

async fn save_edited(ctx: &ServiceContext, ruleset: Value, yes: bool, json: bool) -> Result<()> {
    if !yes {
        if !io::stdout().is_terminal() {
            bail!(
                "Cannot prompt for confirmation in non-interactive mode. Re-run with --yes to save without confirming."
            );
        }
        let preview = build_status(ctx, true, Some(ruleset.clone()));
        println!();
        print_rules(&preview.rules);
        println!();
        let prompt = format!(
            "Save {} to service {} in environment {}?",
            count_label(preview.rule_count),
            ctx.service_name,
            ctx.environment_name
        );
        // Default No — saving the editor buffer alone is not enough.
        if !prompt_confirm_with_default(&prompt, false)? {
            bail!("No changes applied.");
        }
    }

    let stored = write_ruleset(ctx, Some(&ruleset)).await?;
    let status = build_status(ctx, true, stored);
    let message = format!(
        "Saved {} for service {} in environment {}.",
        count_label(status.rule_count),
        ctx.service_name.bold(),
        ctx.environment_name.bold()
    );
    emit(status, "edit", json, &message)
}

async fn clear(ctx: &ServiceContext, yes: bool, json: bool) -> Result<()> {
    let domains = fetch_domains(ctx).await?;
    ensure_available(&domains)?;

    if !yes {
        confirm_or_bail(&format!(
            "Remove all edge rules from service {} in environment {}?",
            ctx.service_name, ctx.environment_name
        ))?;
    }

    let stored = write_ruleset(ctx, None).await?;
    let status = build_status(ctx, true, stored);
    let message = format!(
        "Cleared edge rules for service {} in environment {}.",
        ctx.service_name.bold(),
        ctx.environment_name.bold()
    );
    emit(status, "clear", json, &message)
}

async fn set_rule_enabled(
    ctx: &ServiceContext,
    selector: &str,
    enabled: bool,
    json: bool,
) -> Result<()> {
    let domains = fetch_domains(ctx).await?;
    ensure_available(&domains)?;
    let mut ruleset = fetch_ruleset(ctx)
        .await?
        .context("No edge rules are configured for this service.")?;

    let index = find_rule_index(rules_of(&ruleset), selector)?;
    let label = rule_label(&rules_of(&ruleset)[index]);
    let action = if enabled { "enable" } else { "disable" };

    let rule = ruleset
        .get_mut("rules")
        .and_then(Value::as_array_mut)
        .and_then(|rules| rules.get_mut(index))
        .and_then(Value::as_object_mut)
        .context("Stored rule is not a JSON object")?;
    let was_enabled = rule.get("enabled").and_then(Value::as_bool).unwrap_or(true);

    if was_enabled == enabled {
        let message = format!(
            "Rule {label} is already {}.",
            if enabled { "enabled" } else { "disabled" }
        );
        return emit(
            build_status(ctx, true, Some(ruleset)),
            action,
            json,
            &message,
        );
    }
    rule.insert("enabled".to_string(), Value::Bool(enabled));

    reject_invalid(validate_remote(ctx, &ruleset).await?, json)?;
    let stored = write_ruleset(ctx, Some(&ruleset)).await?;

    let status = build_status(ctx, true, stored);
    let message = format!(
        "{} rule {label} for service {} in environment {}.",
        if enabled { "Enabled" } else { "Disabled" },
        ctx.service_name.bold(),
        ctx.environment_name.bold()
    );
    emit(status, action, json, &message)
}

async fn remove(ctx: &ServiceContext, selector: &str, yes: bool, json: bool) -> Result<()> {
    let domains = fetch_domains(ctx).await?;
    ensure_available(&domains)?;
    let mut ruleset = fetch_ruleset(ctx)
        .await?
        .context("No edge rules are configured for this service.")?;

    let index = find_rule_index(rules_of(&ruleset), selector)?;
    let label = rule_label(&rules_of(&ruleset)[index]);

    if !yes {
        confirm_or_bail(&format!(
            "Remove rule {label} from service {} in environment {}?",
            ctx.service_name, ctx.environment_name
        ))?;
    }

    ruleset
        .get_mut("rules")
        .and_then(Value::as_array_mut)
        .context("Stored ruleset has no rules array")?
        .remove(index);

    reject_invalid(validate_remote(ctx, &ruleset).await?, json)?;
    let stored = write_ruleset(ctx, Some(&ruleset)).await?;

    let status = build_status(ctx, true, stored);
    let message = format!(
        "Removed rule {label} from service {} in environment {}.",
        ctx.service_name.bold(),
        ctx.environment_name.bold()
    );
    emit(status, "remove", json, &message)
}

// API calls.

async fn fetch_ruleset(ctx: &ServiceContext) -> Result<Option<Value>> {
    let response = post_graphql::<queries::ServiceEdgeRules, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::service_edge_rules::Variables {
            environment_id: ctx.environment_id.clone(),
            service_id: ctx.service_id.clone(),
        },
    )
    .await?;
    Ok(response
        .service_instance
        .edge_config
        .and_then(|config| config.edge_rules)
        .filter(|rules| !rules.is_null()))
}

async fn validate_remote(ctx: &ServiceContext, ruleset: &Value) -> Result<Vec<Diagnostic>> {
    let response = post_graphql::<queries::ValidateServiceEdgeRules, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        queries::validate_service_edge_rules::Variables {
            input: queries::validate_service_edge_rules::ValidateServiceEdgeRulesInput {
                service_id: ctx.service_id.clone(),
                environment_id: ctx.environment_id.clone(),
                edge_rules: ruleset.clone(),
            },
        },
    )
    .await?;
    Ok(response
        .validate_service_edge_rules
        .into_iter()
        .map(Diagnostic::from)
        .collect())
}

/// Whole-section replace; `None` clears every rule. Returns the ruleset as
/// stored, which carries the server-assigned `rul_` ids for new rules.
async fn write_ruleset(ctx: &ServiceContext, ruleset: Option<&Value>) -> Result<Option<Value>> {
    let response = post_graphql::<mutations::UpdateServiceEdgeRules, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        mutations::update_service_edge_rules::Variables {
            input: mutations::update_service_edge_rules::UpdateServiceEdgeRulesInput {
                service_id: ctx.service_id.clone(),
                environment_id: ctx.environment_id.clone(),
                // The contract is "null clears": put the null on the wire rather
                // than rely on the server coalescing an omitted field.
                edge_rules: Some(ruleset.cloned().unwrap_or(Value::Null)),
            },
        },
    )
    .await?;
    Ok(response
        .update_service_edge_rules
        .edge_rules
        .filter(|rules| !rules.is_null()))
}

// Input handling.

fn read_ruleset_input(file: Option<&str>) -> Result<Value> {
    let text = match file {
        Some("-") => read_stdin()?,
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("Failed to read ruleset from {path}"))?,
        None if !io::stdin().is_terminal() => read_stdin()?,
        None => bail!(
            "No ruleset provided. Pass --file <path>, or pipe the ruleset JSON on stdin. Format reference: {DOCS_URL}"
        ),
    };
    parse_ruleset_text(&text)
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("Failed to read stdin")?;
    Ok(buf)
}

fn parse_ruleset_text(text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        bail!("Ruleset is empty. Expected a JSON object with \"version\" and \"rules\".");
    }
    let value: Value = serde_json::from_str(text)
        .map_err(|error| anyhow!("Ruleset is not valid JSON: {error}"))?;
    normalize_ruleset(value)
}

/// Accept the wire shape as-is, and wrap a bare array of rules as version 1.
fn normalize_ruleset(value: Value) -> Result<Value> {
    match value {
        Value::Array(rules) => Ok(json!({ "version": 1, "rules": rules })),
        Value::Object(_) => Ok(value),
        _ => bail!(
            "Ruleset must be a JSON object with \"version\" and \"rules\", or a bare array of rules. Format reference: {DOCS_URL}"
        ),
    }
}

fn empty_ruleset() -> Value {
    json!({ "version": 1, "rules": [] })
}

fn ensure_available(domains: &queries::domains::DomainsDomains) -> Result<()> {
    if has_public_domain(domains) {
        Ok(())
    } else {
        bail!(
            "Edge rules require an applied public domain. Add a domain with `railway domain` first."
        )
    }
}

fn confirm_or_bail(prompt: &str) -> Result<()> {
    if !io::stdout().is_terminal() {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    }
    if !prompt_confirm_with_default(prompt, false)? {
        bail!("Aborted.");
    }
    Ok(())
}

// Diagnostics.

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Diagnostic {
    code: String,
    message: String,
    path: String,
}

impl From<RemoteDiagnostic> for Diagnostic {
    fn from(diagnostic: RemoteDiagnostic) -> Self {
        Self {
            code: diagnostic.code,
            message: diagnostic.message,
            path: diagnostic.path,
        }
    }
}

fn reject_invalid(diagnostics: Vec<Diagnostic>, json: bool) -> Result<()> {
    if diagnostics.is_empty() {
        return Ok(());
    }
    if !json {
        eprintln!("{}", format_diagnostics(&diagnostics));
    }
    Err(invalid_rules_error(diagnostics))
}

fn invalid_rules_error(diagnostics: Vec<Diagnostic>) -> anyhow::Error {
    let diagnostics = diagnostics
        .into_iter()
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code,
                "message": diagnostic.message,
                "path": diagnostic.path,
            })
        })
        .collect();
    RailwayError::InvalidEdgeRules { diagnostics }.into()
}

/// One aligned line per diagnostic: path, code, message.
fn format_diagnostics(diagnostics: &[Diagnostic]) -> String {
    let path_of = |diagnostic: &Diagnostic| {
        if diagnostic.path.is_empty() {
            "(ruleset)".to_string()
        } else {
            diagnostic.path.clone()
        }
    };
    let path_width = diagnostics
        .iter()
        .map(|d| path_of(d).len())
        .max()
        .unwrap_or(0);
    let code_width = diagnostics.iter().map(|d| d.code.len()).max().unwrap_or(0);

    let mut out = String::new();
    for diagnostic in diagnostics {
        out.push_str(&format!(
            "  {:<path_width$}  {:<code_width$}  {}\n",
            path_of(diagnostic),
            diagnostic.code,
            diagnostic.message
        ));
    }
    out.trim_end().to_string()
}

// Ruleset model.

fn rules_of(ruleset: &Value) -> &[Value] {
    ruleset
        .get("rules")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Evaluation order: priority ascending, missing priority last, array index
/// as the tie-breaker — the same sort the engine applies.
fn ordered_rule_indices(rules: &[Value]) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..rules.len()).collect();
    indices.sort_by_key(|&index| {
        (
            rules[index]
                .get("priority")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX),
            index,
        )
    });
    indices
}

/// Resolve a `rul_` id or a 1-based position (in evaluation order) to the
/// rule's index in the stored array.
fn find_rule_index(rules: &[Value], selector: &str) -> Result<usize> {
    let selector = selector.trim();
    if let Ok(position) = selector.parse::<usize>() {
        let order = ordered_rule_indices(rules);
        return position
            .checked_sub(1)
            .and_then(|index| order.get(index).copied())
            .with_context(|| {
                format!(
                    "No rule at position {position}. `railway edge-rules list` shows {}.",
                    count_label(rules.len())
                )
            });
    }
    rules
        .iter()
        .position(|rule| rule.get("id").and_then(Value::as_str) == Some(selector))
        .with_context(|| {
            format!(
                "No rule with ID {selector}. Run `railway edge-rules list` to see rule IDs and positions."
            )
        })
}

fn rule_label(rule: &Value) -> String {
    let id = rule.get("id").and_then(Value::as_str);
    let description = rule
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (id, description) {
        (Some(id), Some(description)) => format!("{id} ({description})"),
        (Some(id), None) => id.to_string(),
        (None, Some(description)) => format!("\"{description}\""),
        (None, None) => "(unnamed)".to_string(),
    }
}

fn count_label(count: usize) -> String {
    if count == 1 {
        "1 rule".to_string()
    } else {
        format!("{count} rules")
    }
}

// Human summaries, mirroring the dashboard's rule list.

fn attr_label(attr: &str) -> &str {
    match attr {
        "ipv4.src" => "Client IP",
        "http.host" => "Host",
        "http.path" => "Path",
        "http.header" => "Header",
        other => other,
    }
}

fn op_label(op: &str) -> &str {
    match op {
        "eq" => "is",
        "neq" => "is not",
        "in" => "is in",
        "not_in" => "is not in",
        "contains" => "contains",
        "matches" => "matches",
        other => other,
    }
}

fn summarize_clause(clause: &Value) -> String {
    let attr = clause.get("attr").and_then(Value::as_str).unwrap_or("?");
    let op = clause.get("op").and_then(Value::as_str).unwrap_or("?");
    let key = clause
        .get("key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty());
    let subject = match (attr, key) {
        ("http.header", Some(key)) => format!("{} {key}", attr_label(attr)),
        _ => attr_label(attr).to_string(),
    };
    let value = match clause.get("value") {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(value) => value.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Some(other) => other.to_string(),
        None => "…".to_string(),
    };
    format!("{subject} {} {value}", op_label(op))
}

fn summarize_expr(expr: &Value, top_level: bool) -> String {
    let Some(object) = expr.as_object() else {
        return "(invalid expression)".to_string();
    };

    if let Some(inner) = object.get("not") {
        let inner_text = summarize_expr(inner, false);
        let negates_or_group = inner
            .get("or")
            .and_then(Value::as_array)
            .is_some_and(|children| children.len() > 1);
        return if negates_or_group {
            format!("none of {inner_text}")
        } else {
            format!("not {inner_text}")
        };
    }

    for (combinator, joiner) in [("and", " and "), ("or", " or ")] {
        if let Some(children) = object.get(combinator).and_then(Value::as_array) {
            let inner = children
                .iter()
                .map(|child| summarize_expr(child, false))
                .collect::<Vec<_>>()
                .join(joiner);
            return if children.len() > 1 && !top_level {
                format!("({inner})")
            } else {
                inner
            };
        }
    }

    if object.contains_key("attr") {
        return summarize_clause(expr);
    }
    "(invalid expression)".to_string()
}

fn action_name(then: &Value) -> &str {
    then.get("action").and_then(Value::as_str).unwrap_or("?")
}

fn summarize_action(then: &Value) -> String {
    let param = |name: &str| then.get("params").and_then(|params| params.get(name));
    match action_name(then) {
        "block" => match param("status").and_then(Value::as_u64) {
            Some(status) if status != 403 => format!("Block ({status})"),
            _ => "Block".to_string(),
        },
        "allow" => "Allow".to_string(),
        "challenge" => "Challenge".to_string(),
        "redirect" => {
            let target = param("location")
                .or_else(|| param("host"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|target| !target.is_empty());
            let status = param("status").and_then(Value::as_u64);
            match (target, status) {
                (Some(target), Some(status)) if status != 302 => {
                    format!("Redirect to {target} ({status})")
                }
                (Some(target), _) => format!("Redirect to {target}"),
                (None, _) => "Redirect".to_string(),
            }
        }
        "cache_override" => {
            if param("bypass").and_then(Value::as_bool) == Some(true) {
                "Bypass cache".to_string()
            } else if let Some(ttl) = param("ttl_seconds").and_then(Value::as_u64) {
                format!("Cache for {ttl}s")
            } else {
                "Override cache".to_string()
            }
        }
        "?" => "(missing action)".to_string(),
        other => other.to_string(),
    }
}

// Output.

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RulesStatusOutput {
    service: ResourceRef,
    environment: ResourceRef,
    available: bool,
    rule_count: usize,
    rules: Vec<RuleView>,
    ruleset: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleView {
    position: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<u64>,
    action: String,
    action_summary: String,
    condition_summary: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActionOutput {
    action: &'static str,
    #[serde(flatten)]
    status: RulesStatusOutput,
}

fn build_status(
    ctx: &ServiceContext,
    available: bool,
    ruleset: Option<Value>,
) -> RulesStatusOutput {
    let rules = rule_views(ruleset.as_ref());
    RulesStatusOutput {
        service: ResourceRef::service(ctx),
        environment: ResourceRef::environment(ctx),
        available,
        rule_count: rules.len(),
        rules,
        ruleset,
    }
}

fn rule_views(ruleset: Option<&Value>) -> Vec<RuleView> {
    let rules = ruleset.map(rules_of).unwrap_or(&[]);
    ordered_rule_indices(rules)
        .into_iter()
        .enumerate()
        .map(|(position, index)| rule_view(position + 1, &rules[index]))
        .collect()
}

fn rule_view(position: usize, rule: &Value) -> RuleView {
    let text = |name: &str| {
        rule.get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    RuleView {
        position,
        id: text("id"),
        description: text("description"),
        enabled: rule.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        priority: rule.get("priority").and_then(Value::as_u64),
        action: rule.get("then").map(action_name).unwrap_or("?").to_string(),
        action_summary: rule
            .get("then")
            .map(summarize_action)
            .unwrap_or_else(|| "(missing action)".to_string()),
        condition_summary: rule
            .get("if")
            .map(|expr| summarize_expr(expr, true))
            .unwrap_or_else(|| "(missing condition)".to_string()),
    }
}

fn emit(status: RulesStatusOutput, action: &'static str, json: bool, message: &str) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ActionOutput { action, status })?
        );
    } else {
        println!("{message}");
        print_status(&status);
    }
    Ok(())
}

fn print_status(status: &RulesStatusOutput) {
    println!("{}", "Edge rules".bold());
    println!();
    print_field("Service:", &status.service.name.green().bold());
    print_field("Environment:", &status.environment.name.blue().bold());

    if !status.available {
        print_field("Status:", &"unavailable".yellow().bold());
        print_field(
            "Message:",
            &"Add a public domain with `railway domain` before configuring edge rules.",
        );
        return;
    }

    print_field("Rules:", &count_label(status.rule_count));
    println!();
    if status.rules.is_empty() {
        println!(
            "{}",
            "No edge rules configured. Run `railway edge-rules edit` to add one.".dimmed()
        );
        return;
    }
    print_rules(&status.rules);
}

fn print_rules(rules: &[RuleView]) {
    for rule in rules {
        let title = rule.description.as_deref().unwrap_or("Untitled rule");
        let mut header = format!("{}. {}", rule.position, title.bold());
        if let Some(id) = &rule.id {
            header.push_str(&format!("  {}", id.dimmed()));
        }
        if !rule.enabled {
            header.push_str(&format!("  {}", "off".yellow()));
        }
        println!("{header}");
        println!("   {} {}", "if  ".dimmed(), rule.condition_summary);
        println!("   {} {}", "then".dimmed(), rule.action_summary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_subcommands_and_selectors() {
        assert!(matches!(
            Args::parse_from(["edge-rules", "list"]).command,
            Commands::List
        ));
        assert!(matches!(
            Args::parse_from(["edge-rules", "get"]).command,
            Commands::Get
        ));
        let args = Args::parse_from([
            "edge-rules",
            "--project",
            "project-id",
            "--environment",
            "production",
            "--service",
            "web",
            "--json",
            "list",
        ]);
        assert_eq!(args.project.as_deref(), Some("project-id"));
        assert_eq!(args.environment.as_deref(), Some("production"));
        assert_eq!(args.service.as_deref(), Some("web"));
        assert!(args.json);

        let Commands::Set(input) =
            Args::parse_from(["edge-rules", "set", "--file", "rules.json"]).command
        else {
            panic!("expected set");
        };
        assert_eq!(input.file.as_deref(), Some("rules.json"));

        let Commands::Validate(input) =
            Args::parse_from(["edge-rules", "validate", "-f", "-"]).command
        else {
            panic!("expected validate");
        };
        assert_eq!(input.file.as_deref(), Some("-"));

        let Commands::Remove(remove) =
            Args::parse_from(["edge-rules", "remove", "rul_abc", "--yes"]).command
        else {
            panic!("expected remove");
        };
        assert_eq!(remove.rule, "rul_abc");
        assert!(remove.yes);

        let Commands::Edit(edit) = Args::parse_from(["edge-rules", "edit", "-y"]).command else {
            panic!("expected edit");
        };
        assert!(edit.yes);

        assert!(Args::try_parse_from(["edge-rules"]).is_err());
        assert!(Args::try_parse_from(["edge-rules", "enable"]).is_err());
    }

    #[test]
    fn normalizes_bare_rule_arrays_and_rejects_scalars() {
        let wrapped = normalize_ruleset(json!([{ "priority": 10 }])).unwrap();
        assert_eq!(
            wrapped,
            json!({ "version": 1, "rules": [{ "priority": 10 }] })
        );

        let object = json!({ "version": 1, "rules": [] });
        assert_eq!(normalize_ruleset(object.clone()).unwrap(), object);

        assert!(normalize_ruleset(json!("nope")).is_err());
        assert!(normalize_ruleset(json!(42)).is_err());
    }

    #[test]
    fn parse_ruleset_text_rejects_empty_and_invalid_json() {
        assert!(parse_ruleset_text("   \n").is_err());
        let error = parse_ruleset_text("{ not json").unwrap_err().to_string();
        assert!(error.starts_with("Ruleset is not valid JSON"), "{error}");
        assert_eq!(
            parse_ruleset_text("[]").unwrap(),
            json!({ "version": 1, "rules": [] })
        );
    }

    #[test]
    fn orders_rules_by_priority_then_index_with_missing_last() {
        let rules = vec![
            json!({ "id": "c", "priority": 30 }),
            json!({ "id": "a", "priority": 10 }),
            json!({ "id": "none" }),
            json!({ "id": "b", "priority": 10 }),
        ];
        assert_eq!(ordered_rule_indices(&rules), vec![1, 3, 0, 2]);
    }

    #[test]
    fn finds_rules_by_position_and_id() {
        let rules = vec![
            json!({ "id": "rul_c", "priority": 30 }),
            json!({ "id": "rul_a", "priority": 10 }),
        ];
        assert_eq!(find_rule_index(&rules, "1").unwrap(), 1);
        assert_eq!(find_rule_index(&rules, "2").unwrap(), 0);
        assert_eq!(find_rule_index(&rules, "rul_c").unwrap(), 0);
        assert_eq!(find_rule_index(&rules, " rul_a ").unwrap(), 1);

        let error = find_rule_index(&rules, "0").unwrap_err().to_string();
        assert!(error.contains("No rule at position 0"), "{error}");
        let error = find_rule_index(&rules, "3").unwrap_err().to_string();
        assert!(error.contains("shows 2 rules"), "{error}");
        let error = find_rule_index(&rules, "rul_zzz").unwrap_err().to_string();
        assert!(error.contains("No rule with ID rul_zzz"), "{error}");
    }

    #[test]
    fn summarizes_conditions_like_the_dashboard() {
        let docs_example = json!({
            "and": [
                { "attr": "http.path", "op": "matches", "value": "/admin*" },
                { "attr": "ipv4.src", "op": "in", "value": ["203.0.113.0/24", "198.51.100.7"] }
            ]
        });
        assert_eq!(
            summarize_expr(&docs_example, true),
            "Path matches /admin* and Client IP is in 203.0.113.0/24, 198.51.100.7"
        );

        let nested = json!({
            "or": [
                { "attr": "http.path", "op": "matches", "value": "/internal*" },
                { "not": { "attr": "http.header", "key": "x-client-type", "op": "eq", "value": "trusted" } }
            ]
        });
        assert_eq!(
            summarize_expr(&nested, true),
            "Path matches /internal* or not Header x-client-type is trusted"
        );

        let grouped = json!({
            "and": [
                { "attr": "http.host", "op": "eq", "value": "app.example.com" },
                { "or": [
                    { "attr": "http.path", "op": "eq", "value": "/a" },
                    { "attr": "http.path", "op": "eq", "value": "/b" }
                ] }
            ]
        });
        assert_eq!(
            summarize_expr(&grouped, true),
            "Host is app.example.com and (Path is /a or Path is /b)"
        );

        let none_of = json!({ "not": { "or": [
            { "attr": "http.path", "op": "eq", "value": "/a" },
            { "attr": "http.path", "op": "eq", "value": "/b" }
        ] } });
        assert_eq!(
            summarize_expr(&none_of, true),
            "none of (Path is /a or Path is /b)"
        );

        assert_eq!(
            summarize_expr(&json!("bogus"), true),
            "(invalid expression)"
        );
    }

    #[test]
    fn summarizes_actions() {
        assert_eq!(summarize_action(&json!({ "action": "block" })), "Block");
        assert_eq!(
            summarize_action(&json!({ "action": "block", "params": { "status": 403 } })),
            "Block"
        );
        assert_eq!(
            summarize_action(&json!({ "action": "block", "params": { "status": 429 } })),
            "Block (429)"
        );
        assert_eq!(summarize_action(&json!({ "action": "allow" })), "Allow");
        assert_eq!(
            summarize_action(&json!({ "action": "challenge" })),
            "Challenge"
        );
        assert_eq!(
            summarize_action(
                &json!({ "action": "redirect", "params": { "host": "www.example.com" } })
            ),
            "Redirect to www.example.com"
        );
        assert_eq!(
            summarize_action(&json!({
                "action": "redirect",
                "params": { "location": "https://example.com/new", "status": 301 }
            })),
            "Redirect to https://example.com/new (301)"
        );
        assert_eq!(
            summarize_action(&json!({ "action": "cache_override", "params": { "bypass": true } })),
            "Bypass cache"
        );
        assert_eq!(
            summarize_action(
                &json!({ "action": "cache_override", "params": { "ttl_seconds": 3600 } })
            ),
            "Cache for 3600s"
        );
        assert_eq!(summarize_action(&json!({ "action": "future" })), "future");
        assert_eq!(summarize_action(&json!({})), "(missing action)");
    }

    #[test]
    fn formats_diagnostics_in_aligned_columns() {
        let diagnostics = vec![
            Diagnostic {
                code: "invalid_status".to_string(),
                message: "block status must be 400-499, got 200".to_string(),
                path: "rules[0].then.params.status".to_string(),
            },
            Diagnostic {
                code: "too_many_rules".to_string(),
                message: "21 rules, max 20".to_string(),
                path: String::new(),
            },
        ];
        let rendered = format_diagnostics(&diagnostics);
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "  rules[0].then.params.status  invalid_status  block status must be 400-499, got 200"
        );
        assert_eq!(
            lines[1],
            "  (ruleset)                    too_many_rules  21 rules, max 20"
        );
    }

    #[test]
    fn invalid_rules_error_carries_diagnostics_as_details() {
        let error = invalid_rules_error(vec![Diagnostic {
            code: "missing_priority".to_string(),
            message: "priority is required".to_string(),
            path: "rules[1].priority".to_string(),
        }]);
        assert_eq!(error.to_string(), "Edge rules are invalid: 1 problem");
        let railway_error = error.downcast_ref::<RailwayError>().unwrap();
        assert_eq!(railway_error.code(), "INVALID_EDGE_RULES");
        let details = railway_error.details().unwrap();
        assert_eq!(details[0]["path"], "rules[1].priority");
        assert_eq!(details[0]["code"], "missing_priority");
    }

    #[test]
    fn rule_views_follow_evaluation_order_and_default_enabled() {
        let ruleset = json!({
            "version": 1,
            "rules": [
                {
                    "id": "rul_b",
                    "priority": 20,
                    "enabled": false,
                    "if": { "attr": "http.path", "op": "matches", "value": "/admin*" },
                    "then": { "action": "block", "params": { "status": 403, "body": "Forbidden" } }
                },
                {
                    "description": "Allow office access to admin",
                    "priority": 10,
                    "if": { "attr": "ipv4.src", "op": "eq", "value": "203.0.113.7" },
                    "then": { "action": "allow" }
                }
            ]
        });
        let views = rule_views(Some(&ruleset));
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].position, 1);
        assert_eq!(
            views[0].description.as_deref(),
            Some("Allow office access to admin")
        );
        assert!(views[0].enabled);
        assert_eq!(views[0].action, "allow");
        assert_eq!(views[0].condition_summary, "Client IP is 203.0.113.7");
        assert_eq!(views[1].position, 2);
        assert_eq!(views[1].id.as_deref(), Some("rul_b"));
        assert!(!views[1].enabled);
        assert_eq!(views[1].action_summary, "Block");

        assert!(rule_views(None).is_empty());
        assert!(rule_views(Some(&json!({ "version": 1 }))).is_empty());
    }

    #[test]
    fn status_output_uses_camel_case_and_null_ruleset() {
        let status = RulesStatusOutput {
            service: ResourceRef {
                id: "svc_123".to_string(),
                name: "web".to_string(),
            },
            environment: ResourceRef {
                id: "env_123".to_string(),
                name: "production".to_string(),
            },
            available: true,
            rule_count: 0,
            rules: Vec::new(),
            ruleset: None,
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["available"], true);
        assert_eq!(value["ruleCount"], 0);
        assert!(value["ruleset"].is_null());
        assert_eq!(value["rules"], json!([]));

        let action = serde_json::to_value(ActionOutput {
            action: "clear",
            status,
        })
        .unwrap();
        assert_eq!(action["action"], "clear");
        assert_eq!(action["service"]["name"], "web");
    }

    #[test]
    fn rule_view_json_omits_absent_optional_fields() {
        let view = rule_view(
            1,
            &json!({
                "priority": 10,
                "if": { "attr": "http.path", "op": "eq", "value": "/" },
                "then": { "action": "challenge" }
            }),
        );
        let value = serde_json::to_value(&view).unwrap();
        assert!(value.get("id").is_none());
        assert!(value.get("description").is_none());
        assert_eq!(value["priority"], 10);
        assert_eq!(value["actionSummary"], "Challenge");
        assert_eq!(value["conditionSummary"], "Path is /");
    }

    #[test]
    fn rule_labels_prefer_id_then_description() {
        assert_eq!(
            rule_label(&json!({ "id": "rul_a", "description": "Block bots" })),
            "rul_a (Block bots)"
        );
        assert_eq!(rule_label(&json!({ "id": "rul_a" })), "rul_a");
        assert_eq!(
            rule_label(&json!({ "description": "Block bots" })),
            "\"Block bots\""
        );
        assert_eq!(rule_label(&json!({})), "(unnamed)");
    }
}
