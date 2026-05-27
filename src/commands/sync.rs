use std::{env, path::PathBuf, process::Stdio};

use serde::Deserialize;
use serde_json::Value;
use tokio::{io::AsyncWriteExt, process::Command};

use super::*;

/// Preview or stage Railway IaC changes from .railway/railway.ts
#[derive(Parser)]
pub struct Args {
    /// Path to the Railway IaC file. Defaults to nearest .railway/railway.ts resolved by the runner.
    #[clap(long)]
    file: Option<PathBuf>,

    /// Stage the proposed ChangeSet in Backboard.
    #[clap(long)]
    stage: bool,

    /// Output raw runner JSON.
    #[clap(long)]
    json: bool,

    /// Confirm destructive staged changes.
    #[clap(long)]
    yes: bool,

    /// Ask Backboard to decrypt variables while planning, when authorized.
    #[clap(long)]
    decrypt_variables: bool,

    /// Include generated graph TypeScript types in runner output.
    #[clap(long)]
    include_types: bool,

    /// Override linked project id. Primarily for local alpha testing.
    #[clap(long)]
    project_id: Option<String>,

    /// Override linked environment id. Primarily for local alpha testing.
    #[clap(long)]
    environment_id: Option<String>,

    /// Path to the TypeScript IaC runner binary. Defaults to RAILWAY_IAC_TS_BIN or railway-iac-ts.
    #[clap(long)]
    runner: Option<String>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerResponse {
    ok: bool,
    command: String,
    file: String,
    current_environment: Option<CurrentEnvironment>,
    change_set: Option<ChangeSet>,
    diff: Option<String>,
    diagnostics: Vec<Diagnostic>,
    staged_patch: Option<StagedPatch>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentEnvironment {
    project_id: Option<String>,
    environment_id: String,
    environment_name: Option<String>,
}

#[derive(Deserialize, serde::Serialize)]
struct ChangeSet {
    changes: Vec<Change>,
}

#[derive(Deserialize, serde::Serialize)]
struct Change {
    summary: Option<String>,
    severity: Option<String>,
    kind: Option<String>,
}

#[derive(Deserialize, serde::Serialize)]
struct Diagnostic {
    severity: String,
    path: String,
    message: String,
}

#[derive(Deserialize, serde::Serialize)]
struct StagedPatch {
    id: String,
    #[allow(dead_code)]
    patch: Option<Value>,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let linked_project = configs.get_linked_project().await?;
    let token = get_runner_token(&configs)?;
    let command = if args.stage { "stage" } else { "plan" };

    if args.stage && !args.yes {
        let preview = invoke_runner(&args, &configs, &linked_project, &token, "plan").await?;
        if has_destructive_changes(&preview) {
            bail!("Plan contains destructive changes. Re-run with --yes to stage.");
        }
    }

    let output = invoke_runner(&args, &configs, &linked_project, &token, command).await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
        if !output.ok {
            bail!("IaC runner returned diagnostics");
        }
        return Ok(());
    }

    print_response(&output);
    if !output.ok {
        bail!("IaC runner returned diagnostics");
    }

    Ok(())
}

fn get_runner_token(configs: &Configs) -> Result<String> {
    if Configs::get_railway_token().is_some() {
        bail!("railway sync currently requires a user/API token; project tokens are not supported by the TypeScript IaC runner yet")
    }

    configs
        .get_railway_auth_token()
        .context("Not authenticated. Run `railway login` or set RAILWAY_API_TOKEN.")
}

async fn invoke_runner(
    args: &Args,
    configs: &Configs,
    linked_project: &LinkedProject,
    token: &str,
    command: &str,
) -> Result<RunnerResponse> {
    let runner = args
        .runner
        .clone()
        .or_else(|| env::var("RAILWAY_IAC_TS_BIN").ok())
        .unwrap_or_else(|| "railway-iac-ts".to_string());

    let request = serde_json::json!({
        "command": command,
        "file": args.file.as_ref().map(|path| path.to_string_lossy().to_string()),
        "includeTypes": args.include_types,
        "pretty": false,
        "backboard": {
            "endpoint": configs.get_backboard(),
            "token": token,
            "projectId": args.project_id.as_deref().unwrap_or(&linked_project.project),
            "environmentId": args.environment_id.as_deref().unwrap_or(&linked_project.environment),
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

fn print_response(response: &RunnerResponse) {
    println!("{}", "Railway IaC sync".bold());
    println!("runner: {}", response.command);
    println!("file: {}", response.file);

    if let Some(environment) = &response.current_environment {
        println!(
            "project: {}",
            environment.project_id.as_deref().unwrap_or("(unknown)")
        );
        println!(
            "environment: {}",
            environment
                .environment_name
                .as_deref()
                .unwrap_or(&environment.environment_id)
        );
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
            println!("{}", text.red());
        } else {
            println!("{}", text.yellow());
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

    if changes.is_empty() {
        println!("{}", "No changes.".green());
    } else {
        println!("{}", "ChangeSet".bold());
        if let Some(diff) = &response.diff {
            println!("{diff}");
        } else {
            for change in changes {
                println!(
                    "{}",
                    change
                        .summary
                        .as_deref()
                        .or(change.kind.as_deref())
                        .unwrap_or("change")
                );
            }
        }

        let destructive = changes
            .iter()
            .filter(|change| change.severity.as_deref() == Some("destructive"))
            .count();
        if destructive > 0 {
            println!("{}", format!("{destructive} destructive change(s).").red());
        }
    }

    if let Some(staged_patch) = &response.staged_patch {
        println!();
        println!(
            "{}",
            format!("Staged Backboard patch: {}", staged_patch.id).green()
        );
    } else {
        println!();
        println!("Run with {} to stage the proposed ChangeSet.", "--stage".cyan());
    }
}
