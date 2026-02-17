use strum::IntoEnumIterator;

use super::{New as Args, changes::Change, *};
use crate::{
    controllers::config::{
        self, EnvironmentConfig, PatchEntry,
        environment::{fetch_environment_config, prepare_config_for_duplication},
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

    // Step 1: Create a new empty environment (no sourceEnvironmentId)
    let vars = mutations::environment_create::Variables {
        project_id: project.id.clone(),
        name,
        source_id: None,
        apply_changes_in_background: None,
    };

    let spinner = create_spinner_if(!json, "Creating environment...".into());

    let response =
        post_graphql::<mutations::EnvironmentCreate, _>(&client, &configs.get_backboard(), vars)
            .await?;

    let env_id = response.environment_create.id.clone();
    let env_name = response.environment_create.name.clone();

    // Step 2: If duplicating, fetch source config and merge with overrides
    if let Some(ref source_env_id) = duplicate_id {
        if let Some(ref s) = spinner {
            s.set_message("Fetching source environment config...");
        }

        // Fetch the source environment's full config
        let source_config = fetch_environment_config(&client, &configs, source_env_id, true)
            .await?
            .config;

        // Prepare config for duplication: mark services/volumes/buckets for creation
        let source_config = prepare_config_for_duplication(source_config);

        // Get any --service-config overrides
        let override_config = edit_services_select(
            &args,
            &client,
            &configs,
            &project,
            source_env_id.clone(),
            source_config.clone(),
        )
        .await?;

        // Merge source config with overrides (overrides take precedence)
        let merged_config = merge_configs(source_config, override_config);

        if !config::is_empty(&merged_config) {
            if let Some(ref s) = spinner {
                s.set_message("Applying configuration...");
            }
            apply_environment_config(&client, &configs, &env_id, merged_config).await?;
        }
    }
    // No duplication = empty environment, nothing to configure

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
    exisiting_config: EnvironmentConfig,
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
    parse_interactive_configs(
        client,
        configs,
        project,
        &environment_id,
        Some(exisiting_config),
    )
    .await
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
        let full_path = format!("services.{service_id}.{normalized_path}");
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
/// If `existing_config` is None, fetches the current environment config
pub async fn parse_interactive_configs(
    client: &reqwest::Client,
    configs: &Configs,
    project: &queries::project::ProjectProject,
    environment_id: &str,
    existing_config: Option<EnvironmentConfig>,
) -> Result<EnvironmentConfig> {
    let services = get_environment_services(project, environment_id)?;

    // Use provided config or fetch existing environment config for placeholders
    let existing_config = match existing_config {
        Some(config) => Some(config),
        None => fetch_environment_config(client, configs, environment_id, false)
            .await
            .map(|r| r.config)
            .ok(),
    };

    // Step 1: Select which services to configure
    let prompt_services = services
        .iter()
        .map(|s| PromptServiceInstance(&s.node))
        .collect::<Vec<_>>();

    let selected_services = prompt_multi_options(
        "What services do you want to configure? <enter to skip>",
        prompt_services,
    )?;

    if selected_services.is_empty() {
        return Ok(EnvironmentConfig::default());
    }

    let mut all_entries: Vec<PatchEntry> = Vec::new();

    // Step 2: For each service, select what to configure and collect config
    for service in selected_services {
        let service_id = &service.0.service_id;
        let service_name = &service.0.service_name;

        // Look up existing service config for placeholders
        let existing_service = existing_config
            .as_ref()
            .and_then(|c| c.services.get(service_id));

        let selected_changes = prompt_multi_options(
            &format!("What do you want to configure for {service_name}?"),
            Change::iter().collect(),
        )?;

        // Step 3: For each change type, parse interactively
        for change in selected_changes {
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

/// Merge two EnvironmentConfigs, with override_config taking precedence
fn merge_configs(base: EnvironmentConfig, overrides: EnvironmentConfig) -> EnvironmentConfig {
    // Convert both to JSON, deep merge, then convert back
    let base_json = serde_json::to_value(&base).unwrap_or_default();
    let override_json = serde_json::to_value(&overrides).unwrap_or_default();

    let merged = deep_merge_json(base_json, override_json);

    serde_json::from_value(merged).unwrap_or(base)
}

/// Deep merge two JSON values, with right taking precedence
fn deep_merge_json(left: serde_json::Value, right: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;

    match (left, right) {
        (Value::Object(mut left_map), Value::Object(right_map)) => {
            for (key, right_val) in right_map {
                let merged_val = if let Some(left_val) = left_map.remove(&key) {
                    deep_merge_json(left_val, right_val)
                } else {
                    right_val
                };
                left_map.insert(key, merged_val);
            }
            Value::Object(left_map)
        }
        // For non-objects, right takes precedence
        (_, right) => right,
    }
}
