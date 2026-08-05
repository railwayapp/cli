use super::*;
use crate::{
    client::post_graphql,
    commands::mutations,
    controllers::{
        project::resolve_service_context,
        variables::{Variable, get_service_variables},
    },
    table::Table,
    util::progress::create_spinner_if,
};
use anyhow::{Context, bail};
use colored::Colorize;
use std::io::{IsTerminal, Read};

/// Manage environment variables for a service
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway variable list --service api --json\n  railway variable list --service api --kv\n  railway variable set API_URL=https://example.com --skip-deploys --json\n  echo \"secret\" | railway variable set API_KEY --stdin --skip-deploys --json\n  railway variable delete API_KEY --service api --json\n\nAutomation notes:\n  JSON and KV output include raw variable values. Avoid sharing command output from secret-bearing variable commands.\n  For idempotent deletes, list variables first, check whether the key exists, then delete it."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,

    // Legacy flags for backwards compatibility
    /// The service to show/set variables for
    #[clap(short, long)]
    service: Option<String>,

    /// The environment to show/set variables for
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Show variables in KV format. This prints raw values.
    #[clap(short, long)]
    kv: bool,

    /// The "{key}={value}" environment variable pair to set the service variables (legacy, use 'variable set' instead)
    #[clap(long)]
    set: Vec<Variable>,

    /// Set a variable with the value read from stdin (legacy, use 'variable set --stdin' instead)
    #[clap(long, value_name = "KEY")]
    set_from_stdin: Option<String>,

    /// Output in JSON format. Variable list JSON includes raw values.
    #[clap(long)]
    json: bool,

    /// Skip triggering deploys when setting variables
    #[clap(long)]
    skip_deploys: bool,
}

#[derive(Parser)]
enum Commands {
    /// List variables for a service
    #[clap(visible_alias = "ls")]
    List(ListArgs),

    /// Set a variable
    Set(SetArgs),

    /// Delete a variable
    #[clap(visible_alias = "rm", visible_alias = "remove")]
    Delete(DeleteArgs),

    /// Import variables from a file
    Import(ImportArgs),

    /// Export variables to a file
    Export(ExportArgs),
}

#[derive(Parser)]
struct ListArgs {
    /// The service to list variables for
    #[clap(short, long)]
    service: Option<String>,

    /// The environment to list variables from
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Show variables in KV format. This prints raw values.
    #[clap(short, long)]
    kv: bool,

    /// Output in JSON format. This includes raw values.
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct SetArgs {
    /// Variable(s) in KEY=VALUE format, or just KEY when using --stdin
    #[clap(required = true)]
    variables: Vec<String>,

    /// The service to set the variable for
    #[clap(short, long)]
    service: Option<String>,

    /// The environment to set the variable in
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Read the value from stdin instead of the command line (only with single KEY)
    #[clap(long)]
    stdin: bool,

    /// Skip triggering deploys when setting the variable
    #[clap(long)]
    skip_deploys: bool,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct DeleteArgs {
    /// The variable key to delete
    key: String,

    /// The service to delete the variable from
    #[clap(short, long)]
    service: Option<String>,

    /// The environment to delete the variable from
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct ImportArgs {
    /// The file to import from (.env or .json)
    #[clap(short, long)]
    file: String,

    /// The service to import variables to
    #[clap(short, long)]
    service: Option<String>,

    /// The environment to import variables to
    #[clap(short, long)]
    environment: Option<String>,

    /// Skip triggering deploys when importing variables
    #[clap(long)]
    skip_deploys: bool,

    /// Accept all overwrites without prompting
    #[clap(short, long)]
    yes: bool,

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct ExportArgs {
    /// The file to export to (.env or .json)
    #[clap(short, long)]
    file: String,

    /// The service to export variables from
    #[clap(short, long)]
    service: Option<String>,

    /// The environment to export variables from
    #[clap(short, long)]
    environment: Option<String>,

    /// Output in JSON format (to stdout)
    #[clap(long)]
    json: bool,
}

pub async fn command(args: Args) -> Result<()> {
    if let Some(cmd) = args.command {
        return match cmd {
            Commands::List(list_args) => list_variables(list_args).await,
            Commands::Set(set_args) => set_variable(set_args).await,
            Commands::Delete(delete_args) => delete_variable(delete_args).await,
            Commands::Import(import_args) => import_variables(import_args).await,
            Commands::Export(export_args) => export_variables(export_args).await,
        };
    }

    // Legacy behavior: handle --set-from-stdin
    if let Some(key) = args.set_from_stdin {
        let value = read_value_from_stdin()?;
        let variable = Variable { key, value };
        return set_variables_legacy(
            vec![variable],
            args.service,
            args.environment,
            args.project,
            args.skip_deploys,
        )
        .await;
    }

    // Legacy behavior: handle --set flag
    if !args.set.is_empty() {
        return set_variables_legacy(
            args.set,
            args.service,
            args.environment,
            args.project,
            args.skip_deploys,
        )
        .await;
    }

    // Legacy behavior: list variables (default)
    list_variables(ListArgs {
        service: args.service,
        environment: args.environment,
        project: args.project,
        kv: args.kv,
        json: args.json,
    })
    .await
}

async fn list_variables(args: ListArgs) -> Result<()> {
    let ctx = resolve_service_context(args.project, args.service, args.environment).await?;

    let variables = get_service_variables(
        &ctx.client,
        &ctx.configs,
        ctx.project.id.clone(),
        ctx.environment_id,
        ctx.service_id,
    )
    .await?;

    if args.kv {
        for (key, value) in variables {
            println!("{key}={value}");
        }
        return Ok(());
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&variables)?);
        return Ok(());
    }

    if variables.is_empty() {
        eprintln!("No variables found");
        return Ok(());
    }

    let table = Table::new(ctx.service_name, variables);
    table.print()?;

    Ok(())
}

async fn set_variable(args: SetArgs) -> Result<()> {
    let variables = if args.stdin {
        if args.variables.len() != 1 {
            bail!("--stdin requires exactly one KEY argument");
        }
        let key = &args.variables[0];
        if key.contains('=') {
            bail!(
                "Cannot use --stdin with KEY=VALUE format. Use: railway variable set KEY --stdin"
            );
        }
        let value = read_value_from_stdin()?;
        vec![Variable {
            key: key.clone(),
            value,
        }]
    } else {
        args.variables
            .iter()
            .map(|s| s.parse::<Variable>())
            .collect::<Result<Vec<_>, _>>()?
    };

    set_variables_internal(
        variables,
        args.service,
        args.environment,
        args.project,
        args.skip_deploys,
        args.json,
    )
    .await
}

async fn delete_variable(args: DeleteArgs) -> Result<()> {
    let ctx = resolve_service_context(args.project, args.service, args.environment).await?;

    let variables = get_service_variables(
        &ctx.client,
        &ctx.configs,
        ctx.project_id.clone(),
        ctx.environment_id.clone(),
        ctx.service_id.clone(),
    )
    .await?;
    if !variables.contains_key(&args.key) {
        bail!("Variable '{}' not found", args.key);
    }

    let spinner = create_spinner_if(!args.json, format!("Deleting {}...", args.key.bold()));

    let vars = mutations::variable_delete::Variables {
        project_id: ctx.project_id,
        environment_id: ctx.environment_id,
        name: args.key.clone(),
        service_id: Some(ctx.service_id),
    };

    post_graphql::<mutations::VariableDelete, _>(&ctx.client, ctx.configs.get_backboard(), vars)
        .await?;

    if let Some(sp) = spinner {
        sp.finish_with_message(format!("Deleted variable {}", args.key.bold()));
    } else {
        println!("{}", serde_json::json!({"key": args.key, "deleted": true}));
    }

    Ok(())
}

// Legacy helper for --set flag
async fn set_variables_legacy(
    variables: Vec<Variable>,
    service: Option<String>,
    environment: Option<String>,
    project: Option<String>,
    skip_deploys: bool,
) -> Result<()> {
    set_variables_internal(
        variables,
        service,
        environment,
        project,
        skip_deploys,
        false,
    )
    .await
}

async fn set_variables_internal(
    variables: Vec<Variable>,
    service: Option<String>,
    environment: Option<String>,
    project: Option<String>,
    skip_deploys: bool,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(project, service, environment).await?;

    let keys: Vec<String> = variables.iter().map(|v| v.key.clone()).collect();
    let fmt_keys = keys
        .iter()
        .map(|k| k.bold().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    let spinner = create_spinner_if(!json, format!("Setting {fmt_keys}..."));

    let vars = mutations::variable_collection_upsert::Variables {
        project_id: ctx.project_id,
        environment_id: ctx.environment_id,
        service_id: ctx.service_id,
        variables: variables.into_iter().map(|v| (v.key, v.value)).collect(),
        skip_deploys: skip_deploys.then_some(true),
    };

    post_graphql::<mutations::VariableCollectionUpsert, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        vars,
    )
    .await?;

    if let Some(sp) = spinner {
        sp.finish_with_message(format!("Set variables {fmt_keys}"));
    } else {
        println!("{}", serde_json::json!({"keys": keys, "set": true}));
    }

    Ok(())
}

fn read_value_from_stdin() -> Result<String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        bail!(
            "No input provided via stdin. Use --stdin with piped input, e.g.:\n  echo \"value\" | railway variable set KEY --stdin"
        );
    }

    let mut value = String::new();
    stdin.lock().read_to_string(&mut value)?;

    let value = value.trim_end_matches('\n').trim_end_matches('\r');

    if value.is_empty() {
        bail!("Empty value provided via stdin");
    }

    Ok(value.to_string())
}

async fn import_variables(args: ImportArgs) -> Result<()> {
    let ctx = resolve_service_context(args.service.clone(), args.environment.clone()).await?;

    // Read file content
    let content = tokio::fs::read_to_string(&args.file)
        .await
        .with_context(|| format!("Failed to read file: {}", args.file))?;

    // Parse variables based on file extension
    let variables = if args.file.ends_with(".json") {
        parse_json_variables(&content)?
    } else {
        parse_env_variables(&content)?
    };

    if variables.is_empty() {
        eprintln!("No variables found in file");
        return Ok(());
    }

    // Get existing variables for conflict detection
    let existing = get_service_variables(
        &ctx.client,
        &ctx.configs,
        ctx.project.id.clone(),
        ctx.environment_id.clone(),
        ctx.service_id.clone(),
    )
    .await?;

    // Determine which variables to import
    let mut to_import = Vec::new();
    let mut skipped = Vec::new();

    let is_tty = std::io::stdout().is_terminal();

    for var in &variables {
        if existing.contains_key(&var.key) {
            let should_overwrite = if args.yes {
                true
            } else if !is_tty {
                eprintln!(
                    "Skipping {}: already exists (use --yes to overwrite)",
                    var.key.bold()
                );
                false
            } else {
                match prompt_for_overwrite(&var.key)? {
                    OverwriteChoice::Yes => true,
                    OverwriteChoice::No => {
                        skipped.push(var.key.clone());
                        false
                    }
                    OverwriteChoice::Quit => {
                        eprintln!("Import cancelled");
                        return Ok(());
                    }
                }
            };

            if should_overwrite {
                to_import.push(var.clone());
            }
        } else {
            to_import.push(var.clone());
        }
    }

    if to_import.is_empty() {
        eprintln!("No variables to import");
        return Ok(());
    }

    // Import the variables
    let keys: Vec<String> = to_import.iter().map(|v| v.key.clone()).collect();
    let fmt_keys = keys
        .iter()
        .map(|k| k.bold().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    let spinner = create_spinner_if(!args.json, format!("Importing {fmt_keys}..."));

    let vars = mutations::variable_collection_upsert::Variables {
        project_id: ctx.project_id,
        environment_id: ctx.environment_id,
        service_id: ctx.service_id,
        variables: to_import.into_iter().map(|v| (v.key, v.value)).collect(),
        skip_deploys: args.skip_deploys.then_some(true),
    };

    post_graphql::<mutations::VariableCollectionUpsert, _>(
        &ctx.client,
        ctx.configs.get_backboard(),
        vars,
    )
    .await?;

    if let Some(sp) = spinner {
        sp.finish_with_message(format!("Imported {} variables", keys.len()));
    } else {
        println!(
            "{}",
            serde_json::json!({"imported": keys.len(), "skipped": skipped.len(), "keys": keys})
        );
    }

    Ok(())
}

#[derive(Clone, Copy)]
enum OverwriteChoice {
    Yes,
    No,
    Quit,
}

fn prompt_for_overwrite(key: &str) -> Result<OverwriteChoice> {
    use std::io::Write;

    print!("Overwrite {}? [y/N/q]: ", key.bold());
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    match input.trim().to_lowercase().as_str() {
        "y" | "yes" => Ok(OverwriteChoice::Yes),
        "q" | "quit" => Ok(OverwriteChoice::Quit),
        _ => Ok(OverwriteChoice::No),
    }
}

fn parse_json_variables(content: &str) -> Result<Vec<Variable>> {
    let map: std::collections::BTreeMap<String, String> = serde_json::from_str(content)
        .with_context(|| "Failed to parse JSON file. Expected format: {\"KEY\": \"value\"}")?;

    Ok(map
        .into_iter()
        .map(|(key, value)| Variable { key, value })
        .collect())
}

fn parse_env_variables(content: &str) -> Result<Vec<Variable>> {
    let mut variables = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let line_num = line_num + 1;
        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Parse KEY=VALUE format
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_string();
            let value = parse_env_value(&trimmed[eq_pos + 1..])
                .with_context(|| format!("Failed to parse value on line {}", line_num))?;

            if key.is_empty() {
                bail!("Empty key on line {}", line_num);
            }

            variables.push(Variable { key, value });
        } else {
            bail!("Invalid format on line {}: expected KEY=VALUE", line_num);
        }
    }

    Ok(variables)
}

fn parse_env_value(value: &str) -> Result<String> {
    let value = value.trim();

    if value.is_empty() {
        return Ok(String::new());
    }

    // Handle double-quoted values
    if value.starts_with('"') && value.ends_with('"') && value.len() > 1 {
        let inner = &value[1..value.len() - 1];
        // Process escape sequences
        let mut result = String::new();
        let mut chars = inner.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => result.push('\n'),
                    Some('t') => result.push('\t'),
                    Some('r') => result.push('\r'),
                    Some('\\') => result.push('\\'),
                    Some('"') => result.push('"'),
                    Some(c) => {
                        result.push('\\');
                        result.push(c);
                    }
                    None => result.push('\\'),
                }
            } else {
                result.push(c);
            }
        }
        return Ok(result);
    }

    // Handle single-quoted values (no escape processing)
    if value.starts_with('\'') && value.ends_with('\'') && value.len() > 1 {
        return Ok(value[1..value.len() - 1].to_string());
    }

    // Unquoted value
    Ok(value.to_string())
}

async fn export_variables(args: ExportArgs) -> Result<()> {
    let ctx = resolve_service_context(args.service, args.environment).await?;

    let variables = get_service_variables(
        &ctx.client,
        &ctx.configs,
        ctx.project.id.clone(),
        ctx.environment_id,
        ctx.service_id,
    )
    .await?;

    if variables.is_empty() {
        eprintln!("No variables found to export");
        return Ok(());
    }

    // Determine format by file extension
    let is_json = args.file.ends_with(".json");

    let content = if is_json {
        serde_json::to_string_pretty(&variables)?
    } else {
        // .env format
        let mut lines = Vec::new();
        for (key, value) in &variables {
            // Escape special characters for .env format
            let escaped_value = if value.contains('\n')
                || value.contains('"')
                || value.contains('\'')
                || value.contains('$')
            {
                // Use double quotes and escape
                let escaped = value
                    .replace('\\', "\\\\")
                    .replace('\n', "\\n")
                    .replace('\t', "\\t")
                    .replace('\r', "\\r")
                    .replace('"', "\\\"");
                format!("{}=\"{}\"", key, escaped)
            } else if value.contains(' ') || value.contains('#') {
                format!("{}=\"{}\"", key, value)
            } else {
                format!("{}={}", key, value)
            };
            lines.push(escaped_value);
        }
        lines.join("\n") + "\n"
    };

    // Write to file
    tokio::fs::write(&args.file, content)
        .await
        .with_context(|| format!("Failed to write to file: {}", args.file))?;

    if !args.json {
        println!(
            "Exported {} variables to {}",
            variables.len(),
            args.file.bold()
        );
    } else {
        println!(
            "{}",
            serde_json::json!({
                "exported": variables.len(),
                "file": args.file,
            })
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_env_basic() {
        let content = "KEY=value\nFOO=bar";
        let vars = parse_env_variables(content).unwrap();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].key, "KEY");
        assert_eq!(vars[0].value, "value");
        assert_eq!(vars[1].key, "FOO");
        assert_eq!(vars[1].value, "bar");
    }

    #[test]
    fn test_parse_env_quoted_double() {
        let content = r#"KEY="value with spaces""#;
        let vars = parse_env_variables(content).unwrap();
        assert_eq!(vars[0].key, "KEY");
        assert_eq!(vars[0].value, "value with spaces");
    }

    #[test]
    fn test_parse_env_quoted_single() {
        let content = "KEY='no escapes here'";
        let vars = parse_env_variables(content).unwrap();
        assert_eq!(vars[0].key, "KEY");
        assert_eq!(vars[0].value, "no escapes here");
    }

    #[test]
    fn test_parse_env_escapes() {
        let content = r#"KEY="line1\nline2\ttab""#;
        let vars = parse_env_variables(content).unwrap();
        assert_eq!(vars[0].value, "line1\nline2\ttab");
    }

    #[test]
    fn test_parse_env_comments_and_empty() {
        let content = "\n# This is a comment\nKEY=value\n\n\n# Another comment\nFOO=bar";
        let vars = parse_env_variables(content).unwrap();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].key, "KEY");
        assert_eq!(vars[1].key, "FOO");
    }

    #[test]
    fn test_parse_env_empty_value() {
        let content = "KEY=";
        let vars = parse_env_variables(content).unwrap();
        assert_eq!(vars[0].key, "KEY");
        assert_eq!(vars[0].value, "");
    }

    #[test]
    fn test_parse_env_whitespace_trimmed() {
        let content = "  KEY  =  value  ";
        let vars = parse_env_variables(content).unwrap();
        assert_eq!(vars[0].key, "KEY");
        assert_eq!(vars[0].value, "value");
    }

    #[test]
    fn test_parse_env_invalid_format() {
        let content = "INVALID_LINE";
        assert!(parse_env_variables(content).is_err());
    }

    #[test]
    fn test_parse_env_empty_key() {
        let content = "=value";
        assert!(parse_env_variables(content).is_err());
    }

    #[test]
    fn test_parse_json_basic() {
        let content = r#"{"KEY": "value", "NUM": "123"}"#;
        let vars = parse_json_variables(content).unwrap();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].key, "KEY");
        assert_eq!(vars[0].value, "value");
        assert_eq!(vars[1].key, "NUM");
        assert_eq!(vars[1].value, "123");
    }

    #[test]
    fn test_parse_json_empty() {
        let content = "{}";
        let vars = parse_json_variables(content).unwrap();
        assert!(vars.is_empty());
    }

    #[test]
    fn test_parse_json_invalid() {
        let content = "not valid json";
        assert!(parse_json_variables(content).is_err());
    }

    #[test]
    fn test_parse_env_value_with_equals() {
        let content = "KEY=val=ue=with=equals";
        let vars = parse_env_variables(content).unwrap();
        assert_eq!(vars[0].key, "KEY");
        assert_eq!(vars[0].value, "val=ue=with=equals");
    }

    #[test]
    fn test_parse_env_unknown_escapes() {
        let content = r#"KEY="value\x\y\z""#;
        let vars = parse_env_variables(content).unwrap();
        assert_eq!(vars[0].value, "value\\x\\y\\z");
    }

    #[test]
    fn test_parse_env_escaped_quotes() {
        let content = r#"KEY="say \"hello\"""#;
        let vars = parse_env_variables(content).unwrap();
        assert_eq!(vars[0].value, "say \"hello\"");
    }

    #[test]
    fn test_parse_env_backslash_at_end() {
        let content = r#"KEY="value\""#;
        let vars = parse_env_variables(content).unwrap();
        assert_eq!(vars[0].value, "value\\");
    }

    #[test]
    fn test_parse_env_unquoted_special() {
        let content = "KEY=value$with$special\nOTHER=simple";
        let vars = parse_env_variables(content).unwrap();
        assert_eq!(vars[0].value, "value$with$special");
        assert_eq!(vars[1].value, "simple");
    }
}
