use std::collections::HashMap;

use super::{Config as Args, *};
use crate::controllers::{config::environment::fetch_environment_config, project::get_project};

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;

    // Resolve environment: --environment flag, or linked environment
    let environment_id = resolve_environment(&args, &project, &linked_project, args.json)?;

    // Get environment name for display
    let environment_name = project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment_id)
        .map(|e| e.node.name.clone())
        .unwrap_or_else(|| environment_id.clone());

    // Build service ID -> name map
    let service_names: HashMap<&str, &str> = project
        .services
        .edges
        .iter()
        .map(|s| (s.node.id.as_str(), s.node.name.as_str()))
        .collect();

    let response = fetch_environment_config(&client, &configs, &environment_id, true).await?;
    let config = response.config;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&config)?);
    } else {
        println!(
            "{} {}",
            "Environment:".dimmed(),
            environment_name.magenta().bold()
        );

        // Services
        let active_services: Vec<_> = config
            .services
            .iter()
            .filter(|(_, s)| s.is_deleted != Some(true))
            .collect();

        if !active_services.is_empty() {
            println!("\n{}", "Services".bold());
            for (id, service) in &active_services {
                let name = service_names
                    .get(id.as_str())
                    .copied()
                    .unwrap_or(id.as_str());

                println!("\n{}", name.cyan().bold());

                // Source: image or repo/root directory
                if let Some(ref source) = service.source {
                    if let Some(ref image) = source.image {
                        println!("  {} {}", "image:".dimmed(), image);
                    }
                    if let Some(ref root) = source.root_directory {
                        println!("  {} {}", "root:".dimmed(), root);
                    }
                }

                // Builder
                if let Some(ref build) = service.build {
                    if let Some(ref builder) = build.builder {
                        println!("  {} {}", "builder:".dimmed(), builder.to_lowercase());
                    }
                    if let Some(ref cmd) = build.build_command {
                        println!("  {} {}", "build cmd:".dimmed(), cmd);
                    }
                }

                // Deploy config
                if let Some(ref deploy) = service.deploy {
                    if let Some(ref cmd) = deploy.start_command {
                        println!("  {} {}", "start cmd:".dimmed(), cmd);
                    }
                    if let Some(replicas) = deploy.num_replicas {
                        if replicas != 1 {
                            println!("  {} {}", "replicas:".dimmed(), replicas);
                        }
                    }
                    if let Some(ref regions) = deploy.multi_region_config {
                        let region_list: Vec<_> = regions.keys().collect();
                        if !region_list.is_empty() {
                            println!(
                                "  {} {}",
                                "regions:".dimmed(),
                                region_list
                                    .iter()
                                    .map(|s| s.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                        }
                    }
                }

                // Domains
                if let Some(ref networking) = service.networking {
                    for domain in networking.service_domains.keys() {
                        println!("  {} {}", "domain:".dimmed(), domain);
                    }
                    for domain in networking.custom_domains.keys() {
                        println!("  {} {}", "domain:".dimmed(), domain);
                    }
                }

                // Variables
                if !service.variables.is_empty() {
                    println!("  {} {}", "variables:".dimmed(), service.variables.len());
                }

                // Volume mounts
                for mount in service.volume_mounts.values() {
                    if let Some(ref path) = mount.mount_path {
                        println!("  {} {}", "volume:".dimmed(), path);
                    }
                }
            }
        }

        // Shared variables
        if !config.shared_variables.is_empty() {
            println!(
                "\n{} {}",
                "Shared Variables:".bold(),
                config.shared_variables.len()
            );
        }

        // Volumes
        let active_volumes: Vec<_> = config
            .volumes
            .iter()
            .filter(|(_, v)| v.is_deleted != Some(true))
            .collect();

        if !active_volumes.is_empty() {
            let regions: Vec<_> = active_volumes
                .iter()
                .filter_map(|(_, v)| v.region.as_ref())
                .collect();
            let region_str = if regions.is_empty() {
                String::new()
            } else {
                format!(" ({})", regions.first().unwrap())
            };
            println!(
                "\n{} {}{}",
                "Volumes:".bold(),
                active_volumes.len(),
                region_str.dimmed()
            );
        }

        // Private networking
        if config.private_network_disabled == Some(true) {
            println!("\n{} {}", "Private Network:".bold(), "disabled".dimmed());
        }
    }

    Ok(())
}

/// Resolve the environment ID from --environment flag or linked environment
fn resolve_environment(
    args: &Args,
    project: &queries::project::ProjectProject,
    linked_project: &crate::config::LinkedProject,
    json: bool,
) -> Result<String> {
    if let Some(ref env_input) = args.environment {
        let env = project.environments.edges.iter().find(|e| {
            e.node.name.to_lowercase() == env_input.to_lowercase()
                || e.node.id.to_lowercase() == *env_input.to_lowercase()
        });

        if let Some(env) = env {
            if !json {
                fake_select("Environment", &env.node.name);
            }
            Ok(env.node.id.clone())
        } else {
            bail!(RailwayError::EnvironmentNotFound(env_input.clone()))
        }
    } else {
        let env_id = linked_project.environment.clone();
        let env_name = project
            .environments
            .edges
            .iter()
            .find(|e| e.node.id == env_id)
            .map(|e| e.node.name.clone())
            .unwrap_or_else(|| env_id.clone());
        if !json {
            fake_select("Environment", &env_name);
        }
        Ok(env_id)
    }
}
