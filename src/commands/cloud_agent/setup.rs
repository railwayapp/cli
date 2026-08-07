//! `railway ca setup` — configure how cloud agents are launched.
//!
//! Entirely local: no GraphQL client, no linked project, no login. Someone
//! should be able to set their default before they have ever created an agent,
//! and this must work on a machine that has never run `railway link`.

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result};
use clap::Parser;
use colored::Colorize;

use super::prefs::{AgentPrefs, DefaultProject, SkillsPrefs};
use super::skills_sync::{self, SkillSource};
use crate::config::Configs;
use crate::macros::is_stdout_terminal;

#[derive(Parser, Default)]
pub struct Args {
    /// Skip the prompts: keep any existing preferences, and otherwise pick the
    /// first detected agent with skills sync off. Also engaged automatically
    /// when stdout is not a terminal.
    #[clap(short = 'y', long)]
    yes: bool,

    /// Print the current preferences and exit
    #[clap(long)]
    show: bool,
}

/// The harnesses a cloud agent can launch today. Every one of these ships in
/// `cloud-agent-base`; what differs is whether the launcher can carry the
/// user's credential to it, which is why droid and cursor are absent — their
/// CLI sign-ins have no copyable local form.
struct Harness {
    slug: &'static str,
    name: &'static str,
    /// Local config directory, as a `$HOME`-relative component.
    config_dir: &'static str,
    bin: &'static str,
}

const HARNESSES: &[Harness] = &[
    Harness {
        slug: "claude",
        name: "Claude Code",
        config_dir: ".claude",
        bin: "claude",
    },
    Harness {
        slug: "codex",
        name: "Codex",
        config_dir: ".codex",
        bin: "codex",
    },
    Harness {
        slug: "grok",
        name: "Grok",
        config_dir: ".grok",
        bin: "grok",
    },
];

#[derive(Clone)]
struct HarnessChoice {
    slug: &'static str,
    name: &'static str,
    detected: bool,
}

impl fmt::Display for HarnessChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.detected {
            write!(f, "{}  {}", self.name, "(detected)".dimmed())
        } else {
            write!(f, "{}", self.name)
        }
    }
}

/// A harness counts as present if its binary is on `PATH` or it has a config
/// directory. Neither alone is reliable — a user may have uninstalled the
/// binary and kept the directory, or use a version manager that hides it from
/// `which` in a non-login shell — and the list is never filtered down to what
/// was detected, only reordered, so a false negative costs nothing.
fn harness_choices(home: &Path) -> Vec<HarnessChoice> {
    HARNESSES
        .iter()
        .map(|h| HarnessChoice {
            slug: h.slug,
            name: h.name,
            detected: which::which(h.bin).is_ok() || home.join(h.config_dir).is_dir(),
        })
        .collect()
}

#[derive(Clone)]
struct SourceChoice {
    slug: &'static str,
    label: &'static str,
    count: usize,
    path: String,
}

impl fmt::Display for SourceChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}",
            self.label,
            format!("— {} skills in {}", self.count, self.path).dimmed()
        )
    }
}

pub async fn command(args: Args) -> Result<()> {
    let home = dirs::home_dir().context("Unable to get home directory")?;
    let existing = AgentPrefs::load_in(&home);

    if args.show {
        return show(&home, existing.as_ref());
    }

    let non_interactive = args.yes || !is_stdout_terminal();
    let prefs = if non_interactive {
        non_interactive_prefs(&home, existing)
    } else {
        prompt(&home, existing).await?
    };

    prefs.save_in(&home)?;
    print_summary(&home, &prefs)?;
    Ok(())
}

fn show(home: &Path, existing: Option<&AgentPrefs>) -> Result<()> {
    match existing {
        Some(prefs) => {
            println!(
                "{}",
                AgentPrefs::path_in(home).display().to_string().dimmed()
            );
            print_summary(home, prefs)?;
        }
        None => println!(
            "No cloud agent preferences yet. Run {} to create them.",
            "railway ca setup".cyan()
        ),
    }
    Ok(())
}

/// `-y` / piped: never invent a skills sync (that moves the user's files
/// without them asking), and keep whatever is already configured.
fn non_interactive_prefs(home: &Path, existing: Option<AgentPrefs>) -> AgentPrefs {
    let mut prefs = existing.unwrap_or_default();
    if prefs.agent.is_none() {
        prefs.agent = harness_choices(home)
            .into_iter()
            .find(|c| c.detected)
            .map(|c| c.slug.to_string());
    }
    prefs
}

async fn prompt(home: &Path, existing: Option<AgentPrefs>) -> Result<AgentPrefs> {
    println!("\n{}\n", "Railway Cloud Agent Setup".bold().cyan());

    let previous = existing.clone().unwrap_or_default();
    let default_project = prompt_default_project(previous.default_project.clone()).await?;
    let agent = prompt_agent(home, previous.agent.as_deref())?;
    let skills = prompt_skills(home, &previous.skills)?;

    let theme = prompt_theme(previous.theme.as_deref())?;

    Ok(AgentPrefs {
        version: super::prefs::CURRENT_VERSION,
        agent: Some(agent),
        skills,
        default_project,
        theme: Some(theme),
    })
}

/// The agent picker on its own, so a launch with no configured default can ask
/// the one question it needs instead of pushing the user through all of setup.
pub fn prompt_agent(home: &Path, current: Option<&str>) -> Result<String> {
    let choices = harness_choices(home);
    // Pre-select what they already chose; otherwise the first detected one.
    let starting = current
        .and_then(|slug| choices.iter().position(|c| c.slug == slug))
        .or_else(|| choices.iter().position(|c| c.detected))
        .unwrap_or(0);

    let picked = inquire::Select::new("Default coding agent:", choices)
        .with_starting_cursor(starting)
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for the default coding agent")?;
    Ok(picked.slug.to_string())
}

fn prompt_skills(home: &Path, current: &SkillsPrefs) -> Result<SkillsPrefs> {
    let managed = skills_sync::railway_managed_names(home);
    // Only offer sources that hold something we would actually send: a
    // directory containing nothing but Railway's own skills is not worth a
    // question, because the image already has them.
    let sources: Vec<(&'static SkillSource, Vec<String>)> = skills_sync::populated_sources(home)
        .into_iter()
        .map(|(source, names)| {
            let syncable: Vec<String> =
                names.into_iter().filter(|n| !managed.contains(n)).collect();
            (source, syncable)
        })
        .filter(|(_, names)| !names.is_empty())
        .collect();

    if sources.is_empty() {
        println!(
            "{}",
            "No personal skills found on this machine — skipping skills setup.".dimmed()
        );
        return Ok(SkillsPrefs::default());
    }

    let total: usize = sources.iter().map(|(_, n)| n.len()).sum();
    let enabled = inquire::Confirm::new(&format!(
        "Bring your own skills to cloud agents? ({total} found)"
    ))
    .with_default(current.enabled)
    .with_render_config(Configs::get_render_config())
    .prompt()
    .context("Failed to prompt for skills sync")?;

    if !enabled {
        return Ok(SkillsPrefs {
            enabled: false,
            ..current.clone()
        });
    }

    let source = if sources.len() == 1 {
        sources[0].0.slug.to_string()
    } else {
        let choices: Vec<SourceChoice> = sources
            .iter()
            .map(|(source, names)| SourceChoice {
                slug: source.slug,
                label: source.label,
                count: names.len(),
                path: display_path(home, &source.path_in(home)),
            })
            .collect();
        let starting = current
            .source
            .as_deref()
            .and_then(|slug| choices.iter().position(|c| c.slug == slug))
            .unwrap_or(0);
        inquire::Select::new("Which skills directory?", choices)
            .with_starting_cursor(starting)
            .with_render_config(Configs::get_render_config())
            .prompt()
            .context("Failed to prompt for the skills directory")?
            .slug
            .to_string()
    };

    Ok(SkillsPrefs {
        enabled: true,
        source: Some(source),
        exclude: current.exclude.clone(),
    })
}

/// What setup does about a home for cloud agents.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProjectChoice {
    Create,
    Existing,
    Keep,
    Skip,
}

impl fmt::Display for ProjectChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProjectChoice::Create => write!(
                f,
                "Create a default project  {}",
                "— a new project named \"Cloud Agents\"".dimmed()
            ),
            ProjectChoice::Existing => write!(
                f,
                "Use an existing project   {}",
                "— pick one of yours".dimmed()
            ),
            ProjectChoice::Keep => write!(f, "Keep the current default"),
            ProjectChoice::Skip => write!(
                f,
                "Skip                      {}",
                "— choose a target each time".dimmed()
            ),
        }
    }
}

/// The default home for cloud agents.
///
/// The one step here that needs the network, so it degrades rather than
/// failing: a machine that is not logged in still gets through setup, and
/// picks a target per launch instead.
async fn prompt_default_project(current: Option<DefaultProject>) -> Result<Option<DefaultProject>> {
    let mut options = Vec::new();
    if current.is_some() {
        options.push(ProjectChoice::Keep);
    }
    options.push(ProjectChoice::Create);
    options.push(ProjectChoice::Existing);
    options.push(ProjectChoice::Skip);

    if let Some(project) = &current {
        println!(
            "  {} {}/{}\n",
            "Current default:".dimmed(),
            project.project_name,
            project.environment_name
        );
    }
    let choice = inquire::Select::new("Default project for cloud agents:", options)
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for the default project")?;

    match choice {
        ProjectChoice::Keep => Ok(current),
        ProjectChoice::Skip => Ok(None),
        ProjectChoice::Create => match create_default_project().await {
            Ok(project) => {
                println!(
                    "{} {}",
                    "✓".green(),
                    format!("Created {}", project.project_name).bold()
                );
                Ok(Some(project))
            }
            Err(err) => {
                eprintln!("{} {err:#}", "Couldn't create the project:".yellow());
                Ok(current)
            }
        },
        ProjectChoice::Existing => match pick_existing_project().await {
            Ok(project) => Ok(Some(project)),
            Err(err) => {
                eprintln!("{} {err:#}", "Couldn't pick a project:".yellow());
                Ok(current)
            }
        },
    }
}

/// The project name new users get. Fixed rather than prompted: it is a home for
/// agents, not something to name.
const DEFAULT_PROJECT_NAME: &str = "Cloud Agents";

async fn create_default_project() -> Result<DefaultProject> {
    use crate::client::{GQLClient, post_graphql};
    use crate::gql::mutations;

    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let workspaces = crate::workspace::workspaces_with_client(&client, &configs).await?;
    // Which workspace: the only one, or ask. Creating a project somewhere the
    // user did not expect is worse than one more question.
    let workspace = crate::workspace::pick_workspace(workspaces, None)?;

    let created = post_graphql::<mutations::ProjectCreate, _>(
        &client,
        configs.get_backboard(),
        mutations::project_create::Variables {
            name: Some(DEFAULT_PROJECT_NAME.to_string()),
            description: Some("Home for Railway cloud agents".to_string()),
            workspace_id: Some(workspace.id().to_string()),
        },
    )
    .await?
    .project_create;

    let environment = created
        .environments
        .edges
        .first()
        .map(|edge| (edge.node.id.clone(), edge.node.name.clone()))
        .context("the new project has no environment")?;
    let _ = &mut configs;

    Ok(DefaultProject {
        project_id: created.id,
        project_name: created.name,
        environment_id: environment.0,
        environment_name: environment.1,
    })
}

async fn pick_existing_project() -> Result<DefaultProject> {
    let mut configs = Configs::new()?;
    let client = crate::client::GQLClient::new_authorized(&configs)?;
    let (project_id, environment_id) =
        crate::commands::sandbox::prompt_workspace_project_env(&client, &mut configs).await?;

    // Names for the label, from the listing we already have.
    let workspaces = crate::workspace::workspaces_with_client(&client, &configs).await?;
    for workspace in workspaces {
        for project in workspace.projects() {
            if project.id() != project_id {
                continue;
            }
            let environment = project
                .environments()
                .into_iter()
                .find(|env| env.id == environment_id);
            return Ok(DefaultProject {
                project_id,
                project_name: project.name().to_string(),
                environment_name: environment
                    .as_ref()
                    .map(|env| env.name.clone())
                    .unwrap_or_else(|| "production".into()),
                environment_id,
            });
        }
    }
    anyhow::bail!("that project is no longer listed")
}

#[derive(Clone)]
struct ThemeChoice(&'static super::tui::theme::Theme);

impl fmt::Display for ThemeChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.label)
    }
}

/// The TUI's colours. Also switchable live with ⌥t, which persists back here —
/// this is the same setting, asked once during setup.
fn prompt_theme(current: Option<&str>) -> Result<String> {
    use super::tui::theme::{THEMES, Theme};
    let choices: Vec<ThemeChoice> = THEMES.iter().map(ThemeChoice).collect();
    let starting = Theme::from_slug(current).index();
    Ok(inquire::Select::new("Theme for `railway ca`:", choices)
        .with_starting_cursor(starting)
        .with_render_config(Configs::get_render_config())
        .prompt()
        .context("Failed to prompt for the theme")?
        .0
        .slug
        .to_string())
}

fn print_summary(home: &Path, prefs: &AgentPrefs) -> Result<()> {
    let agent_name = prefs
        .agent
        .as_deref()
        .and_then(|slug| HARNESSES.iter().find(|h| h.slug == slug))
        .map(|h| h.name)
        .unwrap_or("not set");
    println!("\n  {}  {}", "Agent ".dimmed(), agent_name.bold());

    match (prefs.skills.enabled, prefs.skills.source.as_deref()) {
        (true, Some(slug)) => {
            let source = SkillSource::from_slug(slug);
            let (count, path) = match source {
                Some(source) => {
                    let dir = source.path_in(home);
                    let managed = skills_sync::railway_managed_names(home);
                    let count = skills_sync::discover_skill_names(&dir)
                        .into_iter()
                        .filter(|n| !managed.contains(n))
                        .count();
                    (count, display_path(home, &dir))
                }
                None => (0, slug.to_string()),
            };
            println!(
                "  {}  {} {}",
                "Skills".dimmed(),
                format!("{count} from {path}").bold(),
                "(Railway's own excluded — the agent already has them)".dimmed()
            );
            println!(
                "\n{}",
                "Skills sync is add-only: turning it off later leaves what is already on an agent."
                    .dimmed()
            );
        }
        _ => println!("  {}  {}", "Skills".dimmed(), "not synced".bold()),
    }

    if let Some(project) = prefs.default_project.as_ref() {
        println!(
            "  {}  {}",
            "Project".dimmed(),
            format!("{}/{}", project.project_name, project.environment_name).bold()
        );
    }
    if let Some(theme) = prefs.theme.as_deref() {
        println!(
            "  {}  {}",
            "Theme ".dimmed(),
            super::tui::theme::Theme::from_slug(Some(theme))
                .label
                .bold()
        );
    }

    println!(
        "\nSaved to {}",
        AgentPrefs::path_in(home).display().to_string().cyan()
    );
    println!(
        "Launch with {} (or {}).",
        "railway ca".cyan(),
        "railway code".cyan()
    );
    Ok(())
}

/// `~/.claude/skills` reads better than a full home path in a prompt.
fn display_path(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_harness_by_its_config_directory() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();

        let choices = harness_choices(home.path());
        let codex = choices.iter().find(|c| c.slug == "codex").unwrap();
        assert!(codex.detected);
    }

    /// Undetected harnesses stay in the list — detection orders the picker, it
    /// does not gate it.
    #[test]
    fn every_harness_is_offered() {
        let home = tempfile::tempdir().unwrap();
        let slugs: Vec<&str> = harness_choices(home.path())
            .iter()
            .map(|c| c.slug)
            .collect();
        assert_eq!(slugs, vec!["claude", "codex", "grok"]);
    }

    #[test]
    fn non_interactive_keeps_existing_preferences() {
        let home = tempfile::tempdir().unwrap();
        let existing = AgentPrefs {
            agent: Some("grok".into()),
            skills: SkillsPrefs {
                enabled: true,
                source: Some("claude".into()),
                exclude: Vec::new(),
            },
            ..Default::default()
        };
        let prefs = non_interactive_prefs(home.path(), Some(existing.clone()));
        assert_eq!(prefs, existing);
    }

    /// `-y` on a fresh machine must never turn on a sync the user did not ask
    /// for — it moves their files to a VM.
    #[test]
    fn non_interactive_never_enables_skills_by_itself() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".claude").join("skills")).unwrap();
        let prefs = non_interactive_prefs(home.path(), None);
        assert!(!prefs.skills.enabled);
    }

    #[test]
    fn non_interactive_picks_the_detected_agent() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        let prefs = non_interactive_prefs(home.path(), None);
        // `which` may also see a locally installed claude, so assert only that
        // something detected was chosen.
        assert!(prefs.agent.is_some());
    }

    #[test]
    fn display_path_shortens_under_home() {
        let home = Path::new("/home/x");
        assert_eq!(
            display_path(home, &home.join(".claude").join("skills")),
            "~/.claude/skills"
        );
        assert_eq!(display_path(home, Path::new("/opt/skills")), "/opt/skills");
    }
}
