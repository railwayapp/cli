use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Result, bail};

use futures::StreamExt;
use indicatif::{ProgressBar, ProgressFinish, ProgressStyle};
use is_terminal::IsTerminal;

use crate::{
    consts::TICK_STRING,
    controllers::{
        deployment::{stream_build_logs, stream_deploy_logs},
        environment::get_matched_environment,
        project::get_project,
        service::get_or_prompt_service,
        upload::{create_deploy_tarball, upload_deploy_tarball},
    },
    subscription::subscribe_graphql,
    subscriptions::deployment::DeploymentStatus,
    util::{
        logs::{LogFormat, print_log},
        prompt::prompt_confirm_with_default,
    },
};

use super::*;

/// Upload and deploy project from the current directory
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway up --service api --environment production\n  railway up ./apps/api --path-as-root --service api\n  railway up --detach --json --message \"deploy api\"\n\nAutomation notes:\n  `railway up --detach --json` starts an upload and deployment, but it does not wait for the deployment to become healthy.\n  Poll with `railway deployment list --json` and inspect logs with `railway logs --json --lines 100`.\n  To switch a locally uploaded service to GitHub autodeploys, run `railway service source connect --repo owner/repo --branch main --service api`."
)]
pub struct Args {
    path: Option<PathBuf>,

    #[clap(short, long)]
    /// Don't attach to the log stream
    detach: bool,

    #[clap(short, long)]
    /// Stream build logs only, then exit (equivalent to setting $CI=true).
    ci: bool,

    #[clap(short, long)]
    /// Service to deploy to (defaults to linked service)
    service: Option<String>,

    #[clap(short, long)]
    /// Environment to deploy to (defaults to linked environment)
    environment: Option<String>,

    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    /// Project ID to deploy to (defaults to linked project)
    project: Option<String>,

    #[clap(long)]
    /// Don't ignore paths from .gitignore
    no_gitignore: bool,

    #[clap(long)]
    /// Use the path argument as the prefix for the archive instead of the project directory.
    path_as_root: bool,

    #[clap(long)]
    /// Verbose output
    verbose: bool,

    #[clap(long)]
    /// Output logs in JSON format (implies CI mode behavior)
    json: bool,

    #[clap(long)]
    /// Apply Railway configuration before deploying if .railway/railway.ts exists
    sync: bool,

    #[clap(long)]
    /// Do not apply Railway configuration before deploying
    no_sync: bool,

    #[clap(long)]
    /// Confirm Railway configuration prompts
    yes: bool,

    #[clap(short, long)]
    /// Message to attach to the deployment
    message: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let hostname = configs.get_host();
    let client = GQLClient::new_authorized(&configs)?;

    if args.project.is_some() && args.environment.is_none() {
        bail!("--environment is required when using --project");
    }

    let iac_service = maybe_sync_iac_before_up(&args).await?;

    let linked_project = if args.project.is_none() {
        Some(configs.get_linked_project().await?)
    } else {
        None
    };

    let linked_project_path = linked_project.as_ref().map(|lp| lp.project_path.clone());
    let deploy_paths = get_deploy_paths(&args, linked_project_path)?;

    let project_id = args
        .project
        .clone()
        .or_else(|| linked_project.as_ref().map(|lp| lp.project.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!("No project specified. Use --project or run `railway link` first")
        })?;

    let project = get_project(&client, &configs, project_id.clone()).await?;

    let environment = args
        .environment
        .clone()
        .or_else(|| {
            linked_project
                .as_ref()
                .and_then(|lp| lp.environment.clone())
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No environment specified. Set RAILWAY_ENVIRONMENT_ID, use --environment, or run `railway environment` to link one."
            )
        })?;
    let environment_id = get_matched_environment(&project, environment)?.id;

    let service = get_or_prompt_service(linked_project, project, args.service.clone().or(iac_service)).await?;

    let is_tty = std::io::stdout().is_terminal() && !args.json;

    let spinner = if is_tty {
        let spinner = ProgressBar::new_spinner()
            .with_style(
                ProgressStyle::default_spinner()
                    .tick_chars(TICK_STRING)
                    .template("{spinner:.green} {msg:.cyan.bold}")?,
            )
            .with_message("Indexing");
        spinner.enable_steady_tick(Duration::from_millis(100));
        Some(spinner)
    } else if !args.json {
        println!("Indexing...");
        None
    } else {
        None
    };

    let mut progress_bar: Option<ProgressBar> = None;
    let body = create_deploy_tarball(
        &deploy_paths.project_path,
        &deploy_paths.archive_prefix_path,
        args.no_gitignore,
        |current, total| {
            if current == 0 {
                // Indexing complete
                if let Some(s) = &spinner {
                    s.finish_with_message("Indexed");
                }
                if is_tty {
                    let pg = ProgressBar::new(total as u64)
                        .with_style(
                            ProgressStyle::default_bar()
                                .template(
                                    "{spinner:.green} {msg:.cyan.bold} [{bar:20}] {percent}% ",
                                )
                                .unwrap()
                                .progress_chars("=> ")
                                .tick_chars(TICK_STRING),
                        )
                        .with_message("Compressing")
                        .with_finish(ProgressFinish::WithMessage("Compressed".into()));
                    pg.enable_steady_tick(Duration::from_millis(100));
                    progress_bar = Some(pg);
                }
            } else if let Some(pg) = &progress_bar {
                pg.inc(1);
            }
        },
    )?;

    // Ensure progress bar finishes if no entries were processed
    drop(progress_bar);

    if args.verbose {
        println!("railway up");
        println!("service: {}", service.as_deref().unwrap_or_default());
        println!("environment: {environment_id}");
        println!("bytes: {}", body.len());
    }

    let spinner = if std::io::stdout().is_terminal() && !args.json {
        let spinner = ProgressBar::new_spinner()
            .with_style(
                ProgressStyle::default_spinner()
                    .tick_chars(TICK_STRING)
                    .template("{spinner:.green} {msg:.cyan.bold}")?,
            )
            .with_message("Uploading");
        spinner.enable_steady_tick(Duration::from_millis(100));
        Some(spinner)
    } else if !args.json {
        println!("Uploading...");
        None
    } else {
        None
    };

    let up_result = upload_deploy_tarball(
        &client,
        hostname,
        &project_id,
        &environment_id,
        service.as_deref(),
        args.message.as_deref(),
        body,
    )
    .await;

    let body = match up_result {
        Err(e) => {
            if let Some(spinner) = spinner {
                spinner.finish_with_message("Failed");
            }
            return Err(e);
        }
        Ok(body) => {
            if let Some(spinner) = spinner {
                spinner.finish_with_message("Uploaded");
            }
            body
        }
    };

    let deployment_id = body.deployment_id;

    if !args.json {
        println!("  {}: {}", "Build Logs".green().bold(), body.logs_url);
    }

    if args.detach {
        if args.json {
            println!(
                "{}",
                serde_json::json!({"deploymentId": deployment_id, "logsUrl": body.logs_url})
            );
        }
        return Ok(());
    }

    let ci_mode = Configs::env_is_ci() || args.ci || args.json;
    if ci_mode && !args.json {
        println!("{}", "CI mode enabled".green().bold());
    }

    // If the user is not in a terminal AND if we are not in CI mode, don't stream logs
    if !std::io::stdout().is_terminal() && !ci_mode {
        return Ok(());
    }

    //	Create vector of log streaming tasks
    //	Always stream build logs
    //  Add a small delay before starting log streaming to allow the backend
    //  to fully register the deployment. This prevents race conditions where
    //  the WebSocket subscription fails because the deployment isn't ready yet.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let build_deployment_id = deployment_id.clone();
    let json_mode = args.json;
    let ci_flag = args.ci;
    let mut tasks = vec![tokio::task::spawn(async move {
        if let Err(e) = stream_build_logs(build_deployment_id, None, |log| {
            let should_exit =
                ci_flag && log.message.starts_with("No changed files matched patterns");
            if json_mode {
                print_log(log, true, LogFormat::LevelOnly);
            } else {
                println!("{}", log.message);
            }
            if should_exit {
                std::process::exit(0);
            }
        })
        .await
        {
            eprintln!("Failed to stream build logs: {e}");

            if ci_mode {
                std::process::exit(1);
            }
        }
    })];

    // Stream deploy logs only if is not in ci mode
    if !ci_mode {
        let deploy_deployment_id = deployment_id.clone();
        tasks.push(tokio::task::spawn(async move {
            if let Err(e) = stream_deploy_logs(deploy_deployment_id, None, |log| {
                print_log(log, false, LogFormat::Full)
            })
            .await
            {
                eprintln!("Failed to stream deploy logs: {e}");
            }
        }));
    }

    let mut stream =
        subscribe_graphql::<subscriptions::Deployment>(subscriptions::deployment::Variables {
            id: deployment_id.clone(),
        })
        .await?;

    tokio::task::spawn(async move {
        while let Some(Ok(res)) = stream.next().await {
            if let Some(errors) = res.errors {
                if json_mode {
                    eprintln!(
                        "{}",
                        serde_json::json!({"error": errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; ")})
                    );
                } else {
                    eprintln!(
                        "Failed to get deploy status: {}",
                        errors
                            .iter()
                            .map(|err| err.to_string())
                            .collect::<Vec<String>>()
                            .join("; ")
                    );
                }
                if ci_mode {
                    std::process::exit(1);
                }
            }
            if let Some(data) = res.data {
                match data.deployment.status {
                    DeploymentStatus::SUCCESS => {
                        if json_mode {
                            println!("{}", serde_json::json!({"status": "success"}));
                        } else {
                            println!("{}", "Deploy complete".green().bold());
                        }
                        if ci_mode {
                            std::process::exit(0);
                        }
                    }
                    DeploymentStatus::FAILED => {
                        if json_mode {
                            println!("{}", serde_json::json!({"status": "failed"}));
                        } else {
                            println!("{}", "Deploy failed".red().bold());
                        }
                        std::process::exit(1);
                    }
                    DeploymentStatus::CRASHED => {
                        if json_mode {
                            println!("{}", serde_json::json!({"status": "crashed"}));
                        } else {
                            println!("{}", "Deploy crashed".red().bold());
                        }
                        std::process::exit(1);
                    }
                    _ => {}
                }
            }
        }
    });

    futures::future::join_all(tasks).await;

    Ok(())
}

async fn maybe_sync_iac_before_up(args: &Args) -> Result<Option<String>> {
    if args.no_sync {
        return Ok(None);
    }

    let railway_file = match find_railway_file(std::env::current_dir()?) {
        Some(file) => file,
        None => return Ok(None),
    };

    let sync_args = crate::commands::sync::Args {
        file: Some(railway_file.clone()),
        stage: false,
        json: args.json,
        yes: true,
        apply: false,
        decrypt_variables: false,
        include_types: false,
        runner: None,
        verbose: false,
    };

    let plan = crate::commands::sync::run(&sync_args, "plan").await?;
    if !plan.ok {
        crate::commands::sync::print_response(&plan);
        bail!("IaC runner returned diagnostics");
    }

    let changes = plan.change_set.as_ref().map(|change_set| change_set.changes.len()).unwrap_or(0);
    if changes == 0 {
        crate::commands::sync::print_response(&plan);
        return Ok(infer_iac_deploy_service(&plan));
    }

    if !args.yes {
        if !std::io::stdout().is_terminal() {
            if args.sync {
                bail!("Applying Railway configuration before deploy requires --yes in non-interactive mode.");
            }
            println!(
                "Found Railway configuration at {}, skipping project changes in non-interactive mode. Use --sync --yes to apply before deploy.",
                display_path(&railway_file)
            );
            return Ok(None);
        }

        crate::commands::sync::print_response_with_options_and_next(&plan, true, false);
        println!();
        let apply_sync = prompt_confirm_with_default(
            "Apply these Railway configuration changes before deploying?",
            true,
        )?;
        if !apply_sync {
            return Ok(infer_iac_deploy_service(&plan));
        }
        println!();
    }

    let apply_args = crate::commands::sync::Args {
        apply: true,
        ..sync_args
    };
    let response = crate::commands::sync::run(&apply_args, "apply").await?;
    crate::commands::sync::print_response(&response);
    if !response.ok {
        bail!("IaC runner returned diagnostics");
    }
    Ok(infer_iac_deploy_service(&response))
}

fn display_path(path: &Path) -> String {
    let cwd = std::env::current_dir().ok();
    let display_path = cwd
        .as_ref()
        .and_then(|cwd| path.strip_prefix(cwd).ok())
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(path);
    display_path.display().to_string()
}

fn infer_iac_deploy_service(response: &crate::commands::sync::RunnerResponse) -> Option<String> {
    let services = response
        .desired_graph
        .as_ref()?
        .resources
        .iter()
        .filter(|resource| resource.r#type == "service")
        .collect::<Vec<_>>();
    if services.len() == 1 {
        Some(services[0].name.clone())
    } else {
        None
    }
}

fn find_railway_file(start: PathBuf) -> Option<PathBuf> {
    let mut cursor = start;
    loop {
        let file = cursor.join(".railway/railway.ts");
        if file.exists() {
            return Some(file);
        }
        if !cursor.pop() {
            return None;
        }
    }
}

struct DeployPaths {
    project_path: PathBuf,
    archive_prefix_path: PathBuf,
}

fn get_deploy_paths(args: &Args, linked_project_path: Option<String>) -> Result<DeployPaths> {
    if args.path_as_root {
        if args.path.is_none() {
            bail!("--path-as-root requires a path to be specified");
        }

        let path = args.path.clone().unwrap();
        Ok(DeployPaths {
            project_path: path.clone(),
            archive_prefix_path: path,
        })
    } else {
        let project_dir: PathBuf = match linked_project_path {
            Some(path) => PathBuf::from(path),
            None => std::env::current_dir().context("Failed to get current directory")?,
        };
        let project_path = match args.path {
            Some(ref path) => path.clone(),
            None => project_dir.clone(),
        };
        Ok(DeployPaths {
            project_path,
            archive_prefix_path: project_dir,
        })
    }
}
