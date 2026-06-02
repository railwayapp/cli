use std::{env, path::PathBuf, process::Stdio};

use is_terminal::IsTerminal;

use serde::Deserialize;
use serde_json::Value;
use tokio::{io::AsyncWriteExt, process::Command};

use crate::util::{progress::{create_spinner_if, fail_spinner, success_spinner}, prompt::prompt_confirm_with_default};

use super::*;

/// Preview or stage Railway IaC changes from .railway/railway.ts
#[derive(Parser)]
pub struct Args {
    /// Path to the Railway IaC file. Defaults to nearest .railway/railway.ts resolved by the runner.
    #[clap(long)]
    pub(super) file: Option<PathBuf>,

    /// Stage the proposed ChangeSet in Backboard.
    #[clap(long)]
    pub(super) stage: bool,

    /// Output raw runner JSON.
    #[clap(long)]
    pub(super) json: bool,

    /// Confirm prompts and proceed non-interactively.
    #[clap(long)]
    pub(super) yes: bool,

    #[clap(skip)]
    pub(super) apply: bool,

    /// Ask Backboard to decrypt variables while planning, when authorized.
    #[clap(long)]
    pub(super) decrypt_variables: bool,

    /// Include generated graph TypeScript types in runner output.
    #[clap(long)]
    pub(super) include_types: bool,

    /// Path to the TypeScript IaC runner binary. Defaults to RAILWAY_IAC_TS_BIN or railway-iac-ts.
    #[clap(long)]
    pub(super) runner: Option<String>,

    /// Show full change details.
    #[clap(long, alias = "full")]
    pub(super) verbose: bool,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RunnerResponse {
    pub(super) ok: bool,
    command: String,
    file: String,
    current_environment: Option<CurrentEnvironment>,
    pub(super) change_set: Option<ChangeSet>,
    diff: Option<String>,
    diagnostics: Vec<Diagnostic>,
    pub(super) current_graph: Option<DesiredGraph>,
    pub(super) desired_graph: Option<DesiredGraph>,
    staged_patch: Option<StagedPatch>,
    apply_result: Option<ChangeSetApplyResult>,
    deployment_id: Option<String>,
    staged_patch_id: Option<String>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentEnvironment {
    project_id: Option<String>,
    environment_id: String,
    environment_name: Option<String>,
}

#[derive(Deserialize, serde::Serialize)]
pub(super) struct ChangeSet {
    pub(super) changes: Vec<Change>,
}

#[derive(Deserialize, serde::Serialize)]
pub(super) struct Change {
    summary: Option<String>,
    severity: Option<String>,
    kind: Option<String>,
    details: Option<Vec<String>>,
}

#[derive(Deserialize, serde::Serialize)]
struct Diagnostic {
    severity: String,
    path: String,
    message: String,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangeSetApplyResult {
    id: String,
    status: String,
    changes: Vec<ChangeOperationResult>,
    diagnostics: Value,
    deployment_id: Option<String>,
    staged_patch_id: Option<String>,
}

#[derive(Deserialize, serde::Serialize)]
struct ChangeOperationResult {
    kind: String,
    path: Option<String>,
    summary: Option<String>,
    status: String,
    outputs: Option<Value>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DesiredGraph {
    pub(super) resources: Vec<DesiredResource>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DesiredResource {
    pub(super) address: Option<String>,
    pub(super) r#type: String,
    pub(super) name: String,
    pub(super) engine: Option<String>,
    pub(super) variables: Option<serde_json::Map<String, Value>>,
    pub(super) source: Option<Value>,
    pub(super) build: Option<Value>,
    pub(super) deploy: Option<Value>,
    pub(super) networking: Option<Value>,
    pub(super) config: Option<Value>,
}

#[derive(Deserialize, serde::Serialize)]
struct StagedPatch {
    id: String,
    #[allow(dead_code)]
    patch: Option<Value>,
}

pub(super) async fn run(args: &Args, command: &str) -> Result<RunnerResponse> {
    let (configs, linked_project, token, auth_type) = ensure_config_context().await?;
    invoke_runner(args, &configs, &linked_project, &token, auth_type, command).await
}

pub async fn command(args: Args) -> Result<()> {
    let (configs, linked_project, token, auth_type) = ensure_config_context().await?;
    let command = if args.stage { "stage" } else if args.apply || args.yes { "apply" } else { "plan" };

    if args.stage && !args.yes {
        let mut spinner = create_spinner_if(!args.json && std::io::stdout().is_terminal(), "Checking proposed changes".into());
        let preview = invoke_runner(&args, &configs, &linked_project, &token, auth_type, "plan").await?;
        if let Some(spinner) = &mut spinner {
            if preview.ok {
                success_spinner(spinner, "Checked proposed changes".into());
            } else {
                fail_spinner(spinner, "Could not check proposed changes".into());
            }
        }

        if has_destructive_changes(&preview) {
            bail!("These changes remove Railway resources. Re-run with --stage --yes to stage them.");
        }
    }

    if command == "apply" && !args.yes && !args.json {
        if !std::io::stdout().is_terminal() {
            bail!("Run `railway config apply --yes` to apply changes non-interactively.");
        }

        let mut spinner = create_spinner_if(true, "Checking Railway configuration".into());
        let preview = invoke_runner(&args, &configs, &linked_project, &token, auth_type, "plan").await?;
        if let Some(spinner) = &mut spinner {
            if preview.ok {
                success_spinner(spinner, "Checked Railway configuration".into());
            } else {
                fail_spinner(spinner, "Could not read Railway configuration".into());
            }
        }

        print_response_with_options_and_next(&preview, args.verbose, false);
        if !preview.ok {
            bail!("IaC runner returned diagnostics");
        }
        let changes = preview.change_set.as_ref().map(|change_set| change_set.changes.len()).unwrap_or(0);
        if changes == 0 {
            return Ok(());
        }

        let destructive = has_destructive_changes(&preview);
        println!();
        let prompt = if destructive {
            "Apply these changes? This will remove Railway resources or variables."
        } else {
            "Apply these changes to Railway?"
        };
        if !prompt_confirm_with_default(prompt, false)? {
            bail!("No changes applied.");
        }
        println!();
    }

    let mut spinner = create_spinner_if(!args.json && std::io::stdout().is_terminal(), runner_message(command).into());
    let output = invoke_runner(&args, &configs, &linked_project, &token, auth_type, command).await?;
    if let Some(spinner) = &mut spinner {
        if output.ok {
            success_spinner(spinner, runner_done_message(command).into());
        } else {
            fail_spinner(spinner, "Could not read Railway configuration".into());
        }
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
        if !output.ok {
            bail!("IaC runner returned diagnostics");
        }
        return Ok(());
    }

    print_response_with_options(&output, args.verbose);
    if !output.ok {
        bail!("IaC runner returned diagnostics");
    }

    Ok(())
}

async fn ensure_config_context() -> Result<(Configs, LinkedProject, String, &'static str)> {
    let configs = Configs::new()?;
    let (token, auth_type) = match get_runner_token(&configs) {
        Ok(token) => token,
        Err(error) if std::io::stdout().is_terminal() => {
            println!("{}", "Log in to Railway to continue.".bold());
            crate::commands::login::prompt_login().await?;
            get_runner_token(&Configs::new()?).map_err(|_| error)?
        }
        Err(error) => return Err(error),
    };

    let linked_project = match configs.get_linked_project().await {
        Ok(linked_project) => linked_project,
        Err(_error) if std::io::stdout().is_terminal() => {
            println!();
            println!("{}", "Connect Railway configuration".bold());
            println!("Choose where .railway/railway.ts should plan and apply changes.");
            crate::commands::link::link_project_without_service().await?
        }
        Err(error) => return Err(error),
    };

    Ok((Configs::new()?, linked_project, token, auth_type))
}

fn get_runner_token(configs: &Configs) -> Result<(String, &'static str)> {
    if let Some(token) = Configs::get_railway_token() {
        return Ok((token, "project-token"));
    }

    configs
        .get_railway_auth_token()
        .map(|token| (token, "bearer"))
        .context("Not authenticated. Run `railway login`, set RAILWAY_API_TOKEN, or set RAILWAY_TOKEN.")
}

async fn invoke_runner(
    args: &Args,
    configs: &Configs,
    linked_project: &LinkedProject,
    token: &str,
    auth_type: &str,
    command: &str,
) -> Result<RunnerResponse> {
    let runner = args
        .runner
        .clone()
        .or_else(|| env::var("RAILWAY_IAC_TS_BIN").ok())
        .unwrap_or_else(|| "railway-iac-ts".to_string());

    let cwd = env::current_dir()
        .context("Unable to get current working directory")?
        .to_string_lossy()
        .to_string();

    let request = serde_json::json!({
        "command": command,
        "cwd": cwd,
        "file": args.file.as_ref().map(|path| path.to_string_lossy().to_string()),
        "includeTypes": args.include_types,
        "pretty": false,
        "backboard": {
            "endpoint": configs.get_backboard(),
            "token": token,
            "authType": auth_type,
            "projectId": linked_project.project,
            "environmentId": linked_project.environment,
            "decryptVariables": args.decrypt_variables,
            "merge": true
        }
    });

    let mut command = Command::new(&runner);
    if let Some(runner_cwd) = runner_cwd(&runner) {
        command.current_dir(runner_cwd);
    }
    if matches!(Configs::get_environment_id(), Environment::Dev) {
        command.env("NODE_TLS_REJECT_UNAUTHORIZED", "0");
    }

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn IaC runner `{runner}`. Install/link the railway TypeScript SDK or pass --runner."))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(request.to_string().as_bytes()).await?;
    }

    let output = child.wait_with_output().await?;
    let stdout = String::from_utf8(output.stdout).context("Runner stdout was not valid UTF-8")?;
    let stderr = String::from_utf8(output.stderr).context("Runner stderr was not valid UTF-8")?;

    let response: RunnerResponse = serde_json::from_str(&stdout).with_context(|| {
        format!("IaC runner returned non-JSON output.\nstdout:\n{stdout}\nstderr:\n{stderr}")
    })?;

    Ok(response)
}

fn runner_cwd(runner: &str) -> Option<PathBuf> {
    let path = PathBuf::from(runner);
    if path.file_name()?.to_str()? != "bin.js" {
        return None;
    }
    let iac_dir = path.parent()?;
    if iac_dir.file_name()?.to_str()? != "iac" {
        return None;
    }
    let dist_dir = iac_dir.parent()?;
    if dist_dir.file_name()?.to_str()? != "dist" {
        return None;
    }
    dist_dir.parent().map(|path| path.to_path_buf())
}

fn has_destructive_changes(response: &RunnerResponse) -> bool {
    response
        .change_set
        .as_ref()
        .map(|change_set| {
            change_set
                .changes
                .iter()
                .any(|change| change.severity.as_deref() == Some("destructive"))
        })
        .unwrap_or(false)
}

fn runner_message(command: &str) -> &'static str {
    match command {
        "apply" => "Applying Railway configuration",
        "stage" => "Checking Railway configuration",
        _ => "Checking Railway configuration",
    }
}

fn runner_done_message(command: &str) -> &'static str {
    match command {
        "apply" => "Applied Railway configuration",
        "stage" => "Checked Railway configuration",
        _ => "Checked Railway configuration",
    }
}

pub(super) fn print_response(response: &RunnerResponse) {
    print_response_with_options(response, false);
}

pub(super) fn print_response_with_options(response: &RunnerResponse, verbose: bool) {
    print_response_with_options_and_next(response, verbose, true);
}

pub(super) fn print_response_with_options_and_next(response: &RunnerResponse, verbose: bool, show_next: bool) {
    println!();
    println!("{}", "Railway configuration".bold());
    println!("{} {}", "Using".dimmed(), display_file_path(&response.file).cyan());

    if let Some(environment) = &response.current_environment {
        let environment_name = environment
            .environment_name
            .as_deref()
            .unwrap_or(&environment.environment_id);
        println!("{} {}", "Environment".dimmed(), environment_name.cyan());
        if verbose {
            if let Some(project_id) = &environment.project_id {
                println!("{} {}", "Project".dimmed(), project_id.dimmed());
            }
        }
    }
    println!();

    for diagnostic in &response.diagnostics {
        let text = if diagnostic.path.is_empty() {
            format!("{}: {}", diagnostic.severity, diagnostic.message)
        } else {
            format!(
                "{}: {}: {}",
                diagnostic.severity, diagnostic.path, diagnostic.message
            )
        };
        if diagnostic.severity == "error" {
            println!("{} {}", "Error".red().bold(), text.red());
        } else {
            println!("{} {}", "Warning".yellow().bold(), text.yellow());
        }
    }

    if !response.ok {
        return;
    }

    let changes = response
        .change_set
        .as_ref()
        .map(|change_set| change_set.changes.as_slice())
        .unwrap_or(&[]);

    if let Some(apply_result) = &response.apply_result {
        print_operation_results(apply_result, verbose);
        if verbose {
            println!();
            println!("{} {}", "Result".dimmed(), apply_result.id.dimmed());
            if let Some(deployment_id) = response.deployment_id.as_ref().or(apply_result.deployment_id.as_ref()) {
                println!("{} {}", "Deployment".dimmed(), deployment_id.dimmed());
            }
            if let Some(staged_patch_id) = response.staged_patch_id.as_ref().or(apply_result.staged_patch_id.as_ref()) {
                println!("{} {}", "Patch".dimmed(), staged_patch_id.dimmed());
            }
        }
        return;
    }

    if changes.is_empty() {
        println!("{}", "✓ Your Railway configuration is already up to date.".green());
    } else {
        let total = changes.len();
        println!("{} {}", "Changes".bold(), format!("({total})").dimmed());
        for change in changes {
            print_change(change, verbose);
        }

        let destructive = changes
            .iter()
            .filter(|change| change.severity.as_deref() == Some("destructive"))
            .count();
        if destructive > 0 {
            println!();
            println!(
                "{} {}",
                "!".red().bold(),
                format!("{destructive} destructive change(s) will remove Railway resources or variables.").red()
            );
        }

        if show_next {
            println!();
            println!("{}", "Next".bold());
            println!("  {} Run {} to apply these changes.", "•".cyan(), "railway config apply".cyan());
        }
    }
}

fn display_file_path(path: &str) -> String {
    let path = PathBuf::from(path);
    let cwd = std::env::current_dir().ok();
    let display_path = cwd
        .as_ref()
        .and_then(|cwd| path.strip_prefix(cwd).ok())
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(&path);
    display_path.display().to_string()
}

fn print_operation_results(apply_result: &ChangeSetApplyResult, verbose: bool) {
    if apply_result.changes.is_empty() {
        return;
    }
    let total = apply_result.changes.len();
    println!("{} {}", "Changes".bold(), format!("({total})").dimmed());
    for change in &apply_result.changes {
        let summary = change
            .summary
            .as_deref()
            .or(change.path.as_deref())
            .unwrap_or(&change.kind);
        let marker = match change.status.as_str() {
            "applied" => "✓".green().bold(),
            "noop" => "=".dimmed(),
            "failed" => "✕".red().bold(),
            _ => "•".cyan(),
        };
        if verbose {
            println!("  {} {} {}", marker, summary, format!("({})", change.status).dimmed());
        } else {
            println!("  {} {}", marker, summary);
        }
        if verbose {
            if let Some(outputs) = &change.outputs {
                print_operation_outputs(outputs, 4);
            }
        }
    }
}

fn print_operation_outputs(value: &Value, indent: usize) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                match value {
                    Value::Object(_) | Value::Array(_) => {
                        println!("{}{}", " ".repeat(indent), key.dimmed());
                        print_operation_outputs(value, indent + 2);
                    }
                    _ => println!("{}{} {}", " ".repeat(indent), key.dimmed(), format_output_value(value).cyan()),
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                print_operation_outputs(value, indent);
            }
        }
        _ => println!("{}{}", " ".repeat(indent), format_output_value(value).cyan()),
    }
}

fn format_output_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".to_string(),
        _ => value.to_string(),
    }
}

fn print_change(change: &Change, _verbose: bool) {
    let summary = change
        .summary
        .as_deref()
        .or(change.kind.as_deref())
        .unwrap_or("change");
    let marker = marker_for_change(change);
    println!("  {} {}", marker, summary);
    if let Some(details) = &change.details {
        for detail in details {
            println!("    {} {}", "└".dimmed(), detail.dimmed());
        }
    }
}

fn marker_for_change(change: &Change) -> colored::ColoredString {
    match change.kind.as_deref() {
        Some("resource.create") | Some("variable.set") | Some("domain.create") => "+".green().bold(),
        Some("resource.delete") | Some("variable.delete") => "-".red().bold(),
        _ => "~".yellow().bold(),
    }
}
