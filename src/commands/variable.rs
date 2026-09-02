use super::*;
use crate::{
    controllers::{
        project::resolve_service_context,
        variable_edit::{
            VarChange, applyable_changes, demo_snapshot, diff_edit_snapshot, open_in_editor,
            parse_edit_document, print_variable_plan, temp_edit_path, write_edit_document,
        },
        variables::{
            EditSnapshot, SEALED_TOKEN, Variable, get_service_variables,
            get_service_variables_for_edit, get_service_variables_including_sealed,
            reject_reserved_keys,
        },
    },
    table::Table,
    util::{progress::create_spinner_if, prompt::prompt_confirm_with_default},
};
use anyhow::{Context, bail};
use std::collections::BTreeMap;
use std::fs;
use std::io::{IsTerminal, Read};

/// Manage environment variables for a service
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway variable list --service api --json\n  railway variable list --service api --kv\n  railway variable set API_URL=https://example.com --skip-deploys --json\n  echo \"secret\" | railway variable set API_KEY --stdin --skip-deploys --json\n  railway variable delete API_KEY --service api --json\n  railway variable edit\n  railway variable edit --demo\n\nAutomation notes:\n  JSON and KV output include raw variable values. Avoid sharing command output from secret-bearing variable commands.\n  For idempotent deletes, list variables first, check whether the key exists, then delete it.\n  Sealed variables are listed by name with no value (null in JSON, <sealed> in the table). They are already set and nobody can read them back - do not recreate them.\n  `variable edit` opens $EDITOR, then shows an IaC-style diff and asks for confirmation before applying."
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

    /// Bulk-edit variables in $EDITOR, then confirm an IaC-style diff
    Edit(EditArgs),
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
struct EditArgs {
    /// The service to edit variables for
    #[clap(short, long)]
    service: Option<String>,

    /// The environment to edit variables in
    #[clap(short, long)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,

    /// Skip the confirmation prompt and apply the diff
    #[clap(short = 'y', long)]
    yes: bool,

    /// Skip triggering deploys when applying variable changes
    #[clap(long)]
    skip_deploys: bool,

    /// Show plaintext values in the diff instead of redacting them
    #[clap(long)]
    reveal: bool,

    /// Offline prototype: edit fixture variables and print the would-be apply (no API)
    #[clap(long)]
    demo: bool,

    /// Allow destructive deletes in non-interactive or agent sessions
    #[clap(long)]
    confirm_destructive: bool,
}

pub async fn command(args: Args) -> Result<()> {
    if let Some(cmd) = args.command {
        return match cmd {
            Commands::List(list_args) => list_variables(list_args).await,
            Commands::Set(set_args) => set_variable(set_args).await,
            Commands::Delete(delete_args) => delete_variable(delete_args).await,
            Commands::Edit(edit_args) => edit_variables(edit_args).await,
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

    // Sealed variables are listed by name with no value. Hiding them entirely
    // made them look unset, so agents and scripts would recreate a variable
    // that was already there, or stall waiting for one that already existed.
    let variables = get_service_variables_including_sealed(
        &ctx.client,
        &ctx.configs,
        ctx.project.id.clone(),
        ctx.environment_id,
        ctx.service_id,
    )
    .await?;

    if args.kv {
        for (key, value) in &variables {
            match value {
                Some(value) => println!("{key}={value}"),
                // A comment, not `KEY=`: this output is meant to be sourced,
                // and an empty string is not what the variable is set to.
                None => println!("# {key} is sealed; its value cannot be read"),
            }
        }
        return Ok(());
    }

    if args.json {
        // Sealed variables serialize as `null`, matching the API.
        println!("{}", serde_json::to_string_pretty(&variables)?);
        return Ok(());
    }

    if variables.is_empty() {
        eprintln!("No variables found");
        return Ok(());
    }

    let rows = variables
        .into_iter()
        .map(|(key, value)| (key, value.unwrap_or_else(|| SEALED_TOKEN.to_string())))
        .collect();

    let table = Table::new(ctx.service_name, rows);
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

    // Including sealed: a sealed variable is deletable, it just cannot be read.
    let variables = get_service_variables_including_sealed(
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
        // The spinner draws to stderr and draws nothing at all when stderr
        // is not a terminal, so a scripted/piped run used to succeed in
        // complete silence. Give it a plain stdout confirmation instead.
        if !std::io::stderr().is_terminal() {
            println!("Set variables {}", keys.join(", "));
        }
    } else {
        println!("{}", serde_json::json!({"keys": keys, "set": true}));
    }

    Ok(())
}

async fn edit_variables(args: EditArgs) -> Result<()> {
    if args.demo {
        return edit_variables_demo(args).await;
    }

    let yes = args.yes;
    let reveal = args.reveal;
    let skip_deploys = args.skip_deploys;
    let confirm_destructive = args.confirm_destructive;

    let ctx = resolve_service_context(args.project, args.service, args.environment).await?;
    let before = get_service_variables_for_edit(
        &ctx.client,
        &ctx.configs,
        ctx.project.id.clone(),
        ctx.environment_id.clone(),
        ctx.service_id.clone(),
    )
    .await?;

    let scope = format!(
        "project={}  environment={}  service={}",
        ctx.project.name, ctx.environment_name, ctx.service_name
    );
    let after = run_editor_loop(
        &before,
        &ctx.service_name,
        &[
            "railway variable edit",
            scope.as_str(),
            "Save and quit to review a diff. Abort the editor (non-zero exit) to cancel.",
            "Delete a line to remove a variable. Leave sealed variables as <sealed> unless rotating.",
            "Railway-provided variables are listed as comments and cannot be edited.",
        ],
    )?;

    let changes = diff_edit_snapshot(&before, &after);
    if changes.is_empty() {
        print_variable_plan(&ctx.service_name, &changes, reveal);
        return Ok(());
    }

    // Reject unapplyable edits before showing a plan the user cannot act on.
    reject_reserved_keys(&changes)?;
    let (upserts, deletes) = applyable_changes(&before, &changes)?;

    print_variable_plan(&ctx.service_name, &changes, reveal);
    guard_destructive_apply(yes, confirm_destructive, &changes)?;

    let apply_args = EditApplyArgs { yes, skip_deploys };
    if !confirm_variable_plan(&changes, &apply_args)? {
        bail!("No changes applied.");
    }

    apply_variable_changes(&ctx, upserts, deletes, changes.len(), skip_deploys, false).await?;

    Ok(())
}

async fn edit_variables_demo(args: EditArgs) -> Result<()> {
    let service = args.service.as_deref().unwrap_or("api");
    let yes = args.yes;
    let reveal = args.reveal;
    let skip_deploys = args.skip_deploys;
    let confirm_destructive = args.confirm_destructive;
    let before = demo_snapshot();

    eprintln!(
        "{}",
        "Demo mode — offline fixture, nothing will be written to Railway.".dimmed()
    );

    let after = run_editor_loop(
        &before,
        service,
        &[
            "railway variable edit --demo",
            "project=demo  environment=production  service=api",
            "Save and quit to review a diff. Abort the editor (non-zero exit) to cancel.",
            "Try: change LOG_LEVEL, add FEATURE_NEW=1, delete FEATURE_OLD, rotate STRIPE_SECRET_KEY",
        ],
    )?;

    let changes = diff_edit_snapshot(&before, &after);
    if changes.is_empty() {
        print_variable_plan(service, &changes, reveal);
        return Ok(());
    }

    reject_reserved_keys(&changes)?;
    let (upserts, deletes) = applyable_changes(&before, &changes)?;

    print_variable_plan(service, &changes, reveal);
    guard_destructive_apply(yes, confirm_destructive, &changes)?;

    let apply_args = EditApplyArgs { yes, skip_deploys };
    if !confirm_variable_plan(&changes, &apply_args)? {
        bail!("No changes applied.");
    }

    println!();
    println!("{}", "Would apply (demo — skipped):".bold());
    for key in upserts.keys() {
        println!("  • set {}", key.cyan());
    }
    for key in &deletes {
        println!("  • delete {}", key.cyan());
    }
    if skip_deploys {
        println!("{}", "  (deploys would be skipped)".dimmed());
    } else {
        println!("{}", "  (would trigger a redeploy)".dimmed());
    }

    Ok(())
}

fn run_editor_loop(
    before: &EditSnapshot,
    service: &str,
    header_lines: &[&str],
) -> Result<BTreeMap<String, crate::controllers::variables::EditVariableEntry>> {
    if !std::io::stdin().is_terminal()
        && std::env::var_os("EDITOR").is_none()
        && std::env::var_os("VISUAL").is_none()
    {
        bail!(
            "variable edit requires a TTY (or set $EDITOR). For an offline taste:\n  EDITOR=vim railway variable edit --demo"
        );
    }

    let path = temp_edit_path(service);
    write_edit_document(&path, before, header_lines)?;

    eprintln!(
        "{} {}",
        "Editing".dimmed(),
        path.display().to_string().cyan()
    );
    eprintln!(
        "{}",
        "Opening $EDITOR — save and quit to continue, abort to cancel.".dimmed()
    );

    let edit_result = open_in_editor(&path);
    let contents = fs::read_to_string(&path).ok();
    let _ = fs::remove_file(&path);
    edit_result?;

    let contents = contents.context("Failed to read edited variables file")?;
    parse_edit_document(&contents)
}

struct EditApplyArgs {
    yes: bool,
    skip_deploys: bool,
}

fn guard_destructive_apply(
    yes: bool,
    confirm_destructive: bool,
    changes: &[VarChange],
) -> Result<()> {
    let destructive = changes.iter().any(|c| c.is_destructive());
    if !destructive || confirm_destructive {
        return Ok(());
    }

    if yes || !std::io::stdout().is_terminal() || crate::telemetry::is_agent() {
        bail!(
            "Destructive variable deletes require explicit confirmation. Review the plan, then re-run with `--confirm-destructive` if the removals are expected."
        );
    }

    Ok(())
}

fn confirm_variable_plan(changes: &[VarChange], args: &EditApplyArgs) -> Result<bool> {
    if args.yes {
        return Ok(true);
    }

    if !std::io::stdout().is_terminal() {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Re-run with --yes after reviewing the plan."
        );
    }

    println!();
    let destructive = changes.iter().any(|c| c.is_destructive());
    let prompt = if destructive {
        if args.skip_deploys {
            "Apply these changes? This will remove variables."
        } else {
            "Apply these changes? This will remove variables and may redeploy."
        }
    } else if args.skip_deploys {
        "Apply these variable changes?"
    } else {
        "Apply these variable changes? This may redeploy the service."
    };

    // Default No — :wq alone is not enough.
    prompt_confirm_with_default(prompt, false)
}

async fn apply_variable_changes(
    ctx: &crate::controllers::project::ServiceContext,
    upserts: BTreeMap<String, String>,
    deletes: Vec<String>,
    change_count: usize,
    skip_deploys: bool,
    json: bool,
) -> Result<()> {
    let touched_keys: Vec<String> = upserts.keys().cloned().chain(deletes.clone()).collect();

    let spinner = create_spinner_if(!json, "Applying variable changes...".to_string());

    if !upserts.is_empty() {
        let vars = mutations::variable_collection_upsert::Variables {
            project_id: ctx.project_id.clone(),
            environment_id: ctx.environment_id.clone(),
            service_id: ctx.service_id.clone(),
            variables: upserts,
            skip_deploys: skip_deploys.then_some(true),
        };
        post_graphql::<mutations::VariableCollectionUpsert, _>(
            &ctx.client,
            ctx.configs.get_backboard(),
            vars,
        )
        .await?;
    }

    for key in &deletes {
        let vars = mutations::variable_delete::Variables {
            project_id: ctx.project_id.clone(),
            environment_id: ctx.environment_id.clone(),
            name: key.clone(),
            service_id: Some(ctx.service_id.clone()),
        };
        post_graphql::<mutations::VariableDelete, _>(
            &ctx.client,
            ctx.configs.get_backboard(),
            vars,
        )
        .await?;
    }

    if let Some(sp) = spinner {
        sp.finish_with_message(format!(
            "Applied {} variable change(s)",
            change_count.to_string().bold()
        ));
    } else {
        println!(
            "{}",
            serde_json::json!({
                "applied": change_count,
                "keys": touched_keys,
            })
        );
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
