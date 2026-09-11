//! Save configured VMs and select reusable defaults for the linked environment.
use anyhow::{Result, bail};
use clap::Parser;

use super::lifecycle::TargetArgs;
use crate::{
    client::GQLClient,
    commands::sandbox::variables_to_input,
    config::Configs,
    controllers::{agent_bootstrap as bootstrap, cloud_agent as ca},
    util::progress::create_spinner,
};

#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Parser)]
enum Command {
    /// List bootstraps in the linked project/environment
    #[clap(visible_alias = "ls")]
    List(ListArgs),
    /// Save a running VM as a named bootstrap (or a new version of that name)
    Save(SaveArgs),
    /// Select the local default bootstrap for the linked project/environment
    Default(DefaultArgs),
}

#[derive(Parser)]
struct ListArgs {
    #[clap(flatten)]
    target: TargetArgs,
    /// Output as JSON
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct SaveArgs {
    /// Bootstrap name, unique within this environment
    name: String,
    /// Running VM to capture, by name or ID
    #[clap(long, value_name = "AGENT")]
    agent: Option<String>,
    /// Make this the local default after its capture succeeds
    #[clap(long)]
    default: bool,
    /// Bootstrap variables; --variable overrides entries from --env-file
    #[clap(long = "variable", value_name = "KEY=VALUE[,KEY=VALUE...]")]
    variables: Vec<String>,
    /// Load bootstrap variables from a .env file (repeatable)
    #[clap(long = "env-file", value_name = "PATH")]
    env_files: Vec<std::path::PathBuf>,
    #[clap(flatten)]
    target: TargetArgs,
    /// Output as JSON
    #[clap(long)]
    json: bool,
}

#[derive(Parser)]
struct DefaultArgs {
    name: String,
    #[clap(flatten)]
    target: TargetArgs,
    /// Output as JSON
    #[clap(long)]
    json: bool,
}

pub async fn command(args: Args) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let url = configs.get_backboard();
    match args.command {
        Command::List(args) => {
            let env = args.target.resolve(&mut configs, &client).await?;
            let entries = bootstrap::list(&configs, &client, &url, &env).await?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else if entries.is_empty() {
                println!(
                    "No bootstraps in this environment. Save one with `railway ca bootstrap save <name> --agent <vm>`."
                );
            } else {
                println!("  {:<28} {:<10} LAST SAVED", "NAME", "STATUS");
                for b in entries {
                    println!(
                        "{} {:<28} {:<10} {}",
                        if b.is_default { "*" } else { " " },
                        b.name,
                        b.status,
                        b.updated_at
                    );
                    if let Some(reason) = b.failure_reason {
                        println!("  {reason}");
                    }
                }
                println!("\n* local default for this environment");
            }
        }
        Command::Default(args) => {
            let env = args.target.resolve(&mut configs, &client).await?;
            let mut b = bootstrap::select(
                bootstrap::list(&configs, &client, &url, &env).await?,
                &args.name,
            )?;
            b.require_ready()?;
            b.is_default = configs
                .set_agent_bootstrap_default(&env, &b.id, false)
                .await?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&b)?);
            } else {
                println!(
                    "'{}' is now the local default bootstrap for this environment.",
                    b.name
                );
            }
        }
        Command::Save(args) => {
            let env = args.target.resolve(&mut configs, &client).await?;
            let (agent, _) =
                ca::resolve(&configs, &client, args.agent.as_deref(), Some(&env)).await?;
            let variables = variables_to_input(&args.env_files, &args.variables)?
                .map(serde_json::to_value)
                .transpose()?;
            let b = save_from_agent(
                &mut configs,
                &client,
                &url,
                &agent,
                &args.name,
                args.default,
                variables,
                args.json,
            )
            .await?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&b)?);
            } else {
                println!(
                    "Saved bootstrap '{}'{}.",
                    b.name,
                    if b.is_default { " (default)" } else { "" }
                );
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn save_from_agent(
    configs: &mut Configs,
    client: &reqwest::Client,
    url: &str,
    agent: &ca::Agent,
    name: &str,
    make_default: bool,
    variables: Option<serde_json::Value>,
    quiet: bool,
) -> Result<bootstrap::Bootstrap> {
    if agent.status != ca::Status::Running {
        bail!(
            "'{}' must be running before it can be saved as a bootstrap.",
            agent.name
        );
    }
    let existing = bootstrap::list(configs, client, url, &agent.environment_id)
        .await?
        .into_iter()
        .find(|b| b.name == name);
    let was_default = existing.as_ref().is_some_and(|b| b.is_default);
    let spinner = (!quiet).then(|| create_spinner(format!("Saving bootstrap '{name}'")));
    let result = async {
        let saved = bootstrap::save(
            client,
            url,
            &agent.id,
            name,
            existing.map(|b| b.id),
            variables,
        )
        .await?;
        let mut b = bootstrap::wait_ready(client, url, saved).await?;
        b.is_default = configs
            .set_agent_bootstrap_default(&agent.environment_id, &b.id, !make_default)
            .await?;
        Ok(b)
    }
    .await;
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }
    if result.is_err() && was_default && !quiet {
        eprintln!(
            "This saved a new version of the current default. Check its status before launching another VM."
        );
    }
    result
}

/// The management TUI releases the terminal for this form, then restores its panes.
pub async fn configure(agent_id: &str, environment_id: &str) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let url = configs.get_backboard();
    let agent = ca::get(&client, &url, environment_id, agent_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("The selected VM is no longer available."))?;
    if agent.status != ca::Status::Running {
        bail!("Wake '{}' before saving a bootstrap.", agent.name);
    }
    let entries = bootstrap::list(&configs, &client, &url, environment_id).await?;
    println!(
        "\nSave {} as a reusable bootstrap. Its disk is captured; the VM stays running.",
        agent.name
    );
    println!(
        "Files and installed tools are included. Startup services should be configured in /etc/railway/bootstrap/startup.sh."
    );
    let Some(name) = inquire::Text::new("Bootstrap name:")
        .with_default(&agent.name)
        .with_validator(inquire::validator::ValueRequiredValidator::default())
        .prompt_skippable()?
    else {
        return Ok(());
    };
    if entries.iter().any(|b| b.name == name)
        && !inquire::Confirm::new("Save a new version of this existing bootstrap?")
            .with_default(false)
            .prompt()?
    {
        return Ok(());
    }
    let make_default = if entries.iter().any(|b| b.is_default) {
        inquire::Confirm::new("Make this the local default for this environment?")
            .with_default(false)
            .prompt()?
    } else {
        true
    };
    let b = save_from_agent(
        &mut configs,
        &client,
        &url,
        &agent,
        &name,
        make_default,
        None,
        false,
    )
    .await?;
    println!(
        "Saved '{}'{}.",
        b.name,
        if b.is_default {
            " as your local default bootstrap"
        } else {
            ""
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MockBackboard;
    use serde_json::json;

    fn source() -> ca::Agent {
        ca::Agent {
            id: "source".into(),
            name: "configured".into(),
            status: ca::Status::Running,
            project_id: "project".into(),
            environment_id: "env".into(),
            created_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn bootstrap_failed_capture_never_changes_default() {
        let server = MockBackboard::spawn();
        let dir = tempfile::tempdir().unwrap();
        let mut configs = server.configs(&dir);
        configs
            .set_agent_bootstrap_default("env", "other", false)
            .await
            .unwrap();
        server.stub("AgentBootstraps", json!({"agentBootstraps": []}));
        server.stub(
            "AgentBootstrapSave",
            json!({"agentBootstrapSave": {
                "id": "new", "name": "dev", "environmentId": "env", "status": "DEGRADED",
                "failureReason": "capture failed", "updatedAt": "2026-09-11T00:00:00Z"
            }}),
        );
        let error = save_from_agent(
            &mut configs,
            &reqwest::Client::new(),
            &server.url(),
            &source(),
            "dev",
            true,
            None,
            true,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("capture failed"));
        configs.reload().unwrap();
        assert_eq!(configs.get_agent_bootstrap_default("env"), Some("other"));
    }

    #[tokio::test]
    async fn bootstrap_existing_name_saves_version_without_stealing_default() {
        let server = MockBackboard::spawn();
        let dir = tempfile::tempdir().unwrap();
        let mut configs = server.configs(&dir);
        configs
            .set_agent_bootstrap_default("env", "other", false)
            .await
            .unwrap();
        let row = json!({"id": "existing", "name": "dev", "environmentId": "env", "status": "READY",
            "failureReason": null, "updatedAt": "2026-09-11T00:00:00Z"});
        server.stub("AgentBootstraps", json!({"agentBootstraps": [row.clone()]}));
        server.stub("AgentBootstrapSave", json!({"agentBootstrapSave": row}));
        let saved = save_from_agent(
            &mut configs,
            &reqwest::Client::new(),
            &server.url(),
            &source(),
            "dev",
            false,
            None,
            true,
        )
        .await
        .unwrap();
        assert!(!saved.is_default);
        assert_eq!(
            server.variables_for("AgentBootstrapSave")[0]["input"]["id"],
            "existing"
        );
        assert_eq!(configs.get_agent_bootstrap_default("env"), Some("other"));
    }

    #[tokio::test]
    async fn bootstrap_first_successful_save_becomes_local_default() {
        let server = MockBackboard::spawn();
        let dir = tempfile::tempdir().unwrap();
        let mut configs = server.configs(&dir);
        server.stub("AgentBootstraps", json!({"agentBootstraps": []}));
        let row = |status| {
            json!({"id": "new", "name": "dev", "environmentId": "env", "status": status,
            "failureReason": null, "updatedAt": "2026-09-11T00:00:00Z"})
        };
        server.stub(
            "AgentBootstrapSave",
            json!({"agentBootstrapSave": row("SAVING")}),
        );
        server.stub("AgentBootstrap", json!({"agentBootstrap": row("READY")}));
        let saved = save_from_agent(
            &mut configs,
            &reqwest::Client::new(),
            &server.url(),
            &source(),
            "dev",
            false,
            None,
            true,
        )
        .await
        .unwrap();
        assert!(saved.is_default);
        configs.reload().unwrap();
        assert_eq!(configs.get_agent_bootstrap_default("env"), Some("new"));
        assert_eq!(server.requests().len(), 3);
    }

    #[test]
    fn bootstrap_command_forms() {
        for args in [
            vec![
                "bootstrap",
                "list",
                "--json",
                "--project",
                "project",
                "--environment",
                "staging",
            ],
            vec![
                "bootstrap",
                "save",
                "dev",
                "--agent",
                "configured",
                "--default",
                "--variable",
                "MODE=dev",
            ],
            vec!["bootstrap", "default", "dev"],
        ] {
            assert!(Args::try_parse_from(args).is_ok());
        }
    }
}
