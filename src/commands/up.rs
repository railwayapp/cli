use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use colored::Colorize;
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
        detect::detect_services,
        git::{detect_current_branch, detect_github_remote},
        logs::{LogFormat, print_log},
    },
    workspace::{pick_workspace, workspaces},
};

use super::*;

/// Upload and deploy project from the current directory.
///
/// If you're not signed in, opens a browser to sign in or create a
/// Railway account (single unified OAuth flow — new accounts are
/// created on the fly), then chains into project + service creation
/// and deploy. Pair with -y to skip the surrounding prompts in
/// scripted or agent-driven contexts.
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway up --service api --environment production\n  railway up ./apps/api --path-as-root --service api\n  railway up --detach --json --message \"deploy api\"\n\nAutomation notes:\n  `railway up --detach --json` starts an upload and deployment, but it does not wait for the deployment to become healthy.\n  Poll with `railway deployment list --json` and inspect logs with `railway logs --json --lines 100`."
)]
pub struct Args {
    path: Option<PathBuf>,

    #[clap(short, long, alias = "no-wait")]
    /// Don't attach to the log stream — start the deploy and return
    /// immediately. Use --no-wait as the alternate name in scripted
    /// flows. Combine with -y for fully unattended runs.
    detach: bool,

    #[clap(short = 'y', long)]
    /// Accept all defaults — skip the auth confirm prompt for unauthed
    /// users and skip the project-name prompt when creating a new
    /// project from this directory. The browser still has to open for
    /// OAuth itself; -y just removes the surrounding prompts.
    yes: bool,

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

    #[clap(short, long)]
    /// Workspace to create a new project in (first-run / --new). Auto-selects if you only have one; otherwise prompts.
    workspace: Option<String>,

    #[clap(long)]
    /// Create a NEW project + service from this directory and deploy it,
    /// even if one is already linked. Implied on a cold/unauthenticated
    /// first run, and for `-y` when nothing is linked.
    new: bool,

    #[clap(long)]
    /// Name for a newly created project (defaults to the current
    /// directory's name). Only used when creating a new project.
    name: Option<String>,

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

    #[clap(short, long)]
    /// Message to attach to the deployment
    message: Option<String>,
}

pub async fn command(args: Args) -> Result<()> {
    crate::util::reporter::set_mode(args.json);

    let mut configs = Configs::new()?;

    // If the user isn't signed in, intercept early: show a clack-style
    // picker (Create New Account / Log In and Deploy), chain into the
    // login flow, then reload configs and continue with `up`. This
    // turns the previously cryptic "no token" error path into the
    // canonical first-run experience.
    let came_from_unauth_prompt = configs.get_railway_auth_token().is_none();
    if came_from_unauth_prompt {
        prompt_unauth_and_login(&args).await?;
        configs = Configs::new()?;
    }

    // Create-a-new-project path. `--new` forces it (a fresh project even
    // if one is already linked). Otherwise, when there's no project to
    // deploy to, create one automatically if we just signed up (cold
    // start) or the user passed `-y` ("accept everything"). An authed
    // user with no linked project and no `-y`/`--new` still hits the
    // standard "no project specified" error below — we don't silently
    // create a project behind their back.
    let should_create_new = if args.new {
        true
    } else if args.project.is_some() {
        false
    } else {
        (came_from_unauth_prompt || args.yes)
            && configs.get_linked_project().await.is_err()
    };
    if should_create_new {
        return deploy_new_project(&args).await;
    }

    let hostname = configs.get_host();
    let client = GQLClient::new_authorized(&configs)?;

    if args.project.is_some() && args.environment.is_none() {
        bail!("--environment is required when using --project");
    }

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

    let service = get_or_prompt_service(linked_project, project, args.service).await?;

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

/// Drop into the login flow when the user isn't signed in. Sign-up
/// and sign-in go through the same OAuth surface, so we don't bother
/// prompting the user to declare which they're doing — the backend
/// detects fresh accounts on its own (from durable compliance state —
/// a CLI client that hasn't accepted ToS/Fair-Use yet) and adapts the
/// consent screen + post-auth landing accordingly.
async fn prompt_unauth_and_login(args: &Args) -> Result<()> {
    // Decide whether there's a human who can complete a sign-in.
    // JSON/CI consumers, and captured-stdout runs with no agent harness,
    // have nobody to drive it — so surface a structured NOT_AUTHENTICATED
    // error (rendered as JSON in --json mode by the top-level handler)
    // instead of starting a flow nobody can finish. Otherwise we proceed
    // with a browser when one is reachable, or a device code (which the
    // human completes on another device) on SSH / no-DISPLAY. The full
    // truth table lives in `exec_context`.
    let ctx = crate::exec_context::ExecutionContext::detect(args.json, args.ci);
    let transport = match ctx.auto_auth(false) {
        crate::exec_context::AutoAuth::Proceed(transport) => transport,
        crate::exec_context::AutoAuth::FailFast => {
            return Err(crate::errors::RailwayError::NotAuthenticated.into());
        }
    };

    // An agent harness with piped stdin is treated as implicit consent:
    // skip the "Continue?" prompt (stdin can't answer it) and proceed.
    let implicit_consent = ctx.agent_implicit_consent();

    println!();
    println!(
        "  {} {}",
        "!".yellow().bold(),
        "You're not signed in to Railway.".bold(),
    );
    println!();
    println!(
        "  {} will sign you in (or create an account if you don't have one),",
        "railway up".bold(),
    );
    println!(
        "  then create a new Railway project from this directory and deploy it."
    );
    println!();
    println!("  To deploy to an existing project instead, cancel and run");
    println!("  `railway login && railway link --project <name>` first.");
    println!();

    // Skip the confirm prompt under an agent harness (stdin isn't a
    // TTY there either, so the prompt would fail). The agent invoking
    // `railway up` is treated as implicit consent to proceed — print a
    // one-liner so the human watching the agent's transcript knows how
    // sign-in is about to surface, without a prompt.
    if implicit_consent {
        let how = match transport {
            crate::exec_context::AuthTransport::Browser => "opening browser",
            crate::exec_context::AuthTransport::DeviceCode => {
                "printing a device-code sign-in link"
            }
        };
        println!(
            "  {} Agent harness detected — {how} (skipping confirm).",
            "→".cyan(),
        );
    }
    if !args.yes && !implicit_consent {
        // Confirm before opening a browser tab — interactive users
        // appreciate not having tabs spawn out from under them. -y
        // skips this for unattended flows.
        let confirm = crate::util::prompt::prompt_confirm_with_default_with_cancel(
            "Continue?",
            true,
        )?;
        match confirm {
            Some(true) => {}
            _ => bail!("Aborted."),
        }
    }

    super::login::command(super::login::Args {
        browserless: false,
    })
    .await
}

/// Create a brand-new project + service from the current directory and
/// deploy it (the `up --new` path, and the cold-start / `-y`-with-no-link
/// path). Creates the project, bundles + uploads the directory, links the
/// project and service to the cwd, and streams the build to completion.
async fn deploy_new_project(args: &Args) -> Result<()> {
    let mut configs = Configs::new()?;

    // Surface a helpful error rather than a cryptic GQL failure when
    // unauthed. The unauthed `up` chain runs login before we get here,
    // so this only fires for a direct `up --new` with no/expired token.
    if !configs.has_oauth_token() || configs.is_token_expired() {
        bail!("Not signed in. Run `railway login` first.");
    }

    let hostname = configs.get_host().to_owned();
    let client = GQLClient::new_authorized(&configs)?;

    let workspaces = workspaces().await?;
    let workspace = pick_workspace(workspaces, args.workspace.clone())?;

    let cwd_path = args
        .path
        .clone()
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)?;

    // Resolve the project name:
    //   --name foo        → "foo"
    //   -y, no --name     → current directory basename (or backboard-
    //                       generated if there isn't one we can read)
    //   interactive TTY   → prompt with the directory basename as
    //                       default; user can hit Enter to accept
    //   non-TTY, no -y    → fall back to directory basename
    let default_name: Option<String> = cwd_path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let project_name: Option<String> = if args.name.is_some() {
        args.name.clone()
    } else if args.yes || !std::io::stdout().is_terminal() {
        default_name.clone()
    } else {
        let default = default_name.clone().unwrap_or_default();
        let input = if default.is_empty() {
            inquire::Text::new("Project name")
                .with_render_config(Configs::get_render_config())
                .prompt()?
        } else {
            inquire::Text::new("Project name")
                .with_default(&default)
                .with_render_config(Configs::get_render_config())
                .prompt()?
        };
        let trimmed = input.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    // Show GitHub repo detection (informational for now — full GH App
    // integration is a separate piece; we deploy from local tarball).
    if let Some(remote) = detect_github_remote(&cwd_path) {
        let branch = detect_current_branch(&cwd_path)
            .map(|branch| format!(" on {branch}"))
            .unwrap_or_default();
        println!(
            "  {} GitHub remote: {}{} {}",
            "◇".cyan(),
            remote.full_repo_name().bold(),
            branch,
            "(deploying current directory; GH App integration coming later)".dimmed(),
        );
    }

    let detected_services = detect_services(&cwd_path);
    if !detected_services.is_empty() {
        println!(
            "  {} Detected service dependencies: {} {}",
            "◇".cyan(),
            detected_services.join(", ").bold(),
            "(automatic provisioning is not wired yet)".dimmed(),
        );
    }

    // Create the project first so the user has a landing pad even if
    // the build later fails.
    let create_spinner = ProgressBar::new_spinner();
    let _ = create_spinner.set_style(
        ProgressStyle::default_spinner()
            .tick_chars(TICK_STRING)
            .template("{spinner:.green} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    create_spinner.set_message("Creating project");
    create_spinner.enable_steady_tick(Duration::from_millis(100));

    let vars = mutations::project_create::Variables {
        name: project_name,
        description: None,
        workspace_id: Some(workspace.id().to_owned()),
    };
    let project_create =
        post_graphql::<mutations::ProjectCreate, _>(&client, configs.get_backboard(), vars)
            .await?
            .project_create;

    let environment = project_create
        .environments
        .edges
        .first()
        .context("Project has no default environment")?
        .node
        .clone();

    create_spinner.finish_and_clear();
    println!(
        "  {} Created project {} on {}",
        "✓".green(),
        project_create.name.bold(),
        workspace.name(),
    );

    // Bundle the directory.
    let bundle_spinner = ProgressBar::new_spinner();
    let _ = bundle_spinner.set_style(
        ProgressStyle::default_spinner()
            .tick_chars(TICK_STRING)
            .template("{spinner:.green} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    bundle_spinner.set_message("Bundling project");
    bundle_spinner.enable_steady_tick(Duration::from_millis(100));

    let tarball = create_deploy_tarball(&cwd_path, &cwd_path, args.no_gitignore, |_, _| {})?;
    bundle_spinner.finish_and_clear();
    println!("  {} Bundled ({} bytes)", "✓".green(), tarball.len());

    // Upload + queue the build.
    let upload_spinner = ProgressBar::new_spinner();
    let _ = upload_spinner.set_style(
        ProgressStyle::default_spinner()
            .tick_chars(TICK_STRING)
            .template("{spinner:.green} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    upload_spinner.set_message("Uploading & queuing build");
    upload_spinner.enable_steady_tick(Duration::from_millis(100));

    // Reuse the GQLClient::new_authorized reqwest client — it bakes the
    // bearer token into default headers, which backboard's
    // /project/:id/environment/:id/up endpoint requires.
    let up_response = upload_deploy_tarball(
        &client,
        &hostname,
        &project_create.id,
        &environment.id,
        None,
        None,
        tarball,
    )
    .await?;
    upload_spinner.finish_and_clear();

    // Link the project to the current directory so future `railway up`,
    // `railway logs`, etc. target it.
    configs.link_project(
        project_create.id.clone(),
        Some(project_create.name.clone()),
        environment.id.clone(),
        Some(environment.name.clone()),
    )?;

    // backboard's /up endpoint creates a service implicitly but doesn't
    // return its id, so recover it from the logs_url
    // (.../project/<pid>/service/<sid>?...) to link the service too.
    if let Some(service_id) = parse_service_id_from_logs_url(&up_response.logs_url) {
        configs.link_service(service_id)?;
    } else {
        crate::util::reporter::warn(
            "SERVICE_LINK_UNRESOLVED",
            "Couldn't determine the new service id, so it wasn't linked automatically.",
            Some("Run `railway service` to link it before `railway logs`."),
        );
    }

    configs.write()?;

    println!("  {} Build queued", "✓".green());
    println!("  {} {}", "Build Logs:".green().bold(), up_response.logs_url);

    let deploy_url = if up_response.deployment_domain.is_empty() {
        None
    } else if up_response.deployment_domain.starts_with("http") {
        Some(up_response.deployment_domain.clone())
    } else {
        Some(format!("https://{}", up_response.deployment_domain))
    };

    // --no-wait / --detach: surface the URL + summary and return.
    if args.detach {
        print_app_summary(
            &hostname,
            &project_create.id,
            &project_create.name,
            parse_service_id_from_logs_url(&up_response.logs_url).as_deref(),
            &environment.id,
            deploy_url.as_deref(),
        );
        return Ok(());
    }

    // Stream build + deploy logs so the user sees the build happen.
    // Small delay first to let backboard register the deployment.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let build_id_for_logs = up_response.deployment_id.clone();
    let build_task = tokio::task::spawn(async move {
        let _ = stream_build_logs(build_id_for_logs, None, |log| {
            println!("{}", log.message);
        })
        .await;
    });

    let deploy_id_for_logs = up_response.deployment_id.clone();
    let deploy_task = tokio::task::spawn(async move {
        let _ = stream_deploy_logs(deploy_id_for_logs, None, |log| {
            println!("{}", log.message);
        })
        .await;
    });

    // Watch deployment status so we exit cleanly on terminal states.
    let logs_url_for_status = up_response.logs_url.clone();
    let summary_host = hostname.clone();
    let summary_project_id = project_create.id.clone();
    let summary_project_name = project_create.name.clone();
    let summary_service_id = parse_service_id_from_logs_url(&up_response.logs_url);
    let summary_environment_id = environment.id.clone();
    let summary_deploy_url = deploy_url.clone();
    let mut status_stream =
        subscribe_graphql::<subscriptions::Deployment>(subscriptions::deployment::Variables {
            id: up_response.deployment_id.clone(),
        })
        .await?;
    tokio::task::spawn(async move {
        while let Some(Ok(res)) = status_stream.next().await {
            let Some(data) = res.data else { continue };
            match data.deployment.status {
                DeploymentStatus::SUCCESS => {
                    print_app_summary(
                        &summary_host,
                        &summary_project_id,
                        &summary_project_name,
                        summary_service_id.as_deref(),
                        &summary_environment_id,
                        summary_deploy_url.as_deref(),
                    );
                    std::process::exit(0);
                }
                DeploymentStatus::FAILED => {
                    println!();
                    println!("  {} {}", "✗".red(), "Build failed".bold());
                    println!(
                        "     {} {}",
                        "Logs:".dimmed(),
                        logs_url_for_status.bold().underline(),
                    );
                    println!();
                    std::process::exit(1);
                }
                DeploymentStatus::CRASHED => {
                    println!();
                    println!("  {} {}", "✗".red(), "Deploy crashed".bold());
                    println!(
                        "     {} {}",
                        "Logs:".dimmed(),
                        logs_url_for_status.bold().underline(),
                    );
                    println!();
                    std::process::exit(1);
                }
                _ => {}
            }
        }
    });

    let _ = futures::future::join_all([build_task, deploy_task]).await;
    println!();
    println!("  {} Watch the build:", "🔧".dimmed());
    println!("     {}", up_response.logs_url.bold().underline());
    println!();

    Ok(())
}

/// Print the end-of-run summary: the running URL when one exists, a hint
/// to add one when it doesn't, and the project + dashboard link so an
/// agent (or human) has something concrete to hand back. We never
/// auto-generate a domain — exposing a service publicly is the user's
/// call (`railway domain`).
fn print_app_summary(
    host: &str,
    project_id: &str,
    project_name: &str,
    service_id: Option<&str>,
    environment_id: &str,
    deploy_url: Option<&str>,
) {
    println!();
    match deploy_url {
        Some(url) => {
            println!("  {} {}", "🚀".dimmed(), "Live at".bold());
            println!("     {}", url.bold().underline());
        }
        None => {
            println!("  {} {}", "✓".green(), "Deploy complete".bold());
            println!(
                "     {} run {} to add a public URL.",
                "No public domain yet —".dimmed(),
                "railway domain".bold(),
            );
        }
    }

    let dashboard = match service_id {
        Some(sid) => format!(
            "https://{host}/project/{project_id}/service/{sid}?environmentId={environment_id}"
        ),
        None => format!("https://{host}/project/{project_id}"),
    };
    println!();
    println!("  {} Project {}", "✓".green(), project_name.bold());
    println!("     {} {}", "Manage:".dimmed(), dashboard.bold().underline());
    println!();
}

/// Extract the service ID from a logs URL of shape
/// `.../project/{project_id}/service/{service_id}?...`. Returns None if
/// the URL doesn't contain a `/service/<id>` segment.
fn parse_service_id_from_logs_url(logs_url: &str) -> Option<String> {
    let after_service = logs_url.split("/service/").nth(1)?;
    let service_id: String = after_service
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if service_id.is_empty() {
        None
    } else {
        Some(service_id)
    }
}
