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
    /// Codex is skipped because it only supports stdio MCP servers.
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

async fn agent_setup(args: AgentArgs) -> Result<()> {
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
        println!(
            "{} {}\n",
            "Detected:".bold(),
            detected.join(", ").cyan()
        );
        detected
    } else {
        let default_indices: Vec<usize> = choices
            .iter()
            .enumerate()
            .filter(|(_, c)| c.detected)
            .map(|(i, _)| i)
            .collect();

        let picked = inquire::MultiSelect::new(
            "Which editors should we set up?",
            choices.clone(),
        )
        .with_default(&default_indices)
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for editor selection")?;

        if picked.is_empty() {
            println!("{}", "No editors selected. Nothing to do.".yellow());
            return Ok(());
        }
        picked.iter().map(|c| c.slug.to_string()).collect()
    };

    if selected_slugs.is_empty() {
        println!(
            "{}",
            "No editors detected. Re-run interactively to pick, or rerun in a TTY.".yellow()
        );
        return Ok(());
    }

    // Step 1: skills install
    skills::install_skills(&selected_slugs).await?;

    // Step 2: MCP install (skips universal internally — no MCP convention)
    mcp_install::install_mcp(&selected_slugs, args.remote).await?;

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

    Ok(())
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
    login::command(login::Args { browserless: false }).await
}
