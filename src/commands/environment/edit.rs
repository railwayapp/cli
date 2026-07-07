use std::io::Read;

use is_terminal::IsTerminal;

use super::{Edit as Args, *};
use crate::{
    controllers::{
        config::{self, EnvironmentConfig},
        environment::get_matched_environment,
        project::{ProjectEnvironmentInstances, get_environment_instances, get_project},
        staged_changes::{flatten_value, staged_changes_notice},
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
    let linked_project = if args.project.is_some() {
        None
    } else {
        Some(configs.get_linked_project().await?)
    };
    let project_id = args
        .project
        .clone()
        .or_else(|| linked_project.as_ref().map(|linked| linked.project.clone()))
        .ok_or_else(|| RailwayError::NoLinkedProject)?;
    let project = get_project(&client, &configs, project_id.clone()).await?;
    let stdin_is_terminal = std::io::stdin().is_terminal();
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let is_interactive = stdin_is_terminal && stdout_is_terminal;
    let json = args.json;

    // Resolve environment: --environment flag, or linked environment
    let environment_id = resolve_environment(&args, &project, linked_project.as_ref())?;

    // Get environment name for display
    let environment_name = project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment_id)
        .map(|e| e.node.name.clone())
        .unwrap_or_else(|| environment_id.clone());
    let environment_instances =
        get_environment_instances(&client, &configs, &project_id, &environment_id).await?;

    // Get config from stdin (if piped), CLI flags, or interactive prompts
    let env_config = get_edit_config(
        &args,
        &client,
        &configs,
        &environment_instances,
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
        } else {
            println!("{}", "No changes to apply".yellow());
        }
        return Ok(());
    }

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
        let staged_count = serde_json::to_value(&env_config)
            .map(|value| flatten_value(&value).len())
            .unwrap_or(1);
        // Stage only: use environmentStageChanges with merge=true
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
                "{} {}",
                "Changes staged for".green(),
                environment_name.magenta().bold(),
            );
            println!("{}", staged_changes_notice(staged_count));
        }
        return Ok(());
    }

    let commit_message = args
        .message
        .clone()
        .inspect(|msg| fake_select("Commit message", msg));

    // Commit directly with patch (single mutation instead of stage + commit)
    let commit_vars = mutations::environment_patch_commit::Variables {
        environment_id: environment_id.clone(),
        patch: env_config,
        commit_message: commit_message.clone(),
    };

    post_graphql::<mutations::EnvironmentPatchCommit, _>(
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
    linked_project: Option<&crate::config::LinkedProject>,
) -> Result<String> {
    if let Some(ref env_input) = args.environment {
        // Find environment by name or ID
        let env = project.environments.edges.iter().find(|e| {
            e.node.name.to_lowercase() == env_input.to_lowercase()
                || e.node.id.to_lowercase() == *env_input.to_lowercase()
        });

        if let Some(env) = env {
            let environment = get_matched_environment(project, env.node.id.clone())?;
            fake_select("Environment", &environment.name);
            Ok(environment.id)
        } else {
            bail!(RailwayError::EnvironmentNotFound(env_input.clone()))
        }
    } else {
        // Use linked environment
        let linked_project = linked_project.ok_or_else(|| {
            anyhow::anyhow!(
                "No environment specified. Use --environment or run `railway link` to link one."
            )
        })?;
        let env_id = linked_project.environment_id()?.to_string();
        let environment = get_matched_environment(project, env_id)?;
        fake_select("Environment", &environment.name);
        Ok(environment.id)
    }
}

/// Get configuration from stdin JSON, CLI flags, or interactive prompts
async fn get_edit_config(
    args: &Args,
    client: &reqwest::Client,
    configs: &Configs,
    environment_instances: &ProjectEnvironmentInstances,
    environment_id: &str,
    stdin_is_terminal: bool,
) -> Result<EnvironmentConfig> {
    let all_configs = args.config.get_all_service_configs();
    let has_cli_flags = !all_configs.is_empty();

    // Priority 1: CLI flags (--service-config, --service-variable). Flags must
    // win over piped stdin: scripts commonly run with a non-terminal stdin, and
    // reading (empty) stdin first would silently drop the flags.
    if has_cli_flags {
        return parse_non_interactive_configs(&all_configs, environment_instances);
    }

    // Priority 2: Piped stdin JSON (auto-detected)
    if !stdin_is_terminal {
        return read_config_from_stdin(environment_instances);
    }

    // Priority 3: Interactive prompts (terminal only)
    if std::io::stdout().is_terminal() {
        return parse_interactive_configs(
            client,
            configs,
            environment_instances,
            environment_id,
            None,
        )
        .await;
    }

    // No input available
    Ok(EnvironmentConfig::default())
}

/// Read and parse JSON config from stdin
fn read_config_from_stdin(
    environment_instances: &ProjectEnvironmentInstances,
) -> Result<EnvironmentConfig> {
    let stdin = std::io::stdin();
    let mut input = String::new();
    stdin.lock().read_to_string(&mut input)?;

    let input = input.trim();
    if input.is_empty() {
        return Ok(EnvironmentConfig::default());
    }

    // Try to parse as EnvironmentConfig directly
    let mut config: EnvironmentConfig = serde_json::from_str(input)
        .context("Failed to parse stdin as JSON. Expected EnvironmentConfig format.")?;

    // Resolve service keys (IDs or names) to canonical service IDs so the
    // staged patch is always keyed by ID. Staging under a name key would
    // produce a patch the rest of the platform doesn't recognize.
    let services = get_environment_services(environment_instances);
    let mut services_by_id = std::collections::BTreeMap::new();
    for (key, service_config) in std::mem::take(&mut config.services) {
        let service = services
            .iter()
            .find(|s| s.node.service_id.to_lowercase() == key.to_lowercase())
            .or_else(|| {
                services
                    .iter()
                    .find(|s| s.node.service_name.to_lowercase() == key.to_lowercase())
            });
        let Some(service) = service else {
            bail!(
                "Service '{}' not found in environment. Available services: {}",
                key,
                services
                    .iter()
                    .map(|s| s.node.service_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        };
        if services_by_id
            .insert(service.node.service_id.clone(), service_config)
            .is_some()
        {
            bail!(
                "Service '{}' is configured more than once (e.g. by both name and ID)",
                service.node.service_name
            );
        }
    }
    config.services = services_by_id;

    fake_select("Input", "stdin (JSON)");

    Ok(config)
}
