use std::io::IsTerminal;

use anyhow::{Context, bail};
use clap::CommandFactory;
use colored::Colorize;
use serde_json::json;

use crate::{
    controllers::staged_changes::{
        DeployWaitResult, PrettyChange, StagedChangesView, deleted_resources,
        deploy_staged_changes, discard_all_staged_changes, discard_staged_change_paths,
        filter_view_by_paths, is_empty_patch, load_staged_changes, output_json,
        patch_requires_two_factor, print_status, render_status_text, resolve_environment_context,
    },
    errors::ExitCode,
    util::{
        progress::create_spinner_if,
        prompt::{prompt_confirm_with_default, prompt_multi_options},
        two_factor::validate_two_factor_if_enabled,
    },
};

use super::*;

/// Review, deploy, and discard staged environment changes
#[derive(Parser)]
#[clap(
    after_help = "Aliases:\n  railway staged-changes, railway change\n\nExamples:\n\n  railway changes status\n  railway changes status --path 'services.svc_123' --json\n  railway changes deploy --yes --json\n  railway changes discard --path services.svc_123.deploy.ipv6EgressEnabled --yes\n  railway changes discard --path 'services.svc_123' --yes\n  railway changes discard --all --yes --json\n\nAutomation notes:\n  Staged changes are environment-scoped. Use `railway changes status --json` to inspect labels, paths, current values, and new values before deploying or discarding.\n  A path prefix matches its whole subtree; `*` matches one segment. Escape literal dots in variable names as '\\.'.\n  Variable values are masked in the table; pass --show-values for plaintext. JSON output always contains full values (sealed variables are null)."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,

    /// Project ID/name to use (defaults to linked project)
    #[clap(long, short = 'p', global = true)]
    project: Option<String>,

    /// Environment name or ID to use (defaults to linked environment)
    #[clap(long, short, global = true)]
    environment: Option<String>,

    /// Output in JSON format
    #[clap(long, global = true)]
    json: bool,
}

#[derive(Parser)]
enum Commands {
    /// Show staged changes
    Status(StatusArgs),

    /// Deploy all staged changes
    Deploy(DeployArgs),

    /// Alias for deploy
    Apply(DeployArgs),

    /// Discard staged changes
    Discard(DiscardArgs),
}

#[derive(Parser)]
struct StatusArgs {
    /// Exit 2 when staged changes are pending, 0 when none
    #[clap(long)]
    detailed_exit_code: bool,

    /// Only show changes matching this dot path (prefix matches the subtree,
    /// `*` matches one segment). May be passed more than once.
    #[clap(long = "path", action = clap::ArgAction::Append)]
    paths: Vec<String>,

    /// Show variable values in plaintext instead of masked
    #[clap(long)]
    show_values: bool,
}

#[derive(Parser, Clone)]
struct DeployArgs {
    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    yes: bool,

    /// Commit message for the staged changes
    #[clap(long, short)]
    message: Option<String>,

    /// Commit staged changes without triggering deploys
    #[clap(long)]
    skip_deploys: bool,

    /// 2FA code for verification (required for deletions and volume region
    /// moves when 2FA is enabled)
    #[clap(long = "2fa-code")]
    two_factor_code: Option<String>,
}

#[derive(Parser)]
struct DiscardArgs {
    /// Discard all staged changes
    #[clap(long, conflicts_with = "paths")]
    all: bool,

    /// Dot path to discard (prefix matches the subtree, `*` matches one
    /// segment). May be passed more than once.
    #[clap(long = "path", action = clap::ArgAction::Append)]
    paths: Vec<String>,

    /// Skip confirmation dialog
    #[clap(short = 'y', long = "yes")]
    yes: bool,
}

pub async fn command(args: Args) -> Result<()> {
    crate::util::reporter::set_mode(args.json);

    let Some(command) = args.command else {
        Args::command().print_help()?;
        println!();
        return Ok(());
    };

    match command {
        Commands::Status(status_args) => {
            status(args.project, args.environment, args.json, status_args).await
        }
        Commands::Deploy(deploy_args) | Commands::Apply(deploy_args) => {
            deploy(args.project, args.environment, args.json, deploy_args).await
        }
        Commands::Discard(discard_args) => {
            discard(args.project, args.environment, args.json, discard_args).await
        }
    }
}

async fn status(
    project: Option<String>,
    environment: Option<String>,
    json_output: bool,
    args: StatusArgs,
) -> Result<()> {
    let ctx = resolve_environment_context(project, environment).await?;
    let view = load_staged_changes(&ctx).await?;
    let view = filter_view_by_paths(&view, &args.paths)?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&output_json(&view))?);
    } else {
        print_status(&view, args.show_values);
    }

    if args.detailed_exit_code && view.pretty.total_changes > 0 {
        return Err(ExitCode(2).into());
    }

    Ok(())
}

async fn deploy(
    project: Option<String>,
    environment: Option<String>,
    json_output: bool,
    args: DeployArgs,
) -> Result<()> {
    let ctx = resolve_environment_context(project, environment).await?;
    let view = load_staged_changes(&ctx).await?;

    if is_empty_patch(&view.patch.patch) {
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "deployed": false,
                    "environmentId": view.environment_id,
                    "environmentName": view.environment_name,
                    "message": "No staged changes to deploy",
                }))?
            );
        } else {
            println!(
                "No staged changes to deploy for environment {}.",
                view.environment_name.magenta().bold()
            );
        }
        return Ok(());
    }

    if view.patch.status == "APPLYING" {
        bail!(
            "Staged changes for environment {} are currently being applied. Check progress with `railway changes status`.",
            view.environment_name
        );
    }

    let stdout_is_terminal = std::io::stdout().is_terminal();
    warn_destructive_changes(&view, json_output);
    confirm_deploy(&view, args.yes, json_output)?;

    if patch_requires_two_factor(&view.patch.patch, &view.current_config) {
        validate_two_factor_if_enabled(
            &ctx.client,
            ctx.configs.as_ref(),
            stdout_is_terminal,
            args.two_factor_code.clone(),
        )
        .await?;
    }

    let spinner = create_spinner_if(
        !json_output && stdout_is_terminal,
        "Applying staged changes...".into(),
    );

    let outcome = deploy_staged_changes(
        &ctx,
        args.message.clone(),
        args.skip_deploys.then_some(true),
    )
    .await
    .map_err(|error| commit_error_with_hint(error, args.skip_deploys));

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(spinner) = spinner {
                spinner.finish_and_clear();
            }
            return Err(error);
        }
    };

    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }

    match outcome.wait {
        DeployWaitResult::Committed => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "deployed": true,
                        "environmentId": ctx.environment_id,
                        "environmentName": ctx.environment_name,
                        "workflowId": outcome.workflow_id,
                        "message": args.message,
                        "skipDeploys": args.skip_deploys,
                    }))?
                );
            } else {
                println!(
                    "{} {}.",
                    "Deployed staged changes for".green(),
                    ctx.environment_name.magenta().bold()
                );
            }
            Ok(())
        }
        DeployWaitResult::Pending => {
            // The commit was accepted; only progress reporting timed out.
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "deployed": false,
                        "pending": true,
                        "environmentId": ctx.environment_id,
                        "environmentName": ctx.environment_name,
                        "workflowId": outcome.workflow_id,
                        "message": "Commit accepted, changes are still applying",
                        "next": ["railway changes status"],
                    }))?
                );
            } else {
                println!(
                    "{} Check progress with {}.",
                    "Commit accepted — changes are still applying.".yellow(),
                    "railway changes status".cyan().bold()
                );
            }
            Ok(())
        }
        DeployWaitResult::Failed(detail) => {
            bail!("Failed to deploy staged changes: {detail}");
        }
    }
}

/// Wraps commit errors with actionable hints for known backend rejections.
fn commit_error_with_hint(error: anyhow::Error, skip_deploys: bool) -> anyhow::Error {
    let message = error.to_string();
    if skip_deploys && message.to_lowercase().contains("volume") {
        return error.context(
            "The backend rejected committing without deploys (new volume mounts only take effect on deploy). Re-run without --skip-deploys.",
        );
    }
    if message.contains("No patch to apply") {
        return error.context("Nothing is staged for this environment.");
    }
    error
}

fn warn_destructive_changes(view: &StagedChangesView, json_output: bool) {
    let deleted = deleted_resources(view);
    if deleted.is_empty() || json_output {
        return;
    }

    eprintln!(
        "{} This deploy deletes {}:",
        "Warning:".red().bold(),
        if deleted.len() == 1 {
            "a resource".to_string()
        } else {
            format!("{} resources", deleted.len())
        }
    );
    for resource in &deleted {
        eprintln!(
            "  {} {} ({})",
            "-".red(),
            resource.name.bold(),
            resource.kind
        );
    }
    eprintln!();
}

fn confirm_deploy(view: &StagedChangesView, yes: bool, json_output: bool) -> Result<()> {
    if yes {
        return Ok(());
    }

    if std::io::stdout().is_terminal() {
        if !json_output {
            print_status(view, false);
        }
        let confirmed = prompt_confirm_with_default("Deploy these staged changes?", false)?;
        if !confirmed {
            bail!("No staged changes deployed.");
        }
        Ok(())
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to deploy staged changes."
        )
    }
}

async fn discard(
    project: Option<String>,
    environment: Option<String>,
    json_output: bool,
    args: DiscardArgs,
) -> Result<()> {
    let ctx = resolve_environment_context(project, environment).await?;
    let view = load_staged_changes(&ctx).await?;

    if is_empty_patch(&view.patch.patch) {
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "discarded": false,
                    "environmentId": view.environment_id,
                    "environmentName": view.environment_name,
                    "message": "No staged changes to discard",
                }))?
            );
        } else {
            println!(
                "No staged changes to discard for environment {}.",
                view.environment_name.magenta().bold()
            );
        }
        return Ok(());
    }

    let discard_selection = resolve_discard_selection(&view, &args)?;
    confirm_discard(&view, &discard_selection, args.yes, json_output)?;

    match discard_selection {
        DiscardSelection::All => {
            let updated = discard_all_staged_changes(&ctx).await?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "discarded": true,
                        "discardedChanges": view.pretty.total_changes,
                        "environmentId": ctx.environment_id,
                        "environmentName": ctx.environment_name,
                        "patchId": updated.id,
                        "status": updated.status,
                    }))?
                );
            } else {
                println!(
                    "{} all staged changes for {}.",
                    "Discarded".green(),
                    ctx.environment_name.magenta().bold()
                );
            }
        }
        DiscardSelection::Paths(paths) => {
            let (updated, discarded) = discard_staged_change_paths(&ctx, &paths).await?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "discarded": true,
                        "discardedChanges": discarded,
                        "paths": paths,
                        "environmentId": ctx.environment_id,
                        "environmentName": ctx.environment_name,
                        "patchId": updated.id,
                        "status": updated.status,
                    }))?
                );
            } else {
                println!(
                    "{} {} staged {} for {}.",
                    "Discarded".green(),
                    discarded,
                    if discarded == 1 { "change" } else { "changes" },
                    ctx.environment_name.magenta().bold()
                );
            }
        }
    }

    Ok(())
}

enum DiscardSelection {
    All,
    Paths(Vec<String>),
}

fn resolve_discard_selection(
    view: &StagedChangesView,
    args: &DiscardArgs,
) -> Result<DiscardSelection> {
    if args.all {
        return Ok(DiscardSelection::All);
    }
    if !args.paths.is_empty() {
        return Ok(DiscardSelection::Paths(args.paths.clone()));
    }

    if !std::io::stdout().is_terminal() {
        bail!(
            "Cannot prompt for staged changes in non-interactive mode. Use --all or --path with --yes."
        );
    }

    let choices = discard_choices(view);
    let labels = choices
        .iter()
        .map(|choice| choice.label.clone())
        .collect::<Vec<_>>();
    let selected = prompt_multi_options("Select staged changes to discard", labels)?;
    if selected.is_empty() {
        bail!("No staged changes selected for discard.");
    }

    if selected.iter().any(|label| label == ALL_CHANGES_LABEL) {
        return Ok(DiscardSelection::All);
    }

    let mut paths = Vec::new();
    for label in selected {
        let choice = choices
            .iter()
            .find(|choice| choice.label == label)
            .with_context(|| format!("Unknown staged change selection: {label}"))?;
        paths.push(choice.path.clone());
    }

    Ok(DiscardSelection::Paths(paths))
}

fn confirm_discard(
    view: &StagedChangesView,
    selection: &DiscardSelection,
    yes: bool,
    json_output: bool,
) -> Result<()> {
    if yes {
        return Ok(());
    }

    if std::io::stdout().is_terminal() {
        if !json_output {
            println!("{}", render_status_text(view, false));
        }
        let message = match selection {
            DiscardSelection::All => "Discard all staged changes?",
            DiscardSelection::Paths(paths) if paths.len() == 1 => "Discard this staged change?",
            DiscardSelection::Paths(_) => "Discard these staged changes?",
        };
        let confirmed = prompt_confirm_with_default(message, false)?;
        if !confirmed {
            bail!("No staged changes discarded.");
        }
        Ok(())
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to discard staged changes."
        )
    }
}

const ALL_CHANGES_LABEL: &str = "All staged changes";

struct DiscardChoice {
    label: String,
    path: String,
}

fn discard_choices(view: &StagedChangesView) -> Vec<DiscardChoice> {
    let mut choices = vec![DiscardChoice {
        label: ALL_CHANGES_LABEL.into(),
        path: String::new(),
    }];
    choices.extend(
        view.pretty
            .groups
            .iter()
            .flat_map(|group| group.changes.iter())
            .map(|change| DiscardChoice {
                label: discard_label(change),
                path: change.path.clone(),
            }),
    );
    choices
}

fn discard_label(change: &PrettyChange) -> String {
    let info = change
        .additional_info
        .as_ref()
        .map(|info| format!(" ({info})"))
        .unwrap_or_default();
    format!(
        "{} {}{} - {}",
        change.change_type.symbol(),
        change.display_name,
        info,
        change.path
    )
}
