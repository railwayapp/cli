use super::*;
use crate::{
    controllers::signals::{
        UpsertFlagResult, get_signal, list_signals, parse_expression, parse_value_for_query_type,
        resolve_owner, set_signal_rule, unset_signal_rule, upsert_flag_default,
    },
    util::progress::create_spinner_if,
};

/// Manage feature flags (Railway Signals)
#[derive(Parser)]
#[command(subcommand_required = false, arg_required_else_help = true)]
#[clap(
    after_help = "Examples:\n\n  railway flag list\n  railway flag checkout.v2 true\n  railway flag theme \"blue\"\n  railway flag set checkout.v2 true --when '{\"attr\":\"plan\",\"op\":\"eq\",\"value\":\"enterprise\"}'\n  railway flag unset checkout.v2 --rule-id enterprise-on\n"
)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,

    /// Flag name (upsert default when no subcommand is given)
    name: Option<String>,

    /// Flag value (upsert default when no subcommand is given)
    value: Option<String>,

    /// Owner scope (defaults to linked project's workspace)
    #[clap(long, global = true)]
    owner: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,

    /// Flag type: bool, string, number, json (inferred from value when omitted)
    #[clap(long, global = true)]
    r#type: Option<String>,

    /// Allow replacing an existing flag's type (clears rules)
    #[clap(long, global = true)]
    force: bool,
}

#[derive(Parser)]
enum Commands {
    /// List feature flags for an owner scope
    #[clap(visible_alias = "ls")]
    List,

    /// Set a targeting rule on a flag
    Set(SetArgs),

    /// Remove a rule from a flag
    #[clap(visible_alias = "rm", visible_alias = "remove")]
    Unset(UnsetArgs),
}

#[derive(Parser)]
struct SetArgs {
    /// Flag name
    name: String,

    /// Value when the expression matches
    value: String,

    /// JSON expression (Radar clause or bucket compare)
    #[clap(long)]
    when: String,

    /// Stable rule id (defaults to a hash of name + expression)
    #[clap(long)]
    rule_id: Option<String>,
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
    let owner = resolve_owner(&client, &configs, args.owner).await?;

    match args.command {
        Some(Commands::List) => list_command(&client, &configs, owner, args.json).await,
        Some(Commands::Set(set_args)) => {
            set_command(&client, &configs, owner, set_args, args.json).await
        }
        Some(Commands::Unset(unset_args)) => {
            unset_command(&client, &configs, owner, unset_args, args.json).await
        }
        None => {
            let name = args
                .name
                .context("flag name required (e.g. `railway flag checkout.v2 true`)")?;
            let value = args
                .value
                .context("flag value required (e.g. `railway flag checkout.v2 true`)")?;
            upsert_command(
                &client,
                &configs,
                owner,
                name,
                value,
                args.r#type.as_deref(),
                args.force,
                args.json,
            )
            .await
        }
    }
}

async fn list_command(
    client: &reqwest::Client,
    configs: &Configs,
    owner: String,
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

    for signal in &signals {
        println!(
            "{}  type={:?}  version={}  default={}",
            signal.name.bold(),
            signal.type_,
            signal.version,
            signal.default
        );
    }
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
    let expression = parse_expression(&args.when)?;
    let existing = get_signal(client, configs, owner.clone(), args.name.clone())
        .await?
        .with_context(|| {
            format!(
                "flag {} not found; create it with `railway flag {} <value>`",
                args.name, args.name
            )
        })?;
    let value = parse_value_for_query_type(&args.value, &existing.type_)?;
    let rule_id = args
        .rule_id
        .unwrap_or_else(|| default_rule_id(&args.name, &args.when));

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
