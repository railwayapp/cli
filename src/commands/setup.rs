use std::fmt;

use super::*;
use crate::{
    commands::{
        login,
        mcp::install as mcp_install,
        skills::{self, coding_tools},
    },
    consts::{RAILWAY_API_TOKEN_ENV, RAILWAY_TOKEN_ENV},
    controllers::user::get_user,
    macros::is_stdout_terminal,
    telemetry::{self, SetupAgentPhase, SetupAgentTrackEvent},
};

const DOCS_URL: &str = "https://docs.railway.com/ai";

/// Set up your editor for Railway agent functionality (skills, MCP, login)
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: SetupCommand,
}

#[derive(Parser)]
enum SetupCommand {
    /// Install Railway agent skills + MCP server into your editor and log in
    Agent(AgentArgs),
}

#[derive(Parser)]
pub struct AgentArgs {
    /// Skip prompts and accept defaults: auto-detect installed editors, skip the login flow.
    /// Also auto-engaged when stdout is not a terminal (e.g. piped or running under CI).
    #[clap(short = 'y', long)]
    yes: bool,

    /// Configure the remote HTTP MCP server at mcp.railway.com instead of the local stdio server.
    #[clap(long)]
    remote: bool,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        SetupCommand::Agent(a) => agent_setup(a).await,
    }
}

#[derive(Clone)]
struct ToolChoice {
    slug: &'static str,
    name: &'static str,
    detected: bool,
}

impl fmt::Display for ToolChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.detected {
            write!(f, "{}  {}", self.name, "(detected)".dimmed())
        } else {
            write!(f, "{}", self.name)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum McpChoice {
    Local,
    Remote,
    Skip,
}

impl fmt::Display for McpChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            McpChoice::Local => write!(
                f,
                "Local (default)  {}",
                "— runs `railway mcp` as a stdio server".dimmed()
            ),
            McpChoice::Remote => write!(
                f,
                "Remote           {}",
                "— https://mcp.railway.com (HTTP)".dimmed()
            ),
            McpChoice::Skip => write!(f, "Skip             {}", "— don't configure MCP".dimmed()),
        }
    }
}

fn pick_mcp_choice(remote_flag: bool, non_interactive: bool) -> Result<McpChoice> {
    if remote_flag {
        return Ok(McpChoice::Remote);
    }
    if non_interactive {
        return Ok(McpChoice::Local);
    }
    let options = vec![McpChoice::Local, McpChoice::Remote, McpChoice::Skip];
    inquire::Select::new("Configure MCP server:", options)
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for MCP transport")
}

async fn agent_setup(args: AgentArgs) -> Result<()> {
    telemetry::send_setup_agent(SetupAgentTrackEvent {
        phase: SetupAgentPhase::Start,
        success: None,
        error_message: None,
        configured_clients: None,
    })
    .await;

    match agent_setup_inner(args).await {
        Ok(configured_clients) => {
            telemetry::send_setup_agent(SetupAgentTrackEvent {
                phase: SetupAgentPhase::Finish,
                success: Some(true),
                error_message: None,
                configured_clients: Some(configured_clients),
            })
            .await;
            Ok(())
        }
        Err(err) => {
            let message = err.to_string();
            telemetry::send_setup_agent(SetupAgentTrackEvent {
                phase: SetupAgentPhase::Finish,
                success: Some(false),
                error_message: Some(if message.len() > 256 {
                    message[..256].to_string()
                } else {
                    message
                }),
                configured_clients: None,
            })
            .await;
            Err(err)
        }
    }
}

async fn agent_setup_inner(args: AgentArgs) -> Result<Vec<String>> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    // Treat the run as non-interactive if the user passed -y, OR if stdout
    // isn't a TTY (piped, CI, agent-driven). Matches the convention used by
    // `interact_or!` elsewhere in this CLI.
    let non_interactive = args.yes || !is_stdout_terminal();

    println!("\n{}\n", "Railway Agent Setup".bold().cyan());

    let choices: Vec<ToolChoice> = coding_tools(&home)
        .into_iter()
        .map(|tool| ToolChoice {
            slug: tool.slug,
            name: tool.name,
            // "universal" is always treated as detected — it's our default
            // skills target and ships in every install.
            detected: tool.slug == "universal" || tool.global_parent.is_dir(),
        })
        .collect();

    let selected_slugs: Vec<String> = if non_interactive {
        let detected: Vec<String> = choices
            .iter()
            .filter(|c| c.detected)
            .map(|c| c.slug.to_string())
            .collect();
        println!("{} {}\n", "Detected:".bold(), detected.join(", ").cyan());
        detected
    } else {
        let default_indices: Vec<usize> = choices
            .iter()
            .enumerate()
            .filter(|(_, c)| c.detected)
            .map(|(i, _)| i)
            .collect();

        let picked = inquire::MultiSelect::new("Which editors should we set up?", choices.clone())
            .with_default(&default_indices)
            .with_render_config(Configs::get_render_config())
            .prompt()
            .context("Failed to prompt for editor selection")?;

        if picked.is_empty() {
            println!("{}", "No editors selected. Nothing to do.".yellow());
            return Ok(Vec::new());
        }
        picked.iter().map(|c| c.slug.to_string()).collect()
    };

    if selected_slugs.is_empty() {
        println!(
            "{}",
            "No editors detected. Re-run interactively to pick, or rerun in a TTY.".yellow()
        );
        return Ok(Vec::new());
    }

    let configured_clients = selected_slugs.clone();

    // Step 1: skills install
    let missing_skills: Vec<String> = selected_slugs
        .iter()
        .filter(|slug| !skills::skills_configured_for_slug(&home, slug))
        .cloned()
        .collect();
    if missing_skills.is_empty() {
        println!(
            "\n{} {}",
            "-".dimmed(),
            "Railway skills already configured; skipping install.".dimmed()
        );
    } else {
        skills::install_skills(&missing_skills).await?;
    }

    // Step 2: MCP install (skips universal internally — no MCP convention).
    // `--remote` short-circuits the prompt; `-y`/non-TTY defaults to local.
    let mcp_choice = pick_mcp_choice(args.remote, non_interactive)?;
    match mcp_choice {
        McpChoice::Local => install_missing_mcp(&home, &selected_slugs, false).await?,
        McpChoice::Remote => install_missing_mcp(&home, &selected_slugs, true).await?,
        McpChoice::Skip => {
            println!(
                "\n{} {}",
                "-".dimmed(),
                "Skipping MCP install. Run `railway mcp install` later to configure.".dimmed()
            );
        }
    }

    // Step 3: login
    if non_interactive {
        warn_if_not_logged_in().await;
    } else {
        ensure_logged_in_interactive().await?;
    }

    // Step 4: docs link
    println!(
        "\n{} {} {}\n",
        "\u{2713}".green().bold(),
        "Setup complete. Learn more:".bold(),
        DOCS_URL.purple()
    );

    if let Err(e) = crate::util::agent_advisory::record_setup_complete() {
        eprintln!("{}: {e}", "Warning: failed to record agent setup".yellow());
    }

    Ok(configured_clients)
}

async fn install_missing_mcp(
    home: &std::path::Path,
    selected_slugs: &[String],
    remote: bool,
) -> Result<()> {
    let missing_mcp: Vec<String> = selected_slugs
        .iter()
        .filter(|slug| slug.as_str() != "universal")
        .filter(|slug| !mcp_install::mcp_configured_for_slug(home, slug, remote))
        .cloned()
        .collect();

    if missing_mcp.is_empty() {
        println!(
            "\n{} {}",
            "-".dimmed(),
            "Railway MCP already configured; skipping install.".dimmed()
        );
        return Ok(());
    }

    mcp_install::install_mcp(&missing_mcp, remote).await
}

/// Mirrors the logic at the top of `login::command` without invoking the
/// interactive flow. Used in headless mode to surface a non-fatal warning.
async fn warn_if_not_logged_in() {
    let configs = match Configs::new() {
        Ok(c) => c,
        Err(_) => {
            print_login_warning();
            return;
        }
    };

    let token_name = if Configs::get_railway_token().is_some() {
        Some(RAILWAY_TOKEN_ENV)
    } else if Configs::get_railway_api_token().is_some() {
        Some(RAILWAY_API_TOKEN_ENV)
    } else {
        None
    };

    if let Some(name) = token_name {
        if let Ok(client) = GQLClient::new_authorized(&configs) {
            if get_user(&client, &configs).await.is_ok() {
                println!("\n{} {}", "Logged in via".bold(), name.cyan());
                return;
            }
        }
    }

    if let Ok(client) = GQLClient::new_authorized(&configs) {
        if get_user(&client, &configs).await.is_ok() {
            println!("\n{}", "Already logged in.".bold());
            return;
        }
    }

    print_login_warning();
}

fn print_login_warning() {
    println!(
        "\n{} {}",
        "!".yellow().bold(),
        "Not logged in. Run `railway login` to finish setup.".yellow()
    );
}

async fn ensure_logged_in_interactive() -> Result<()> {
    if let Ok(configs) = Configs::new() {
        if let Ok(client) = GQLClient::new_authorized(&configs) {
            if get_user(&client, &configs).await.is_ok() {
                println!("\n{}", "Already logged in.".bold());
                return Ok(());
            }
        }
    }

    println!("\n{}", "Logging in to Railway...".bold());
    login::command(login::Args {
        browserless: false,
    })
    .await
}
