use strum::IntoEnumIterator;

use super::{New as Args, changes::Change, *};
use crate::{
    controllers::config::{
        self, EnvironmentConfig, PatchEntry, environment::fetch_environment_config,
    },
    util::progress::create_spinner_if,
};

pub async fn new_environment(args: Args) -> Result<()> {
    let mut configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;
    let project_id = project.id.clone();
    let is_terminal = std::io::stdout().is_terminal();
    let json = args.json;

    let name = select_name_new(&args, is_terminal)?;
    let duplicate_id = select_duplicate_id_new(&args, &project, is_terminal)?;

    let env_config = if let Some(ref duplicate_id) = duplicate_id {
        edit_services_select(&args, &client, &configs, &project, duplicate_id.clone()).await?
    } else {
        EnvironmentConfig::default()
    };
    let has_config_changes = !config::is_empty(&env_config);

    let vars = mutations::environment_create::Variables {
        project_id: project.id.clone(),
        name,
        source_id: duplicate_id.clone(),
        // Apply duplication in background if we're duplicating, we'll wait for it
        apply_changes_in_background: duplicate_id.as_ref().map(|_| true),
    };

    let spinner = create_spinner_if(!json, "Creating environment...".into());

    let response =
        post_graphql::<mutations::EnvironmentCreate, _>(&client, &configs.get_backboard(), vars)
            .await?;

    let env_id = response.environment_create.id.clone();
    let env_name = response.environment_create.name.clone();

    if duplicate_id.is_some() {
        // Wait for background duplication to complete before applying config changes
        if let Some(ref s) = spinner {
            s.set_message("Waiting for environment to duplicate...");
        }
        let _ = wait_for_environment_creation(&client, &configs, env_id.clone()).await;
    }

    // Apply config changes if any
    if has_config_changes {
        if let Some(ref s) = spinner {
            s.set_message("Applying configuration...");
        }
        apply_environment_config(&client, &configs, &env_id, env_config).await?;
    }

    if json {
        println!("{}", serde_json::json!({"id": env_id, "name": env_name}));
    } else if let Some(spinner) = spinner {
        spinner.finish_with_message(format!(
            "{} {} {}",
            "Environment".green(),
            env_name.magenta().bold(),
            "created!".green()
        ));
    }

    configs.link_project(
        project_id,
        linked_project.name.clone(),
        env_id,
        Some(env_name),
    )?;

    Ok(())
}

/// Collects service configuration changes either interactively or from CLI flags.
/// Returns an EnvironmentConfig with only the changed fields set.
pub async fn edit_services_select(
    args: &Args,
    client: &reqwest::Client,
    configs: &Configs,
    project: &queries::project::ProjectProject,
    environment_id: String,
) -> Result<EnvironmentConfig> {
    let is_terminal = std::io::stdout().is_terminal();

    // Check for non-interactive --service-config or --service-variable flags
    let all_configs = args.config.get_all_service_configs();
    let has_non_interactive = !all_configs.is_empty();

    if has_non_interactive {
        // Non-interactive: parse --service-config and --service-variable flags
        return parse_non_interactive_configs(&all_configs, project, &environment_id);
    }

    if !is_terminal {
        // Not a terminal and no flags provided - return empty config
        return Ok(EnvironmentConfig::default());
    }

    // Interactive flow
    parse_interactive_configs(client, configs, project, &environment_id).await
}

/// Parse --service-config flags into EnvironmentConfig
pub fn parse_non_interactive_configs(
    service_configs: &[String],
    project: &queries::project::ProjectProject,
    environment_id: &str,
) -> Result<EnvironmentConfig> {
    let services = get_environment_services(project, environment_id)?;
    let mut entries: Vec<PatchEntry> = Vec::new();
    let mut configured_fields: std::collections::HashSet<String> = std::collections::HashSet::new();

    // service_configs is a flat Vec<String> with 3 values per entry
    for config_entry in service_configs.chunks(3) {
        if config_entry.len() != 3 {
            bail!(
                "Invalid --service-config format, expected 3 values, got {}",
                config_entry.len()
            );
        }

        let service_input = &config_entry[0];
        let path = &config_entry[1];
        let value = &config_entry[2];

        // Resolve service name/id to service_id
        let Some(service) = services.iter().find(|s| {
            s.node.service_id.to_lowercase() == service_input.to_lowercase()
                || s.node.service_name.to_lowercase() == service_input.to_lowercase()
        }) else {
            bail!("Service '{}' not found", service_input);
        };

        let service_id = &service.node.service_id;

        // Validate path and parse value according to schema-defined type
        // Returns normalized path (fixes leading/trailing/double dots)
        let (normalized_path, json_value) = config::parse_service_value(path, value)?;

        // Track what's being configured for display
        // For "variables.X.value" -> show "variables"
        // For "deploy.startCommand" -> show "startCommand"
        // For "source.image" -> show "image"
        let display_field = get_config_display_field(&normalized_path);
        configured_fields.insert(display_field);

        // Build full path with service ID
        let full_path = format!("services.{}.{}", service_id, normalized_path);
        entries.push((full_path, json_value));
    }

    // Print what was configured (fake_select style)
    if !entries.is_empty() {
        fake_select(
            "Configuring",
            &configured_fields.into_iter().collect::<Vec<_>>().join(", "),
        );
    }

    config::build_config(entries)
}

/// Get a display-friendly field name from a config path
/// - "variables.X" or "variables.X.value" -> "variables"
/// - "deploy.startCommand" -> "startCommand"
/// - "source.image" -> "image"
fn get_config_display_field(path: &str) -> String {
    let parts: Vec<&str> = path.split('.').collect();
    match parts.first() {
        Some(&"variables") => "variables".to_string(),
        _ => parts.last().unwrap_or(&"config").to_string(),
    }
}

/// Interactive flow for collecting service configurations
pub async fn parse_interactive_configs(
    client: &reqwest::Client,
    configs: &Configs,
    project: &queries::project::ProjectProject,
    environment_id: &str,
) -> Result<EnvironmentConfig> {
    let services = get_environment_services(project, environment_id)?;

    // Fetch existing environment config for placeholders
    let existing_config = fetch_environment_config(client, configs, environment_id, false)
        .await
        .map(|r| r.config)
        .ok();

    // Step 1: Select what to configure
    let selected_changes = prompt_multi_options(
        "What do you want to configure? <enter to skip>",
        Change::iter().collect(),
    )?;

    if selected_changes.is_empty() {
        return Ok(EnvironmentConfig::default());
    }

    let mut all_entries: Vec<PatchEntry> = Vec::new();

    // Step 2: For each change type, select services and collect config
    for change in selected_changes {
        let prompt_services = services
            .iter()
            .map(|s| PromptServiceInstance(&s.node))
            .collect::<Vec<_>>();

        let selected_services = prompt_multi_options(
            &format!("What services do you want to configure? ({})", change),
            prompt_services,
        )?;

        // Step 3: For each service, parse the change interactively
        for service in selected_services {
            let service_id = &service.0.service_id;
            let service_name = &service.0.service_name;

            // Look up existing service config for placeholders
            let existing_service = existing_config
                .as_ref()
                .and_then(|c| c.services.get(service_id));

            let entries = change.parse_interactive(service_id, service_name, existing_service)?;
            all_entries.extend(entries);
        }
    }

    config::build_config(all_entries)
}

/// Get service instances for an environment
pub fn get_environment_services<'a>(
    project: &'a queries::project::ProjectProject,
    environment_id: &str,
) -> Result<&'a Vec<queries::project::ProjectProjectEnvironmentsEdgesNodeServiceInstancesEdges>> {
    let environment = project
        .environments
        .edges
        .iter()
        .find(|env| env.node.id == environment_id)
        .ok_or_else(|| anyhow::anyhow!("Environment not found: {}", environment_id))?;

    Ok(&environment.node.service_instances.edges)
}

fn select_duplicate_id_new(
    args: &Args,
    project: &queries::project::ProjectProject,
    is_terminal: bool,
) -> Result<Option<String>, anyhow::Error> {
    let duplicate_id = if let Some(ref duplicate) = args.duplicate {
        let env = project.environments.edges.iter().find(|env| {
            (env.node.name.to_lowercase() == duplicate.to_lowercase())
                || (env.node.id == *duplicate)
        });
        if let Some(env) = env {
            fake_select("Duplicate from", &env.node.name);
            Some(env.node.id.clone())
        } else {
            bail!(RailwayError::EnvironmentNotFound(duplicate.clone()))
        }
    } else if is_terminal {
        let environments = project
            .environments
            .edges
            .iter()
            .filter(|env| env.node.can_access)
            .map(|env| Environment(&env.node))
            .collect::<Vec<_>>();
        prompt_options_skippable(
            "Duplicate from <esc to create an empty environment>",
            environments,
        )?
        .map(|e| e.0.id.clone())
    } else {
        None
    };
    Ok(duplicate_id)
}

fn select_name_new(args: &Args, is_terminal: bool) -> Result<String, anyhow::Error> {
    let name = if let Some(name) = args.name.clone() {
        fake_select("Environment name", name.as_str());
        name
    } else if is_terminal {
        loop {
            let q = prompt_text("Environment name")?;
            if q.is_empty() {
                eprintln!(
                    "{}: Environment name cannot be empty",
                    "Warn".yellow().bold()
                );
                continue;
            } else {
                break q;
            }
        }
    } else {
        bail!("Environment name must be specified when not running in a terminal");
    };
    Ok(name)
}

// Polls for environment creation completion when using background processing.
// Returns true when the environment patch status reaches "STAGED" state.
async fn wait_for_environment_creation(
    client: &reqwest::Client,
    configs: &Configs,
    environment_id: String,
) -> Result<bool> {
    let env_id = environment_id;
    let check_status = || async {
        let vars = queries::environment_staged_changes::Variables {
            environment_id: env_id.clone(),
        };

        let response = post_graphql::<queries::EnvironmentStagedChanges, _>(
            client,
            configs.get_backboard(),
            vars,
        )
        .await?;

        let status = &response.environment_staged_changes.status;

        // Check if environment duplication has completed
        use queries::environment_staged_changes::EnvironmentPatchStatus;
        match status {
            EnvironmentPatchStatus::STAGED | EnvironmentPatchStatus::COMMITTED => Ok(true),
            EnvironmentPatchStatus::APPLYING => bail!("Still applying changes"),
            _ => bail!("Unexpected status: {:?}", status),
        }
    };

    let config = RetryConfig {
        max_attempts: 40,        // ~2 minutes with exponential backoff
        initial_delay_ms: 1000,  // Start at 1 second
        max_delay_ms: 5000,      // Cap at 5 seconds
        backoff_multiplier: 1.5, // Exponential backoff
        on_retry: None,
    };

    retry_with_backoff(config, check_status).await
}

/// Apply environment configuration changes via the API
async fn apply_environment_config(
    client: &reqwest::Client,
    configs: &Configs,
    environment_id: &str,
    env_config: EnvironmentConfig,
) -> Result<()> {
    let vars = mutations::environment_patch_commit::Variables {
        environment_id: environment_id.to_string(),
        patch: env_config,
        commit_message: None,
    };

    post_graphql::<mutations::EnvironmentPatchCommit, _>(client, configs.get_backboard(), vars)
        .await?;

    Ok(())
}
