use std::{
    fs,
    io::{self, Read, Write},
    time::Instant,
};

use anyhow::{Context, anyhow, bail};
use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use graphql_parser::query::{Definition, OperationDefinition, Selection};
use is_terminal::IsTerminal;
use serde::Serialize;
use serde_json::{Value, json};

use crate::telemetry;

use super::*;

const MAX_OPERATION_SUMMARY_FIELDS: usize = 5;

/// Query the Railway public GraphQL API
#[derive(Parser, Debug)]
#[command(
    about = "Query the Railway public GraphQL API",
    long_about = "Query the Railway public GraphQL API.\n\nUse `railway api search <term>` and `railway api describe <name>` to inspect the schema before running a query."
)]
pub struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    #[command(flatten)]
    execute: ExecuteArgs,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Print the live GraphQL introspection schema
    Schema(SchemaArgs),
    /// Search GraphQL types and fields
    Search(SearchArgs),
    /// Describe a GraphQL type or field
    Describe(DescribeArgs),
}

#[derive(ClapArgs, Clone, Debug, Default)]
struct ExecuteArgs {
    /// GraphQL query or mutation document. If omitted, stdin is used when piped.
    #[arg(value_name = "QUERY", allow_hyphen_values = true)]
    query: Option<String>,

    /// Read the GraphQL document from a file. Use '-' to read from stdin.
    #[arg(short, long, value_name = "PATH")]
    file: Option<String>,

    /// JSON variables object, or @PATH to read JSON from a file. Use @- for stdin.
    #[arg(long, value_name = "JSON|@PATH")]
    variables: Option<String>,

    /// Set a typed variable as KEY=VALUE. VALUE is parsed as JSON when possible.
    #[arg(long = "var", value_name = "KEY=VALUE")]
    vars: Vec<String>,

    /// Set a string variable as KEY=VALUE.
    #[arg(long = "raw-var", value_name = "KEY=VALUE")]
    raw_vars: Vec<String>,

    /// GraphQL operation name to execute when the document contains multiple operations.
    #[arg(long, value_name = "NAME")]
    operation_name: Option<String>,

    /// Print compact JSON.
    #[arg(long)]
    compact: bool,

    /// Exit successfully even when the GraphQL response has an errors array.
    #[arg(long)]
    allow_errors: bool,
}

#[derive(ClapArgs, Debug)]
struct SchemaArgs {
    /// Print compact JSON.
    #[arg(long)]
    compact: bool,
}

#[derive(Clone, Debug, ValueEnum)]
enum SearchKind {
    All,
    Type,
    Query,
    Mutation,
    Subscription,
    Field,
    Input,
    Enum,
}

#[derive(ClapArgs, Debug)]
struct SearchArgs {
    /// Search term.
    term: String,

    /// Restrict search results to a kind.
    #[arg(long, value_enum, default_value_t = SearchKind::All)]
    kind: SearchKind,

    /// Maximum number of results to print.
    #[arg(long, default_value_t = 25, value_name = "N")]
    limit: usize,

    /// Print compact JSON.
    #[arg(long)]
    compact: bool,
}

#[derive(ClapArgs, Debug)]
struct DescribeArgs {
    /// Type name, root field name, or Parent.field.
    name: String,

    /// Print compact JSON.
    #[arg(long)]
    compact: bool,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Some(Commands::Schema(schema_args)) => run_schema(schema_args).await,
        Some(Commands::Search(search_args)) => run_search(search_args).await,
        Some(Commands::Describe(describe_args)) => run_describe(describe_args).await,
        None => run_execute(args.execute).await,
    }
}

async fn run_schema(args: SchemaArgs) -> Result<()> {
    let schema = fetch_live_schema().await?;
    print_json(&schema, args.compact)
}

async fn run_search(args: SearchArgs) -> Result<()> {
    let schema = fetch_live_schema().await?;
    let results = search_schema(&schema, &args.term, &args.kind, args.limit)?;
    print_json(&json!({ "results": results }), args.compact)
}

async fn run_describe(args: DescribeArgs) -> Result<()> {
    let schema = fetch_live_schema().await?;
    let descriptions = describe_schema_member(&schema, &args.name)?;
    print_json(&json!({ "matches": descriptions }), args.compact)
}

async fn run_execute(args: ExecuteArgs) -> Result<()> {
    let started = Instant::now();
    let query = resolve_query_source(args.query.as_deref(), args.file.as_deref(), false)?;
    let summary = operation_summary(&query.document, args.operation_name.as_deref());

    let result = execute_document(&args, &query).await;

    if let Some(summary) = summary {
        telemetry::send(telemetry::CliTrackEvent {
            command: "api".to_string(),
            sub_command: Some(format!("execute:{summary}")),
            duration_ms: started.elapsed().as_millis() as u64,
            success: result.is_ok(),
            error_message: None,
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            cli_version: env!("CARGO_PKG_VERSION"),
            is_ci: Configs::env_is_ci(),
        })
        .await;
    }

    result
}

async fn execute_document(args: &ExecuteArgs, query: &QuerySource) -> Result<()> {
    let variables = parse_variables(&VariableArgs {
        variables: args.variables.as_deref(),
        vars: &args.vars,
        raw_vars: &args.raw_vars,
        query_reads_stdin: query.read_from_stdin,
    })?;

    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let body = graphql_body(&query.document, args.operation_name.as_deref(), variables);

    let response = send_graphql_request(&client, &configs.get_backboard(), body).await?;

    print_response_body(&response.body, args.compact)?;

    if !response.status.is_success() {
        bail!("Railway API request failed with HTTP {}", response.status);
    }

    if !args.allow_errors {
        let error_count = serde_json::from_str::<Value>(&response.body)
            .ok()
            .and_then(|value| graphql_error_count(&value))
            .unwrap_or(0);
        if error_count > 0 {
            bail!("Railway API returned {error_count} GraphQL error(s)");
        }
    }

    Ok(())
}

/// Summarizes the operation that will execute as `<type>:<field>[+<field>...]`,
/// e.g. `mutation:serviceInstanceUpdate`. Only operation types and top-level
/// field names are captured — never arguments or values — so this is safe to
/// report in telemetry for understanding which API capabilities get used.
fn operation_summary(document: &str, operation_name: Option<&str>) -> Option<String> {
    let ast = graphql_parser::parse_query::<&str>(document).ok()?;
    let operations: Vec<&OperationDefinition<&str>> = ast
        .definitions
        .iter()
        .filter_map(|definition| match definition {
            Definition::Operation(operation) => Some(operation),
            Definition::Fragment(_) => None,
        })
        .collect();

    let operation = match operation_name {
        Some(name) => *operations
            .iter()
            .find(|operation| operation_definition_name(operation) == Some(name))?,
        None => match operations.as_slice() {
            [single] => single,
            _ => return None,
        },
    };

    let (operation_type, selection_set) = match operation {
        OperationDefinition::Query(query) => ("query", &query.selection_set),
        OperationDefinition::Mutation(mutation) => ("mutation", &mutation.selection_set),
        OperationDefinition::Subscription(subscription) => {
            ("subscription", &subscription.selection_set)
        }
        OperationDefinition::SelectionSet(selection_set) => ("query", selection_set),
    };

    let fields: Vec<&str> = selection_set
        .items
        .iter()
        .filter_map(|selection| match selection {
            Selection::Field(field) => Some(field.name),
            _ => None,
        })
        .take(MAX_OPERATION_SUMMARY_FIELDS)
        .collect();

    if fields.is_empty() {
        return None;
    }

    Some(format!("{operation_type}:{}", fields.join("+")))
}

fn operation_definition_name<'a>(operation: &OperationDefinition<'a, &'a str>) -> Option<&'a str> {
    match operation {
        OperationDefinition::Query(query) => query.name,
        OperationDefinition::Mutation(mutation) => mutation.name,
        OperationDefinition::Subscription(subscription) => subscription.name,
        OperationDefinition::SelectionSet(_) => None,
    }
}

#[derive(Debug)]
struct QuerySource {
    document: String,
    read_from_stdin: bool,
}

fn resolve_query_source(
    query: Option<&str>,
    file: Option<&str>,
    allow_empty_stdin: bool,
) -> Result<QuerySource> {
    if query.is_some() && file.is_some() {
        bail!("Pass the GraphQL document either as QUERY or --file, not both");
    }

    if let Some(query) = query {
        return Ok(QuerySource {
            document: query.to_string(),
            read_from_stdin: false,
        });
    }

    if let Some(path) = file {
        if path == "-" {
            return Ok(QuerySource {
                document: read_stdin(allow_empty_stdin)?,
                read_from_stdin: true,
            });
        }
        return Ok(QuerySource {
            document: fs::read_to_string(path)
                .with_context(|| format!("Failed to read GraphQL document from {path}"))?,
            read_from_stdin: false,
        });
    }

    if !io::stdin().is_terminal() {
        return Ok(QuerySource {
            document: read_stdin(allow_empty_stdin)?,
            read_from_stdin: true,
        });
    }

    bail!(
        "No GraphQL document provided. Pass QUERY, use --file, or pipe a document on stdin. Use `railway api search <term>` to inspect the schema."
    );
}

fn read_stdin(allow_empty: bool) -> Result<String> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("Failed to read stdin")?;
    if !allow_empty && buf.trim().is_empty() {
        bail!("No GraphQL document found on stdin");
    }
    Ok(buf)
}

struct VariableArgs<'a> {
    variables: Option<&'a str>,
    vars: &'a [String],
    raw_vars: &'a [String],
    query_reads_stdin: bool,
}

fn parse_variables(args: &VariableArgs<'_>) -> Result<Value> {
    let mut variables = match args.variables {
        Some(raw) => read_variables_value(raw, args.query_reads_stdin)?,
        None => json!({}),
    };

    let object = variables
        .as_object_mut()
        .ok_or_else(|| anyhow!("GraphQL variables must be a JSON object"))?;

    for item in args.vars {
        let (key, value) = split_key_value(item)?;
        let parsed = serde_json::from_str::<Value>(value).unwrap_or_else(|_| json!(value));
        object.insert(key.to_string(), parsed);
    }

    for item in args.raw_vars {
        let (key, value) = split_key_value(item)?;
        object.insert(key.to_string(), Value::String(value.to_string()));
    }

    Ok(variables)
}

fn read_variables_value(raw: &str, query_reads_stdin: bool) -> Result<Value> {
    let json_text = if let Some(path) = raw.strip_prefix('@') {
        if path == "-" {
            if query_reads_stdin {
                bail!("Cannot read both GraphQL document and variables from stdin");
            }
            read_stdin(true)?
        } else {
            fs::read_to_string(path)
                .with_context(|| format!("Failed to read variables from {path}"))?
        }
    } else {
        raw.to_string()
    };

    serde_json::from_str(&json_text).context("Failed to parse GraphQL variables as JSON")
}

fn split_key_value(item: &str) -> Result<(&str, &str)> {
    let Some((key, value)) = item.split_once('=') else {
        bail!("Expected KEY=VALUE, got {item:?}");
    };
    if key.trim().is_empty() {
        bail!("Variable key cannot be empty");
    }
    Ok((key.trim(), value))
}

fn graphql_body(query: &str, operation_name: Option<&str>, variables: Value) -> Value {
    let mut body = json!({
        "query": query,
        "variables": variables,
    });
    if let Some(operation_name) = operation_name {
        body["operationName"] = Value::String(operation_name.to_string());
    }
    body
}

#[derive(Debug)]
struct ApiHttpResponse {
    status: reqwest::StatusCode,
    body: String,
}

async fn send_graphql_request(
    client: &reqwest::Client,
    url: &str,
    body: Value,
) -> Result<ApiHttpResponse> {
    let response = client.post(url).json(&body).send().await?;
    let status = response.status();
    let body = response.text().await?;

    Ok(ApiHttpResponse { status, body })
}

fn response_json(response: &ApiHttpResponse) -> Result<Value> {
    serde_json::from_str(&response.body).with_context(|| {
        format!(
            "Railway API returned HTTP {} with a non-JSON response body",
            response.status
        )
    })
}

fn print_response_body(body: &str, compact: bool) -> Result<()> {
    match serde_json::from_str::<Value>(body) {
        Ok(value) => print_json(&value, compact),
        Err(_) => {
            print!("{body}");
            io::stdout().flush()?;
            Ok(())
        }
    }
}

fn print_json<T: Serialize>(value: &T, compact: bool) -> Result<()> {
    if compact {
        println!("{}", serde_json::to_string(value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

async fn fetch_live_schema() -> Result<Value> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let response = send_graphql_request(
        &client,
        &configs.get_backboard(),
        json!({ "query": introspection_query() }),
    )
    .await?;

    if !response.status.is_success() {
        bail!(
            "Failed to fetch the GraphQL schema: Railway API returned HTTP {}",
            response.status
        );
    }

    let schema = response_json(&response)?;

    if let Some(errors) = schema
        .get("errors")
        .and_then(Value::as_array)
        .filter(|errors| !errors.is_empty())
    {
        let detail = errors
            .first()
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        bail!(
            "Failed to fetch the GraphQL schema: Railway API returned {} GraphQL error(s): {detail}",
            errors.len()
        );
    }

    Ok(schema)
}

fn schema_root(schema: &Value) -> Result<&Value> {
    schema
        .pointer("/data/__schema")
        .or_else(|| schema.get("__schema"))
        .ok_or_else(|| anyhow!("GraphQL introspection schema is missing data.__schema"))
}

fn type_by_name<'a>(schema: &'a Value, name: &str) -> Result<Option<&'a Value>> {
    Ok(schema_root(schema)?
        .get("types")
        .and_then(Value::as_array)
        .and_then(|types| {
            types
                .iter()
                .find(|ty| ty.get("name").and_then(Value::as_str) == Some(name))
        }))
}

fn root_type_name(schema: &Value, operation_type: &str) -> Result<Option<String>> {
    let key = match operation_type {
        "query" => "queryType",
        "mutation" => "mutationType",
        "subscription" => "subscriptionType",
        _ => return Ok(None),
    };

    Ok(schema_root(schema)?
        .get(key)
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string))
}

fn root_type<'a>(schema: &'a Value, operation_type: &str) -> Result<Option<&'a Value>> {
    let Some(name) = root_type_name(schema, operation_type)? else {
        return Ok(None);
    };
    type_by_name(schema, &name)
}

fn type_ref_to_string(type_ref: &Value) -> String {
    match type_ref.get("kind").and_then(Value::as_str) {
        Some("NON_NULL") => format!(
            "{}!",
            type_ref
                .get("ofType")
                .map(type_ref_to_string)
                .unwrap_or_else(|| "Unknown".to_string())
        ),
        Some("LIST") => format!(
            "[{}]",
            type_ref
                .get("ofType")
                .map(type_ref_to_string)
                .unwrap_or_else(|| "Unknown".to_string())
        ),
        _ => type_ref
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Unknown")
            .to_string(),
    }
}

fn named_type(type_ref: &Value) -> Option<String> {
    if let Some(name) = type_ref.get("name").and_then(Value::as_str) {
        return Some(name.to_string());
    }
    type_ref.get("ofType").and_then(named_type)
}

fn is_required_type(type_ref: &Value) -> bool {
    type_ref.get("kind").and_then(Value::as_str) == Some("NON_NULL")
}

fn field_to_json(parent: &str, operation_type: Option<&str>, field: &Value) -> Value {
    let args: Vec<Value> = field
        .get("args")
        .and_then(Value::as_array)
        .map(|args| args.iter().map(arg_to_json).collect())
        .unwrap_or_default();
    let required_args: Vec<String> = field
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter(|arg| {
                    arg.get("defaultValue").is_none_or(Value::is_null)
                        && arg.get("type").is_some_and(is_required_type)
                })
                .filter_map(|arg| arg.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    json!({
        "kind": operation_type.map_or("field".to_string(), str::to_string),
        "parent": parent,
        "name": field.get("name").and_then(Value::as_str),
        "description": field.get("description").cloned().unwrap_or(Value::Null),
        "type": field.get("type").map(type_ref_to_string).unwrap_or_else(|| "Unknown".to_string()),
        "namedType": field.get("type").and_then(named_type),
        "args": args,
        "requiredArgs": required_args,
        "isDeprecated": field.get("isDeprecated").and_then(Value::as_bool).unwrap_or(false),
        "deprecationReason": field.get("deprecationReason").cloned().unwrap_or(Value::Null),
    })
}

fn arg_to_json(arg: &Value) -> Value {
    json!({
        "name": arg.get("name").and_then(Value::as_str),
        "description": arg.get("description").cloned().unwrap_or(Value::Null),
        "type": arg.get("type").map(type_ref_to_string).unwrap_or_else(|| "Unknown".to_string()),
        "namedType": arg.get("type").and_then(named_type),
        "required": arg.get("defaultValue").is_none_or(Value::is_null)
            && arg.get("type").is_some_and(is_required_type),
        "defaultValue": arg.get("defaultValue").cloned().unwrap_or(Value::Null),
    })
}

fn type_to_json(ty: &Value) -> Value {
    let kind = ty.get("kind").and_then(Value::as_str).unwrap_or("UNKNOWN");
    let fields: Vec<Value> = ty
        .get("fields")
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .map(|field| {
                    field_to_json(
                        ty.get("name").and_then(Value::as_str).unwrap_or(""),
                        None,
                        field,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let input_fields: Vec<Value> = ty
        .get("inputFields")
        .and_then(Value::as_array)
        .map(|fields| fields.iter().map(arg_to_json).collect())
        .unwrap_or_default();
    let enum_values: Vec<Value> = ty
        .get("enumValues")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    json!({
                        "name": value.get("name").and_then(Value::as_str),
                        "description": value.get("description").cloned().unwrap_or(Value::Null),
                        "isDeprecated": value.get("isDeprecated").and_then(Value::as_bool).unwrap_or(false),
                        "deprecationReason": value.get("deprecationReason").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    json!({
        "kind": kind,
        "name": ty.get("name").and_then(Value::as_str),
        "description": ty.get("description").cloned().unwrap_or(Value::Null),
        "fields": fields,
        "inputFields": input_fields,
        "enumValues": enum_values,
    })
}

fn search_schema(
    schema: &Value,
    term: &str,
    kind: &SearchKind,
    limit: usize,
) -> Result<Vec<Value>> {
    let needle = term.to_ascii_lowercase();
    let mut results = Vec::new();
    let root = schema_root(schema)?;
    let types = root
        .get("types")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("GraphQL schema is missing types"))?;

    for ty in types {
        if results.len() >= limit {
            break;
        }

        let type_name = ty.get("name").and_then(Value::as_str).unwrap_or("");
        let type_kind = ty.get("kind").and_then(Value::as_str).unwrap_or("");
        let type_description = ty.get("description").and_then(Value::as_str).unwrap_or("");

        if matches_kind_for_type(type_kind, kind)
            && text_matches(&needle, &[type_name, type_description])
        {
            results.push(json!({
                "kind": type_kind.to_ascii_lowercase(),
                "name": type_name,
                "description": ty.get("description").cloned().unwrap_or(Value::Null),
            }));
        }

        if results.len() >= limit {
            break;
        }

        let field_kind = operation_kind_for_root_type(schema, type_name)?;
        if let Some(fields) = ty.get("fields").and_then(Value::as_array) {
            for field in fields {
                if results.len() >= limit {
                    break;
                }

                let result_kind = field_kind.as_deref().unwrap_or("field");
                if !matches_kind_for_field(result_kind, kind) {
                    continue;
                }
                let field_name = field.get("name").and_then(Value::as_str).unwrap_or("");
                let field_description = field
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if text_matches(&needle, &[field_name, field_description, type_name]) {
                    results.push(field_to_json(type_name, field_kind.as_deref(), field));
                }
            }
        }

        if let Some(input_fields) = ty.get("inputFields").and_then(Value::as_array) {
            for field in input_fields {
                if results.len() >= limit {
                    break;
                }
                if !matches!(
                    kind,
                    SearchKind::All | SearchKind::Input | SearchKind::Field
                ) {
                    continue;
                }
                let field_name = field.get("name").and_then(Value::as_str).unwrap_or("");
                let field_description = field
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if text_matches(&needle, &[field_name, field_description, type_name]) {
                    let mut value = arg_to_json(field);
                    value["kind"] = json!("inputField");
                    value["parent"] = json!(type_name);
                    results.push(value);
                }
            }
        }
    }

    Ok(results)
}

fn matches_kind_for_type(type_kind: &str, wanted: &SearchKind) -> bool {
    match wanted {
        SearchKind::All => true,
        SearchKind::Type => true,
        SearchKind::Input => type_kind == "INPUT_OBJECT",
        SearchKind::Enum => type_kind == "ENUM",
        SearchKind::Field | SearchKind::Query | SearchKind::Mutation | SearchKind::Subscription => {
            false
        }
    }
}

fn matches_kind_for_field(result_kind: &str, wanted: &SearchKind) -> bool {
    match wanted {
        SearchKind::All | SearchKind::Field => true,
        SearchKind::Query => result_kind == "query",
        SearchKind::Mutation => result_kind == "mutation",
        SearchKind::Subscription => result_kind == "subscription",
        SearchKind::Type | SearchKind::Input | SearchKind::Enum => false,
    }
}

fn text_matches(needle: &str, values: &[&str]) -> bool {
    values
        .iter()
        .any(|value| value.to_ascii_lowercase().contains(needle))
}

fn operation_kind_for_root_type(schema: &Value, type_name: &str) -> Result<Option<String>> {
    for operation_type in ["query", "mutation", "subscription"] {
        if root_type_name(schema, operation_type)?.as_deref() == Some(type_name) {
            return Ok(Some(operation_type.to_string()));
        }
    }
    Ok(None)
}

fn describe_schema_member(schema: &Value, name: &str) -> Result<Vec<Value>> {
    let mut descriptions = Vec::new();

    if let Some((parent_name, field_name)) = name.split_once('.') {
        let Some(parent) = type_by_name(schema, parent_name)? else {
            bail!("No GraphQL type named {parent_name:?}");
        };
        let Some(field) = fields(parent)
            .into_iter()
            .find(|field| field.get("name").and_then(Value::as_str) == Some(field_name))
        else {
            bail!("No field named {field_name:?} on type {parent_name:?}");
        };
        descriptions.push(field_to_json(parent_name, None, field));
        return Ok(descriptions);
    }

    if let Some(ty) = type_by_name(schema, name)? {
        descriptions.push(type_to_json(ty));
    }

    for operation_type in ["query", "mutation", "subscription"] {
        let Some(root) = root_type(schema, operation_type)? else {
            continue;
        };
        let root_name = root.get("name").and_then(Value::as_str).unwrap_or("");
        for field in fields(root) {
            if field.get("name").and_then(Value::as_str) == Some(name) {
                descriptions.push(field_to_json(root_name, Some(operation_type), field));
            }
        }
    }

    if descriptions.is_empty() {
        bail!("No GraphQL type or root field named {name:?}");
    }

    Ok(descriptions)
}

fn fields(ty: &Value) -> Vec<&Value> {
    ty.get("fields")
        .and_then(Value::as_array)
        .map(|fields| fields.iter().collect())
        .unwrap_or_default()
}

fn graphql_error_count(value: &Value) -> Option<usize> {
    value.get("errors").and_then(Value::as_array).map(Vec::len)
}

fn introspection_query() -> &'static str {
    r#"
query IntrospectionQuery {
  __schema {
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    types {
      kind
      name
      description
      fields(includeDeprecated: true) {
        name
        description
        args {
          name
          description
          type { ...TypeRef }
          defaultValue
        }
        type { ...TypeRef }
        isDeprecated
        deprecationReason
      }
      inputFields {
        name
        description
        type { ...TypeRef }
        defaultValue
      }
      interfaces { ...TypeRef }
      enumValues(includeDeprecated: true) {
        name
        description
        isDeprecated
        deprecationReason
      }
      possibleTypes { ...TypeRef }
    }
    directives {
      name
      description
      locations
      args {
        name
        description
        type { ...TypeRef }
        defaultValue
      }
    }
  }
}

fragment TypeRef on __Type {
  kind
  name
  ofType {
    kind
    name
    ofType {
      kind
      name
      ofType {
        kind
        name
        ofType {
          kind
          name
          ofType {
            kind
            name
            ofType {
              kind
              name
              ofType {
                kind
                name
              }
            }
          }
        }
      }
    }
  }
}
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_schema() -> Value {
        json!({
            "data": {
                "__schema": {
                    "queryType": { "name": "Query" },
                    "mutationType": { "name": "Mutation" },
                    "subscriptionType": null,
                    "types": [
                        {
                            "kind": "OBJECT",
                            "name": "Query",
                            "description": null,
                            "fields": [
                                {
                                    "name": "project",
                                    "description": "Look up a project by ID.",
                                    "args": [
                                        {
                                            "name": "id",
                                            "description": null,
                                            "type": {
                                                "kind": "NON_NULL",
                                                "name": null,
                                                "ofType": { "kind": "SCALAR", "name": "String", "ofType": null }
                                            },
                                            "defaultValue": null
                                        }
                                    ],
                                    "type": {
                                        "kind": "NON_NULL",
                                        "name": null,
                                        "ofType": { "kind": "OBJECT", "name": "Project", "ofType": null }
                                    },
                                    "isDeprecated": false,
                                    "deprecationReason": null
                                }
                            ],
                            "inputFields": null,
                            "enumValues": null
                        },
                        {
                            "kind": "OBJECT",
                            "name": "Mutation",
                            "description": null,
                            "fields": [
                                {
                                    "name": "projectDelete",
                                    "description": null,
                                    "args": [
                                        {
                                            "name": "id",
                                            "description": null,
                                            "type": {
                                                "kind": "NON_NULL",
                                                "name": null,
                                                "ofType": { "kind": "SCALAR", "name": "String", "ofType": null }
                                            },
                                            "defaultValue": null
                                        }
                                    ],
                                    "type": { "kind": "SCALAR", "name": "Boolean", "ofType": null },
                                    "isDeprecated": false,
                                    "deprecationReason": null
                                }
                            ],
                            "inputFields": null,
                            "enumValues": null
                        },
                        {
                            "kind": "OBJECT",
                            "name": "Project",
                            "description": "A Railway project.",
                            "fields": [
                                {
                                    "name": "id",
                                    "description": null,
                                    "args": [],
                                    "type": {
                                        "kind": "NON_NULL",
                                        "name": null,
                                        "ofType": { "kind": "SCALAR", "name": "String", "ofType": null }
                                    },
                                    "isDeprecated": false,
                                    "deprecationReason": null
                                },
                                {
                                    "name": "name",
                                    "description": null,
                                    "args": [],
                                    "type": { "kind": "SCALAR", "name": "String", "ofType": null },
                                    "isDeprecated": false,
                                    "deprecationReason": null
                                }
                            ],
                            "inputFields": null,
                            "enumValues": null
                        },
                        {
                            "kind": "INPUT_OBJECT",
                            "name": "ProjectUpdateInput",
                            "description": null,
                            "fields": null,
                            "inputFields": [
                                {
                                    "name": "name",
                                    "description": null,
                                    "type": { "kind": "SCALAR", "name": "String", "ofType": null },
                                    "defaultValue": null
                                }
                            ],
                            "enumValues": null
                        }
                    ]
                }
            }
        })
    }

    fn result_names(results: &[Value]) -> Vec<&str> {
        results
            .iter()
            .filter_map(|result| result.get("name").and_then(Value::as_str))
            .collect()
    }

    #[test]
    fn search_finds_types_and_fields() {
        let schema = fixture_schema();
        let results = search_schema(&schema, "project", &SearchKind::All, 25).unwrap();
        let names = result_names(&results);

        assert!(names.contains(&"Project"));
        assert!(names.contains(&"project"));
        assert!(names.contains(&"projectDelete"));
        assert!(names.contains(&"ProjectUpdateInput"));
    }

    #[test]
    fn search_restricts_results_by_kind() {
        let schema = fixture_schema();

        let mutations = search_schema(&schema, "project", &SearchKind::Mutation, 25).unwrap();
        assert_eq!(result_names(&mutations), vec!["projectDelete"]);

        let queries = search_schema(&schema, "project", &SearchKind::Query, 25).unwrap();
        assert_eq!(result_names(&queries), vec!["project"]);
    }

    #[test]
    fn search_respects_limit() {
        let schema = fixture_schema();
        let results = search_schema(&schema, "project", &SearchKind::All, 2).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn describe_returns_type_with_fields() {
        let schema = fixture_schema();
        let descriptions = describe_schema_member(&schema, "Project").unwrap();

        assert_eq!(descriptions.len(), 1);
        assert_eq!(descriptions[0]["kind"], json!("OBJECT"));
        assert_eq!(descriptions[0]["name"], json!("Project"));
        let fields = descriptions[0]["fields"].as_array().unwrap();
        assert_eq!(result_names(fields), vec!["id", "name"]);
    }

    #[test]
    fn describe_returns_root_field_with_required_args() {
        let schema = fixture_schema();
        let descriptions = describe_schema_member(&schema, "project").unwrap();

        assert_eq!(descriptions.len(), 1);
        assert_eq!(descriptions[0]["kind"], json!("query"));
        assert_eq!(descriptions[0]["type"], json!("Project!"));
        assert_eq!(descriptions[0]["requiredArgs"], json!(["id"]));
    }

    #[test]
    fn describe_resolves_parent_dot_field() {
        let schema = fixture_schema();
        let descriptions = describe_schema_member(&schema, "Project.name").unwrap();

        assert_eq!(descriptions.len(), 1);
        assert_eq!(descriptions[0]["parent"], json!("Project"));
        assert_eq!(descriptions[0]["name"], json!("name"));
        assert_eq!(descriptions[0]["type"], json!("String"));
    }

    #[test]
    fn describe_fails_for_unknown_name() {
        let schema = fixture_schema();
        assert!(describe_schema_member(&schema, "DoesNotExist").is_err());
        assert!(describe_schema_member(&schema, "Project.doesNotExist").is_err());
    }

    #[test]
    fn type_refs_render_wrapped_types() {
        let non_null_list = json!({
            "kind": "NON_NULL",
            "name": null,
            "ofType": {
                "kind": "LIST",
                "name": null,
                "ofType": {
                    "kind": "NON_NULL",
                    "name": null,
                    "ofType": { "kind": "SCALAR", "name": "String", "ofType": null }
                }
            }
        });

        assert_eq!(type_ref_to_string(&non_null_list), "[String!]!");
    }

    #[test]
    fn summarizes_single_operations() {
        assert_eq!(
            operation_summary("query { me { id } }", None),
            Some("query:me".to_string())
        );
        assert_eq!(
            operation_summary("{ me { id } }", None),
            Some("query:me".to_string())
        );
        assert_eq!(
            operation_summary(
                "mutation Update($id: String!) { serviceInstanceUpdate(serviceId: $id) }",
                None
            ),
            Some("mutation:serviceInstanceUpdate".to_string())
        );
        assert_eq!(
            operation_summary(
                "query { me { id } projects { edges { node { id } } } }",
                None
            ),
            Some("query:me+projects".to_string())
        );
    }

    #[test]
    fn summarizes_operation_selected_by_name() {
        let document = "query A { me { id } } mutation B { projectDelete(id: \"x\") }";

        assert_eq!(
            operation_summary(document, Some("B")),
            Some("mutation:projectDelete".to_string())
        );
        assert_eq!(
            operation_summary(document, Some("A")),
            Some("query:me".to_string())
        );
        assert_eq!(operation_summary(document, Some("C")), None);
        assert_eq!(operation_summary(document, None), None);
    }

    #[test]
    fn summarizes_nothing_for_invalid_documents() {
        assert_eq!(operation_summary("not graphql", None), None);
        assert_eq!(operation_summary("", None), None);
    }

    #[test]
    fn parses_variables_from_json_and_flags() {
        let variables = parse_variables(&VariableArgs {
            variables: Some(r#"{"projectId":"p1","count":1}"#),
            vars: &["enabled=true".to_string(), "labels=[\"api\"]".to_string()],
            raw_vars: &["name=web".to_string()],
            query_reads_stdin: false,
        })
        .unwrap();

        assert_eq!(variables["projectId"], json!("p1"));
        assert_eq!(variables["count"], json!(1));
        assert_eq!(variables["enabled"], json!(true));
        assert_eq!(variables["labels"], json!(["api"]));
        assert_eq!(variables["name"], json!("web"));
    }
}
