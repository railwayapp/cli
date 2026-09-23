use std::collections::BTreeMap;

use anyhow::{Result, bail};
use clap::Parser;
use is_terminal::IsTerminal;
use serde_json::json;

use crate::{
    client::GQLClient,
    iac::ownership::{self, Preview},
    util::prompt::prompt_confirm_with_default,
};

const GUIDANCE: &str = "Update your authoring configuration and re-plan before applying it. Applying an old named-partial configuration can reclaim released ownership. Ordinary named-partial apply can restore ownership later. Whole-project planning requires all named ownership to be cleared; then remove the named partial export and create a fresh whole-project plan.";

#[derive(Parser)]
#[clap(
    after_help = "Uses the linked environment and does not require an authoring file. Release and transfer require environment ADMIN access and only change ownership metadata: resources are not changed, deleted, or redeployed.\n\nExamples:\n  railway config partials list\n  railway config partials transfer legacy-ops operations --resource service.api --resource service.admin --dry-run\n  railway config partials release operations --dry-run\n\nUpdate the authoring configuration and re-plan after changing ownership. Applying an old named-partial configuration can reclaim released ownership."
)]
pub(super) struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Parser)]
enum Command {
    /// List partials and their owned resource addresses in the linked environment
    List(ListArgs),
    /// Release selected resources, or the entire partial when --resource is omitted
    Release(ReleaseArgs),
    /// Transfer selected resources, or the entire source when --resource is omitted
    Transfer(TransferArgs),
}

#[derive(Parser)]
struct ListArgs {
    /// Output the complete address-to-owner map as JSON
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
#[clap(after_help = GUIDANCE)]
struct ReleaseArgs {
    /// Named partial to release; use `railway config partials list` to discover names
    #[clap(value_parser = ownership::named_partial)]
    partial: String,
    #[clap(flatten)]
    options: ChangeArgs,
}

#[derive(Parser)]
#[clap(after_help = GUIDANCE)]
struct TransferArgs {
    /// Named partial that currently owns the resources
    #[clap(value_parser = ownership::named_partial)]
    from_partial: String,
    /// New or existing named partial to receive ownership
    #[clap(value_parser = ownership::named_partial)]
    to_partial: String,
    #[clap(flatten)]
    options: ChangeArgs,
}

#[derive(Parser)]
struct ChangeArgs {
    /// Exact owned address (for example service.api); repeat to select several. Omit for all.
    #[clap(long = "resource", value_name = "ADDRESS", action = clap::ArgAction::Append)]
    resources: Option<Vec<String>>,
    /// Preview exact affected addresses without changing ownership
    #[clap(long, conflicts_with = "yes")]
    dry_run: bool,
    /// Confirm the ownership change and proceed non-interactively
    #[clap(long)]
    yes: bool,
    /// Output JSON and proceed without prompting, as with `railway config apply --json`
    #[clap(long)]
    json: bool,
    /// Require the configEtag from `list`, or baseConfigEtag from a previous --dry-run
    #[clap(long, value_name = "ETAG")]
    base_config_etag: Option<String>,
}

pub(super) async fn command(args: Args) -> Result<()> {
    match args.command {
        Command::List(args) => list(args).await,
        Command::Release(args) => change(args.partial, None, args.options).await,
        Command::Transfer(args) => {
            change(args.from_partial, Some(args.to_partial), args.options).await
        }
    }
}

fn interactive(json: bool) -> bool {
    !json && std::io::stdout().is_terminal() && std::io::stdin().is_terminal()
}

async fn list(args: ListArgs) -> Result<()> {
    let (configs, linked, _, _) =
        super::runner::ensure_config_context_with_prompt(interactive(args.json)).await?;
    let client = GQLClient::new_authorized(&configs)?;
    let snapshot =
        ownership::fetch_snapshot(&client, &configs.get_backboard(), linked.environment_id()?)
            .await?;
    let owners = snapshot.iac_partials.unwrap_or_default();
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "environmentId": snapshot.id,
                "environmentName": snapshot.name,
                "configEtag": snapshot.config_etag,
                "iacPartials": owners,
                "wholeProjectAvailable": ownership::whole_project_available(&owners),
            }))?
        );
    } else {
        println!("IaC ownership in {} ({})", snapshot.name, snapshot.id);
        let mut partials: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (address, owner) in &owners {
            partials.entry(owner).or_default().push(address);
        }
        if partials.is_empty() {
            println!("No named partial ownership.");
        }
        for (partial, addresses) in partials {
            println!("\n{partial}");
            for address in addresses {
                println!("  {address}");
            }
        }
        println!(
            "\n{}",
            availability(ownership::whole_project_available(&owners))
        );
    }
    Ok(())
}

async fn change(from: String, to: Option<String>, args: ChangeArgs) -> Result<()> {
    if to.as_ref() == Some(&from) {
        bail!("The source and destination partials must differ.");
    }
    let interactive = interactive(args.json);
    if !args.dry_run && !args.yes && !args.json && !interactive {
        bail!("Review with --dry-run, then use --yes to change ownership non-interactively.");
    }
    let (configs, linked, _, _) =
        super::runner::ensure_config_context_with_prompt(interactive && !args.yes).await?;
    let client = GQLClient::new_authorized(&configs)?;
    let endpoint = configs.get_backboard();
    let snapshot = ownership::fetch_snapshot(&client, &endpoint, linked.environment_id()?).await?;
    let mut preview = ownership::preview(
        snapshot,
        &from,
        to.as_deref(),
        args.resources.as_deref(),
        args.base_config_etag.as_deref(),
    )?;
    if !args.json {
        print_preview(&preview);
    }
    if !args.dry_run {
        if !args.yes && !args.json && !prompt_confirm_with_default("Change this ownership?", false)?
        {
            bail!("No ownership changed.");
        }
        preview.result = ownership::apply(&client, &endpoint, &preview).await?;
    }
    if args.json {
        let mut output = serde_json::to_value(&preview)?;
        output["ok"] = json!(true);
        output["dryRun"] = json!(args.dry_run);
        output["wholeProjectAvailable"] = json!(ownership::whole_project_available(
            &preview.result.iac_partials
        ));
        output["guidance"] = json!(GUIDANCE);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if args.dry_run {
        println!("\nDry run: no ownership changed.");
    } else {
        println!(
            "\nOwnership changed for {} resource(s).",
            preview.result.affected_resources.len()
        );
        println!(
            "{}",
            availability(ownership::whole_project_available(
                &preview.result.iac_partials
            ))
        );
    }
    Ok(())
}

fn print_preview(preview: &Preview) {
    let action = if let Some(to) = &preview.to_partial {
        format!(
            "Transfer ownership from {:?} to {to:?}",
            preview.from_partial
        )
    } else {
        format!("Release ownership from {:?}", preview.from_partial)
    };
    println!(
        "{action} in {} ({})",
        preview.environment_name, preview.environment_id
    );
    for address in &preview.result.affected_resources {
        println!("  {address}");
    }
    println!(
        "\nOnly ownership metadata changes. Resources are not changed, deleted, or redeployed."
    );
    println!("Base config etag: {}", preview.base_config_etag);
    println!(
        "After this change: {}",
        availability(ownership::whole_project_available(
            &preview.result.iac_partials
        ))
    );
    println!("\n{GUIDANCE}");
}

fn availability(available: bool) -> &'static str {
    if available {
        "No named ownership remains; whole-project planning is available."
    } else {
        "Named ownership remains; whole-project planning requires all named ownership to be cleared."
    }
}
