use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Subcommand;
use indicatif::{ProgressBar, ProgressStyle};

use crate::{
    consts::TICK_STRING,
    controllers::upload::{create_deploy_tarball, upload_deploy_tarball},
    util::git::detect_github_remote,
    workspace::{pick_workspace, workspaces},
};

use super::*;

/// Create something on Railway (an account, an app, etc.)
#[derive(Parser)]
pub struct Args {
    #[command(subcommand)]
    command: CreateCommands,
}

#[derive(Subcommand)]
enum CreateCommands {
    /// Sign up for a new Railway account (or sign in if you have one).
    /// Opens the browser to a signup-friendly landing page and writes
    /// the CLI token on success.
    Account,

    /// Create a new project + service from the current directory and
    /// deploy it. Requires you to be signed in (run `railway create
    /// account` first if you're new).
    App {
        /// Path to the project directory (defaults to cwd).
        path: Option<PathBuf>,

        /// Workspace to create the project in. Auto-selects if you
        /// only have one; otherwise prompts.
        #[clap(short, long)]
        workspace: Option<String>,

        /// Override the auto-generated project name (defaults to the
        /// current directory's name).
        #[clap(long)]
        name: Option<String>,

        /// Don't ignore paths from .gitignore when bundling.
        #[clap(long)]
        no_gitignore: bool,

        /// Skip the wait-for-serve step. Default streams build status
        /// and blocks until the container responds.
        #[clap(long)]
        no_wait: bool,
    },
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        CreateCommands::Account => {
            super::login::command(super::login::Args {
                browserless: false,
                signup: true,
            })
            .await
        }
        CreateCommands::App {
            path,
            workspace,
            name,
            no_gitignore,
            no_wait,
        } => command_app(AppArgs {
            path,
            workspace,
            name,
            no_gitignore,
            no_wait,
        })
        .await,
    }
}

pub struct AppArgs {
    pub path: Option<PathBuf>,
    pub workspace: Option<String>,
    pub name: Option<String>,
    pub no_gitignore: bool,
    pub no_wait: bool,
}

pub async fn command_app(args: AppArgs) -> Result<()> {
    let mut configs = Configs::new()?;

    // Surface a helpful error rather than a cryptic GQL failure when
    // unauthed. The clack flow in `railway up` will already have run
    // login by the time we get here, but a direct `railway create
    // app` invocation needs the same guard.
    if !configs.has_oauth_token() || configs.is_token_expired() {
        bail!(
            "Not signed in. Run `railway create account` (or `railway login`) first."
        );
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

    // Show GitHub repo detection (informational for now — full GH
    // App integration is a separate piece; we deploy from local
    // tarball regardless).
    if let Some(remote) = detect_github_remote(&cwd_path) {
        println!(
            "  {} GitHub remote: {} {}",
            "◇".cyan(),
            remote.full_repo_name().bold(),
            "(deploying current directory; GH App integration coming later)"
                .dimmed(),
        );
    }

    // Create the project first so the user has a landing pad even
    // if the build later fails.
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
        name: args.name.clone(),
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

    let http = reqwest::Client::new();
    let up_response = upload_deploy_tarball(
        &http,
        &hostname,
        &project_create.id,
        &environment.id,
        None,
        None,
        tarball,
    )
    .await?;
    upload_spinner.finish_and_clear();

    // Link the project to the current directory so future `railway
    // up`, `railway logs`, etc. target it. Service association is
    // resolved lazily by those commands.
    configs.link_project(
        project_create.id.clone(),
        Some(project_create.name.clone()),
        environment.id.clone(),
        Some(environment.name.clone()),
    )?;
    configs.write()?;

    println!("  {} Build queued", "✓".green());

    // Wait for the deploy URL to respond unless --no-wait.
    let live_url = if args.no_wait || up_response.deployment_domain.is_empty() {
        None
    } else {
        let url = if up_response.deployment_domain.starts_with("http") {
            up_response.deployment_domain.clone()
        } else {
            format!("https://{}", up_response.deployment_domain)
        };
        let http_short = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .ok();
        match http_short {
            Some(c) => wait_for_serving(&c, &url).await,
            None => None,
        }
    };

    println!();
    match live_url.as_deref() {
        Some(url) => {
            println!("  {} {}", "🚀".dimmed(), "Live at".bold());
            println!("     {}", url.bold().underline());
        }
        None if !up_response.deployment_domain.is_empty() => {
            let url = if up_response.deployment_domain.starts_with("http") {
                up_response.deployment_domain.clone()
            } else {
                format!("https://{}", up_response.deployment_domain)
            };
            println!("  {} {}", "⏳".dimmed(), "Still building. Your URL:".bold());
            println!("     {}", url.bold().underline());
        }
        None => {
            println!("  {} Watch the build:", "🔧".dimmed());
            println!("     {}", up_response.logs_url.bold().underline());
        }
    }
    println!();

    Ok(())
}

/// Poll the deploy URL for up to 30s, returning the URL once it stops
/// returning 5xx. Used to give the user a verified-live signal after
/// the build queues.
async fn wait_for_serving(client: &reqwest::Client, url: &str) -> Option<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        if let Ok(resp) = client.get(url).send().await {
            if !resp.status().is_server_error() {
                return Some(url.to_owned());
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
