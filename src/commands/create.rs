use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Subcommand;
use indicatif::{ProgressBar, ProgressStyle};
use is_terminal::IsTerminal;

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

        /// Accept all defaults — skip the project-name prompt and use
        /// the current directory's name. The browser still has to
        /// open if you need to sign in (run `railway login` first if
        /// you want a fully unattended flow).
        #[clap(short = 'y', long)]
        yes: bool,
    },
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        CreateCommands::Account => {
            // Backed by the same OAuth flow as `railway login` — the
            // backend detects fresh-account state via user.createdAt
            // and adapts the consent screen + post-auth landing, so
            // we don't need a separate signup signal here.
            super::login::command(super::login::Args {
                browserless: false,
            })
            .await
        }
        CreateCommands::App {
            path,
            workspace,
            name,
            no_gitignore,
            no_wait,
            yes,
        } => command_app(AppArgs {
            path,
            workspace,
            name,
            no_gitignore,
            no_wait,
            yes,
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
    pub yes: bool,
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

    // Resolve the project name:
    //   --name foo        → "foo"
    //   -y, no --name     → current directory basename (or backboard-
    //                       generated if there isn't one we can read)
    //   interactive TTY   → prompt with the directory basename as
    //                       default; user can hit Enter to accept
    //   non-TTY, no -y    → fall back to directory basename (better
    //                       than failing here; backend can rename
    //                       later)
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

    // Reuse the GQLClient::new_authorized reqwest client — it bakes
    // the bearer token into default headers, which backboard's
    // /project/:id/environment/:id/up endpoint requires. A fresh
    // reqwest::Client::new() here will 401.
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

    // Link the project to the current directory so future `railway
    // up`, `railway logs`, etc. target it.
    configs.link_project(
        project_create.id.clone(),
        Some(project_create.name.clone()),
        environment.id.clone(),
        Some(environment.name.clone()),
    )?;

    // backboard's /up endpoint creates a service implicitly but
    // doesn't return its id in the response on master. Extract it
    // from the logs_url (shape: .../project/<pid>/service/<sid>?...)
    // so `railway logs` and friends work right after `create app`.
    if let Some(service_id) = parse_service_id_from_logs_url(&up_response.logs_url) {
        configs.link_service(service_id)?;
    }

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

/// Extract the service ID from a logs URL of shape
/// `.../project/{project_id}/service/{service_id}?...`. Returns
/// None if the URL doesn't contain a `/service/<uuid>` segment.
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
