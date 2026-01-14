use super::*;
use crate::{
    controllers::{
        project::resolve_service_context,
        variables::{Variable, get_service_variables},
    },
    table::Table,
    util::progress::create_spinner_if,
};
use anyhow::bail;
use std::io::{IsTerminal, Read};

/// Manage environment variables for a service
#[derive(Parser)]
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

    /// Show variables in KV format
    #[clap(short, long)]
    kv: bool,

    /// The "{key}={value}" environment variable pair to set the service variables (legacy, use 'variable set' instead)
    #[clap(long)]
    set: Vec<Variable>,

    /// Set a variable with the value read from stdin (legacy, use 'variable set --stdin' instead)
    #[clap(long, value_name = "KEY")]
    set_from_stdin: Option<String>,

    /// Output in JSON format
    #[clap(long)]
    json: bool,

    /// Skip triggering deploys when setting variables
    #[clap(long)]
    skip_deploys: bool,
}

#[derive(Parser)]
enum Commands {
    /// List variables for a service
    #[clap(alias = "ls")]
    List(ListArgs),

    /// Set a variable
    Set(SetArgs),

    /// Delete a variable
    #[clap(alias = "rm", alias = "remove")]
    Delete(DeleteArgs),
}

#[derive(Parser)]
struct ListArgs {
    /// The service to list variables for
    #[clap(short, long)]
    service: Option<String>,

    /// The environment to list variables from
    #[clap(short, long)]
    environment: Option<String>,

    /// Show variables in KV format
    #[clap(short, long)]
    kv: bool,

    /// Output in JSON format
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

    /// Output in JSON format
    #[clap(long)]
    json: bool,
}

pub async fn command(args: Args) -> Result<()> {
    if let Some(cmd) = args.command {
        return match cmd {
            Commands::List(list_args) => list_variables(list_args).await,
            Commands::Set(set_args) => set_variable(set_args).await,
            Commands::Delete(delete_args) => delete_variable(delete_args).await,
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
            args.skip_deploys,
        )
        .await;
    }

    // Legacy behavior: handle --set flag
    if !args.set.is_empty() {
        return set_variables_legacy(args.set, args.service, args.environment, args.skip_deploys)
            .await;
    }

    // Legacy behavior: list variables (default)
    list_variables(ListArgs {
        service: args.service,
        environment: args.environment,
        kv: args.kv,
        json: args.json,
    })
    .await
}

async fn list_variables(args: ListArgs) -> Result<()> {
    let ctx = resolve_service_context(args.service, args.environment).await?;

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
        args.skip_deploys,
        args.json,
    )
    .await
}

async fn delete_variable(args: DeleteArgs) -> Result<()> {
    let ctx = resolve_service_context(args.service, args.environment).await?;

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
    skip_deploys: bool,
) -> Result<()> {
    set_variables_internal(variables, service, environment, skip_deploys, false).await
}

async fn set_variables_internal(
    variables: Vec<Variable>,
    service: Option<String>,
    environment: Option<String>,
    skip_deploys: bool,
    json: bool,
) -> Result<()> {
    let ctx = resolve_service_context(service, environment).await?;

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
