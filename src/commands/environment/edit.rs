use std::io::Read;

use is_terminal::IsTerminal;

use super::{Edit as Args, *};
use crate::{
    controllers::{
        config::{self, EnvironmentConfig},
        project::get_project,
    },
    errors::RailwayError,
    util::prompt::{fake_select, prompt_confirm_with_default},
};

use super::new::{
    get_environment_services, parse_interactive_configs, parse_non_interactive_configs,
};

pub async fn edit_environment(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;
    let stdin_is_terminal = std::io::stdin().is_terminal();
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let is_interactive = stdin_is_terminal && stdout_is_terminal;
    let json = args.json;

    // Resolve environment: --environment flag, or linked environment
    let environment_id = resolve_environment(&args, &project, &linked_project)?;

    // Get environment name for display
    let environment_name = project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment_id)
        .map(|e| e.node.name.clone())
        .unwrap_or_else(|| environment_id.clone());

    // Get config from stdin (if piped), CLI flags, or interactive prompts
    let env_config = get_edit_config(
        &args,
        &client,
        &configs,
        &project,
        &environment_id,
        stdin_is_terminal,
    )
    .await?;

    if config::is_empty(&env_config) {
        if json {
            println!(
                "{}",
                serde_json::json!({"staged": false, "committed": false, "message": "No changes to apply"})
            );
        } else if is_interactive {
            println!("{}", "No changes to apply".yellow());
        }
        return Ok(());
    }

    // Stage changes with merge=true to combine with any existing staged changes
    let stage_vars = mutations::environment_stage_changes::Variables {
        environment_id: environment_id.clone(),
        input: env_config,
        merge: Some(true),
    };

    post_graphql::<mutations::EnvironmentStageChanges, _>(
        &client,
        configs.get_backboard(),
        stage_vars,
    )
    .await?;

    // Determine whether to stage only or apply now
    // --stage flag means stage only, --message flag implies apply now
    let should_stage_only = if args.stage {
        fake_select("Apply changes now?", "No");
        true
    } else if args.message.is_some() {
        fake_select("Apply changes now?", "Yes");
        false
    } else if is_interactive {
        !prompt_confirm_with_default("Apply changes now?", true)?
    } else {
        false
    };

    if should_stage_only {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "staged": true,
                    "committed": false,
                    "environmentId": environment_id,
                    "environmentName": environment_name
                })
            );
        } else {
            println!(
                "{} {} {}",
                "Changes staged for".green(),
                environment_name.magenta().bold(),
                "(use 'railway environment edit' to commit)".dimmed()
            );
        }
        return Ok(());
    }

    let commit_message = args
        .message
        .clone()
        .inspect(|msg| fake_select("Commit message", msg));

    // Commit the staged changes
    let commit_vars = mutations::environment_patch_commit_staged::Variables {
        environment_id: environment_id.clone(),
        commit_message: commit_message.clone(),
        skip_deploys: None,
    };

    post_graphql::<mutations::EnvironmentPatchCommitStaged, _>(
        &client,
        configs.get_backboard(),
        commit_vars,
    )
    .await?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "staged": true,
                "committed": true,
                "environmentId": environment_id,
                "environmentName": environment_name,
                "message": commit_message
            })
        );
    } else {
        let msg = commit_message
            .as_ref()
            .map(|m| format!(" ({})", m.dimmed()))
            .unwrap_or_default();
        println!(
            "{} {}{}",
            "Configuration committed for".green(),
            environment_name.magenta().bold(),
            msg
        );
    }

    Ok(())
}

/// Resolve the environment ID from --environment flag or linked environment
fn resolve_environment(
    args: &Args,
    project: &queries::project::ProjectProject,
    linked_project: &crate::config::LinkedProject,
) -> Result<String> {
    if let Some(ref env_input) = args.environment {
        // Find environment by name or ID
        let env = project.environments.edges.iter().find(|e| {
            e.node.name.to_lowercase() == env_input.to_lowercase()
                || e.node.id.to_lowercase() == *env_input.to_lowercase()
        });

        if let Some(env) = env {
            fake_select("Environment", &env.node.name);
            Ok(env.node.id.clone())
        } else {
            bail!(RailwayError::EnvironmentNotFound(env_input.clone()))
        }
    } else {
        // Use linked environment
        let env_id = linked_project.environment.clone();
        let env_name = project
            .environments
            .edges
            .iter()
            .find(|e| e.node.id == env_id)
            .map(|e| e.node.name.clone())
            .unwrap_or_else(|| env_id.clone());
        fake_select("Environment", &env_name);
        Ok(env_id)
    }
}

/// Get configuration from stdin JSON, CLI flags, or interactive prompts
async fn get_edit_config(
    args: &Args,
    client: &reqwest::Client,
    configs: &Configs,
    project: &queries::project::ProjectProject,
    environment_id: &str,
    stdin_is_terminal: bool,
) -> Result<EnvironmentConfig> {
    let all_configs = args.config.get_all_service_configs();
    let has_cli_flags = !all_configs.is_empty();

    // Priority 1: Piped stdin JSON (auto-detected)
    if !stdin_is_terminal {
        return read_config_from_stdin(project, environment_id);
    }

    // Priority 2: CLI flags (--service-config, --service-variable)
    if has_cli_flags {
        return parse_non_interactive_configs(&all_configs, project, environment_id);
    }

    // Priority 3: Interactive prompts (terminal only)
    if std::io::stdout().is_terminal() {
        return parse_interactive_configs(client, configs, project, environment_id, None).await;
    }

    // No input available
    Ok(EnvironmentConfig::default())
}

/// Read and parse JSON config from stdin
fn read_config_from_stdin(
    project: &queries::project::ProjectProject,
    environment_id: &str,
) -> Result<EnvironmentConfig> {
    let stdin = std::io::stdin();
    let mut input = String::new();
    stdin.lock().read_to_string(&mut input)?;

    let input = input.trim();
    if input.is_empty() {
        return Ok(EnvironmentConfig::default());
    }

    // Try to parse as EnvironmentConfig directly
    let config: EnvironmentConfig = serde_json::from_str(input)
        .context("Failed to parse stdin as JSON. Expected EnvironmentConfig format.")?;

    // Validate that referenced services exist
    let services = get_environment_services(project, environment_id)?;
    let service_ids: Vec<&str> = services
        .iter()
        .map(|s| s.node.service_id.as_str())
        .collect();

    for service_id in config.services.keys() {
        if !service_ids.contains(&service_id.as_str()) {
            // Check if it's a service name instead of ID
            let found = services
                .iter()
                .any(|s| s.node.service_name.to_lowercase() == service_id.to_lowercase());
            if !found {
                bail!(
                    "Service '{}' not found in environment. Available services: {}",
                    service_id,
                    services
                        .iter()
                        .map(|s| s.node.service_name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }

    fake_select("Input", "stdin (JSON)");

    Ok(config)
}
