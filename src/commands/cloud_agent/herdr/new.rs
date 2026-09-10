//! `railway ca herdr new`: pick a place, name the agent, create it, and hand
//! the VM to herdr as a saved machine.

use std::fmt::Display;

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;
use inquire::validator::Validation;

use crate::client::GQLClient;
use crate::config::Configs;
use crate::controllers::cloud_agent as ca;
use crate::util::progress::create_spinner;
use crate::workspace::{self, Workspace};

use super::herdr_cli::{Herdr, Machine};
use super::state::Store;
use super::target;

#[derive(Parser)]
pub struct Args {
    /// Name for the agent (defaults to a generated one)
    #[clap(value_name = "NAME")]
    name: Option<String>,

    /// Project ID (skips the project picker)
    #[clap(long, short)]
    project: Option<String>,

    /// Environment ID (skips the environment picker)
    #[clap(long, short)]
    environment: Option<String>,

    #[clap(flatten)]
    harness: super::harness::HarnessFlags,

    /// Open this flow in a herdr popup pane instead of running it here
    #[clap(long)]
    open: bool,

    /// Pick and name only; print what would be created without creating it
    #[clap(long)]
    dry_run: bool,
}

impl Args {
    /// The picker's "new agent": every step asked, nothing skipped.
    pub(super) fn interactive() -> Self {
        Self {
            name: None,
            project: None,
            environment: None,
            harness: Default::default(),
            open: false,
            dry_run: false,
        }
    }
}

pub async fn command(args: Args) -> Result<()> {
    if args.open {
        return Herdr::from_env().plugin_pane_open(super::PLUGIN_ID, "new");
    }

    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let store = Store::new(&configs)?;
    let workspaces = workspace::workspaces_with_client(&client, &configs).await?;
    let rows = rows(&workspaces);
    let row = pick_project(rows, args.project.as_deref(), args.environment.as_deref())?;
    let environment =
        match choose_environment(row.environments.clone(), args.environment.as_deref())? {
            EnvChoice::Chosen(env) => env,
            EnvChoice::Ask(envs) => inquire::Select::new("Environment", envs)
                .with_render_config(Configs::get_render_config())
                .prompt()
                .context("Failed to prompt for environment")?,
        };
    let name = match args.name {
        Some(name) => {
            if !valid_name(&name) {
                bail!("Invalid agent name {name:?}: {NAME_RULE}");
            }
            Some(name)
        }
        None => prompt_name()?,
    };

    let harness = super::harness::choose(&args.harness, true)?;

    if args.dry_run {
        let shown = name.as_deref().unwrap_or("<generated>");
        let target = target::target_for(&environment.id, "<agent-id>");
        let label = target::label(&row.project_name, shown);
        println!("{}", "Dry run, nothing created.".dimmed());
        println!("  agent        {shown}");
        println!("  project      {} ({})", row.project_name, row.project_id);
        println!("  environment  {} ({})", environment.name, environment.id);
        println!("  agent type   {harness}");
        println!("  herdr        machine add {target} --label {label:?}");
        return Ok(());
    }

    let backboard = configs.get_backboard();
    let spinner = create_spinner("Creating a cloud agent".to_string());
    let agent = ca::create(&client, &backboard, &environment.id, name, None)
        .await
        .inspect_err(|_| spinner.finish_and_clear())?;
    ca::remember(&mut configs, &agent)?;
    spinner.set_message(format!("Waiting for agent {} to start", agent.name));
    let agent = ca::wait_until_running(&client, &backboard, &environment.id, &agent.id)
        .await
        .inspect_err(|_| spinner.finish_and_clear())?;
    spinner.finish_and_clear();
    println!("✓ Created agent {}", agent.name.cyan());
    super::watch::nudge();

    let herdr = Herdr::from_env();
    let target = target::target(&agent);
    let label = target::label(&row.project_name, &agent.name);
    super::known_hosts::ensure_relay_known_host()?;
    let spinner = create_spinner(format!("Waiting for {}'s ssh relay", agent.name));
    let ready = super::relay::wait_until_ready(&agent).await;
    spinner.finish_and_clear();
    ready?;
    herdr.machine_add(&target, &label)?;

    match herdr
        .machines()
        .map(|machines| profile_id_for(&machines, &target))
    {
        Ok(Some(profile)) => {
            store
                .update(|s| {
                    s.machines.insert(agent.id.clone(), profile);
                })
                .await?;
        }
        Ok(None) => {
            warn("herdr did not list the new machine; `railway ca herdr sync` will record it")
        }
        Err(e) => warn(&format!("could not read herdr machines: {e:#}")),
    }

    super::bootstrap::run(&agent, harness, &store).await?;

    println!(
        "\n{} is in the herdr sidebar as {}, provisioned for {}.",
        agent.name.cyan(),
        label.cyan(),
        harness.cyan()
    );
    Ok(())
}

fn warn(message: &str) {
    eprintln!("{} {message}", "warning:".yellow());
}

const NAME_RULE: &str =
    "1-63 characters, letters, digits, '.', '_' or '-', starting with a letter or digit";

/// Backboard's grammar: `^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$`.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && name.len() <= 63
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn prompt_name() -> Result<Option<String>> {
    let validator = |input: &str| {
        if input.trim().is_empty() || valid_name(input.trim()) {
            Ok(Validation::Valid)
        } else {
            Ok(Validation::Invalid(NAME_RULE.into()))
        }
    };
    let name = inquire::Text::new("Agent name")
        .with_render_config(Configs::get_render_config())
        .with_placeholder("leave empty for a generated name")
        .with_validator(validator)
        .prompt()
        .context("Failed to prompt for agent name")?;
    let name = name.trim();
    Ok((!name.is_empty()).then(|| name.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    workspace: String,
    project_id: String,
    project_name: String,
    /// Only the environments this user can act in.
    environments: Vec<Env>,
}

impl Display for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} / {}", self.workspace, self.project_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Env {
    id: String,
    name: String,
}

impl Display for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

fn rows(workspaces: &[Workspace]) -> Vec<Row> {
    let mut rows: Vec<Row> = workspaces
        .iter()
        .flat_map(|w| {
            w.projects()
                .into_iter()
                .filter(|p| p.deleted_at().is_none())
                .map(move |p| Row {
                    workspace: w.name().to_string(),
                    project_id: p.id().to_string(),
                    project_name: p.name().to_string(),
                    environments: p
                        .environments()
                        .into_iter()
                        .filter(|e| e.can_access)
                        .map(|e| Env {
                            id: e.id,
                            name: e.name,
                        })
                        .collect(),
                })
        })
        .collect();
    rows.sort_by(|a, b| {
        a.workspace
            .to_lowercase()
            .cmp(&b.workspace.to_lowercase())
            .then_with(|| {
                a.project_name
                    .to_lowercase()
                    .cmp(&b.project_name.to_lowercase())
            })
    });
    rows
}

fn pick_project(
    rows: Vec<Row>,
    project_id: Option<&str>,
    environment_id: Option<&str>,
) -> Result<Row> {
    if rows.is_empty() {
        bail!("No projects found in any of your workspaces.");
    }
    if let Some(id) = project_id {
        return rows
            .into_iter()
            .find(|r| r.project_id == id)
            .ok_or_else(|| anyhow::anyhow!("No project with id {id} in your workspaces."));
    }
    if let Some(id) = environment_id {
        return rows
            .into_iter()
            .find(|r| r.environments.iter().any(|e| e.id == id))
            .ok_or_else(|| anyhow::anyhow!("No environment with id {id} in your workspaces."));
    }
    inquire::Select::new("Project", rows)
        .with_render_config(Configs::get_render_config())
        .with_page_size(15)
        .prompt()
        .context("Failed to prompt for project")
}

#[derive(Debug, PartialEq, Eq)]
enum EnvChoice {
    Chosen(Env),
    Ask(Vec<Env>),
}

fn choose_environment(mut environments: Vec<Env>, requested: Option<&str>) -> Result<EnvChoice> {
    if let Some(id) = requested {
        return match environments.iter().position(|e| e.id == id || e.name == id) {
            Some(i) => Ok(EnvChoice::Chosen(environments.swap_remove(i))),
            None => bail!("Environment {id} is not one you can access in this project."),
        };
    }
    match environments.len() {
        0 => bail!("You have no accessible environments in this project."),
        1 => Ok(EnvChoice::Chosen(environments.remove(0))),
        _ => Ok(EnvChoice::Ask(environments)),
    }
}

fn profile_id_for(machines: &[Machine], target: &str) -> Option<String> {
    machines
        .iter()
        .find(|m| m.target == target)
        .map(|m| m.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(id: &str, name: &str) -> Env {
        Env {
            id: id.into(),
            name: name.into(),
        }
    }

    fn row(project_id: &str, project_name: &str, environments: Vec<Env>) -> Row {
        Row {
            workspace: "Railway".into(),
            project_id: project_id.into(),
            project_name: project_name.into(),
            environments,
        }
    }

    #[test]
    fn name_grammar_matches_backboard() {
        assert!(valid_name("a"));
        assert!(valid_name("Reviewer-1.2_x"));
        assert!(valid_name(&"a".repeat(63)));
        assert!(!valid_name(""));
        assert!(!valid_name("-lead"));
        assert!(!valid_name(".dot"));
        assert!(!valid_name("has space"));
        assert!(!valid_name("naïve"));
        assert!(!valid_name(&"a".repeat(64)));
    }

    #[test]
    fn rows_render_workspace_slash_project() {
        assert_eq!(
            row("p1", "orchestrator", vec![]).to_string(),
            "Railway / orchestrator"
        );
    }

    #[test]
    fn sole_environment_is_chosen_without_asking() {
        assert_eq!(
            choose_environment(vec![env("e1", "production")], None).unwrap(),
            EnvChoice::Chosen(env("e1", "production"))
        );
    }

    #[test]
    fn several_environments_are_asked_about() {
        let envs = vec![env("e1", "production"), env("e2", "staging")];
        assert!(matches!(
            choose_environment(envs, None).unwrap(),
            EnvChoice::Ask(v) if v.len() == 2
        ));
    }

    #[test]
    fn requested_environment_resolves_by_id_or_name() {
        let envs = vec![env("e1", "production"), env("e2", "staging")];
        assert_eq!(
            choose_environment(envs.clone(), Some("e2")).unwrap(),
            EnvChoice::Chosen(env("e2", "staging"))
        );
        assert_eq!(
            choose_environment(envs.clone(), Some("production")).unwrap(),
            EnvChoice::Chosen(env("e1", "production"))
        );
        assert!(choose_environment(envs, Some("nope")).is_err());
    }

    #[test]
    fn no_environment_is_an_error() {
        assert!(choose_environment(vec![], None).is_err());
    }

    #[test]
    fn project_or_environment_flag_skips_the_picker() {
        let rows = vec![
            row("p1", "one", vec![env("e1", "prod")]),
            row("p2", "two", vec![env("e2", "prod")]),
        ];
        assert_eq!(
            pick_project(rows.clone(), Some("p2"), None)
                .unwrap()
                .project_id,
            "p2"
        );
        assert_eq!(
            pick_project(rows.clone(), None, Some("e1"))
                .unwrap()
                .project_id,
            "p1"
        );
        assert!(pick_project(rows.clone(), Some("p9"), None).is_err());
        assert!(pick_project(rows, None, Some("e9")).is_err());
        assert!(pick_project(vec![], None, None).is_err());
    }

    // The fake herdr is a shebang script: unix only.

    #[cfg(unix)]
    #[test]
    fn profile_id_is_looked_up_by_target_after_add() {
        let target = target::target_for("env-1", "agent-1");
        let fake = super::super::herdr_cli::fake::FakeHerdr::with_machines(&format!(
            r#"[{{"id":"0123456789abcdef0123456789abcdef","label":"p/a","target":"{target}","enabled":true}}]"#
        ));
        let herdr = fake.herdr();
        herdr.machine_add(&target, "p/a").unwrap();
        let machines = herdr.machines().unwrap();
        assert_eq!(
            profile_id_for(&machines, &target).as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(profile_id_for(&machines, "someone@elsewhere"), None);
        assert_eq!(fake.calls()[0], format!("machine add {target} --label p/a"));
    }
}
