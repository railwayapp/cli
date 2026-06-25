use super::*;
use crate::{
    controllers::signals::{
        create_signal, list_signals, parse_expression, parse_signal_type, parse_signal_value,
        resolve_owner, set_signal_rule, unset_signal_rule,
    },
    util::progress::create_spinner_if,
};

/// Manage Railway Signals (feature flags)
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway signal list --json\n  railway signal create checkout.v2 --type bool --default false\n  railway signal set checkout.v2 true --when '{\"attr\":\"plan\",\"op\":\"eq\",\"value\":\"enterprise\"}'\n  railway signal unset checkout.v2 --rule-id enterprise-on\n"
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Owner scope (defaults to linked project's workspace)
    #[clap(long, global = true)]
    owner: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,
}

#[derive(Parser)]
enum Commands {
    /// List signals for an owner scope
    #[clap(visible_alias = "ls")]
    List,

    /// Register a new signal
    Create(CreateArgs),

    /// Set a rule on a signal (RFC: signals set <name> <value> when <expression>)
    Set(SetArgs),

    /// Remove a rule from a signal
    #[clap(visible_alias = "rm", visible_alias = "remove")]
    Unset(UnsetArgs),
}

#[derive(Parser)]
struct CreateArgs {
    /// Signal name
    name: String,

    /// Signal type: bool, string, number, json
    #[clap(long)]
    r#type: String,

    /// Canonical default value
    #[clap(long)]
    default: String,
}

#[derive(Parser)]
struct SetArgs {
    /// Signal name
    name: String,

    /// Literal value when the expression matches
    value: String,

    /// JSON expression (Radar clause shape or bucket compare)
    #[clap(long)]
    when: String,

    /// Stable rule id (defaults to a hash of name + expression)
    #[clap(long)]
    rule_id: Option<String>,
}

#[derive(Parser)]
struct UnsetArgs {
    /// Signal name
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
        Commands::List => list_command(&client, &configs, owner, args.json).await,
        Commands::Create(create_args) => {
            create_command(&client, &configs, owner, create_args, args.json).await
        }
        Commands::Set(set_args) => {
            set_command(&client, &configs, owner, set_args, args.json).await
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
    json: bool,
) -> Result<()> {
    let signals = list_signals(client, configs, owner.clone()).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&signals)?);
        return Ok(());
    }

    if signals.is_empty() {
        eprintln!("No signals found for {owner}");
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

async fn create_command(
    client: &reqwest::Client,
    configs: &Configs,
    owner: String,
    args: CreateArgs,
    json: bool,
) -> Result<()> {
    let spinner = create_spinner_if(!json, format!("Creating {}...", args.name.bold()));
    let signal = create_signal(
        client,
        configs,
        owner,
        args.name.clone(),
        parse_signal_type(&args.r#type)?,
        parse_signal_value(&args.default)?,
    )
    .await?;

    if let Some(sp) = spinner {
        sp.finish_with_message(format!("Created signal {}", args.name.bold()));
    } else {
        println!("{}", serde_json::to_string_pretty(&signal)?);
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
    let value = parse_signal_value(&args.value)?;
    let rule_id = args.rule_id.unwrap_or_else(|| default_rule_id(&args.name, &args.when));

    let spinner = create_spinner_if(
        !json,
        format!("Setting rule on {}...", args.name.bold()),
    );
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
        sp.finish_with_message(format!("Updated signal {}", args.name.bold()));
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
    let spinner = create_spinner_if(
        !json,
        format!("Removing rule from {}...", args.name.bold()),
    );
    let signal = unset_signal_rule(
        client,
        configs,
        owner,
        args.name.clone(),
        args.rule_id.clone(),
    )
    .await?;

    if let Some(sp) = spinner {
        sp.finish_with_message(format!("Updated signal {}", args.name.bold()));
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
