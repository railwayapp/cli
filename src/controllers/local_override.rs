use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use anyhow::{Context, Result};

use crate::{
    client::post_graphql,
    config::Configs,
    controllers::environment_config::{EnvironmentConfig, ServiceInstance},
    gql::queries::{self, project::ProjectProject},
};

/// Mode for variable overrides - affects how domains/ports are transformed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideMode {
    /// For docker-compose services - use service slugs for inter-container communication
    DockerNetwork,
    /// For host commands - use localhost with external ports
    HostNetwork,
}

/// Context for applying local variable overrides
pub struct LocalOverrideContext {
    /// service_id -> service slug
    pub service_slugs: HashMap<String, String>,
    /// service_id -> (internal_port -> external_port)
    pub port_mappings: HashMap<String, HashMap<i64, u16>>,
    /// slug -> (internal_port -> external_port) for value substitution
    pub slug_port_mappings: HashMap<String, HashMap<i64, u16>>,
}

/// Returns the path to the docker-compose.yml for a given environment
pub fn get_compose_path(environment_id: &str) -> PathBuf {
    dirs::home_dir()
        .expect("Unable to get home directory")
        .join(".railway")
        .join("develop")
        .join(environment_id)
        .join("docker-compose.yml")
}

/// Check if local develop mode is active (compose file exists)
pub fn is_local_develop_active(environment_id: &str) -> bool {
    get_compose_path(environment_id).exists()
}

/// Build context from environment config (fetches from API)
pub async fn build_local_override_context(
    client: &reqwest::Client,
    configs: &Configs,
    project: &ProjectProject,
    environment_id: &str,
) -> Result<LocalOverrideContext> {
    let vars = queries::get_environment_config::Variables {
        id: environment_id.to_string(),
        decrypt_variables: Some(false),
    };

    let data =
        post_graphql::<queries::GetEnvironmentConfig, _>(client, configs.get_backboard(), vars)
            .await?;

    let config: EnvironmentConfig = serde_json::from_value(data.environment.config)
        .context("Failed to parse environment config")?;

    // Build service name -> slug mapping from project data
    let service_names: HashMap<String, String> = project
        .services
        .edges
        .iter()
        .map(|e| (e.node.id.clone(), e.node.name.clone()))
        .collect();

    let service_slugs: HashMap<String, String> = service_names
        .iter()
        .map(|(id, name)| (id.clone(), slugify(name)))
        .collect();

    // Build port mappings for image-based services
    let mut port_mappings = HashMap::new();
    let mut slug_port_mappings = HashMap::new();

    for (service_id, svc) in config.services.iter() {
        if svc.is_image_based() {
            let mapping = build_port_mapping(service_id, svc);
            if let Some(slug) = service_slugs.get(service_id) {
                slug_port_mappings.insert(slug.clone(), mapping.clone());
            }
            port_mappings.insert(service_id.clone(), mapping);
        }
    }

    Ok(LocalOverrideContext {
        service_slugs,
        port_mappings,
        slug_port_mappings,
    })
}

fn build_port_mapping(service_id: &str, svc: &ServiceInstance) -> HashMap<i64, u16> {
    let mut mapping = HashMap::new();
    if let Some(networking) = &svc.networking {
        for config in networking.service_domains.values().flatten() {
            if let Some(port) = config.port {
                mapping
                    .entry(port)
                    .or_insert_with(|| generate_port(service_id, port));
            }
        }
        for port_str in networking.tcp_proxies.keys() {
            if let Ok(port) = port_str.parse::<i64>() {
                mapping
                    .entry(port)
                    .or_insert_with(|| generate_port(service_id, port));
            }
        }
    }
    mapping
}

/// Apply local overrides to variables for the run command (host network mode)
pub fn apply_local_overrides(
    vars: BTreeMap<String, String>,
    service_id: &str,
    ctx: &LocalOverrideContext,
) -> BTreeMap<String, String> {
    let service_slug = ctx
        .service_slugs
        .get(service_id)
        .cloned()
        .unwrap_or_default();
    let port_mapping = ctx
        .port_mappings
        .get(service_id)
        .cloned()
        .unwrap_or_default();

    override_railway_vars(
        vars,
        &service_slug,
        &port_mapping,
        &ctx.service_slugs,
        &ctx.slug_port_mappings,
        OverrideMode::HostNetwork,
    )
}

// --- Shared functions (used by both develop and run) ---

pub fn slugify(name: &str) -> String {
    let s: String = name
        .chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c == ' ' || c == '-' || c == '_' {
                Some('-')
            } else {
                None
            }
        })
        .collect();
    s.trim_matches('-').to_string()
}

pub fn generate_port(service_id: &str, internal_port: i64) -> u16 {
    let mut hash: u32 = 5381;
    for b in service_id.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u32);
    }
    hash = hash.wrapping_add(internal_port as u32);
    // range 10000-60000
    10000 + (hash % 50000) as u16
}

pub fn is_deprecated_railway_var(key: &str) -> bool {
    if key == "RAILWAY_STATIC_URL" {
        return true;
    }
    // RAILWAY_SERVICE_{name}_URL is deprecated, but RAILWAY_SERVICE_ID and RAILWAY_SERVICE_NAME are not
    if key.starts_with("RAILWAY_SERVICE_") && key.ends_with("_URL") {
        return true;
    }
    false
}

pub fn override_railway_vars(
    vars: BTreeMap<String, String>,
    service_slug: &str,
    port_mapping: &HashMap<i64, u16>,
    service_slugs: &HashMap<String, String>,
    slug_port_mappings: &HashMap<String, HashMap<i64, u16>>,
    mode: OverrideMode,
) -> BTreeMap<String, String> {
    vars.into_iter()
        .filter(|(key, _)| !is_deprecated_railway_var(key))
        .map(|(key, value)| {
            let new_value = match key.as_str() {
                "RAILWAY_PRIVATE_DOMAIN" => match mode {
                    OverrideMode::DockerNetwork => service_slug.to_string(),
                    OverrideMode::HostNetwork => "localhost".to_string(),
                },
                "RAILWAY_PUBLIC_DOMAIN" | "RAILWAY_TCP_PROXY_DOMAIN" => "localhost".to_string(),
                "RAILWAY_TCP_PROXY_PORT" => port_mapping
                    .values()
                    .next()
                    .map(|p| p.to_string())
                    .unwrap_or(value),
                _ => replace_domain_refs(&value, service_slugs, slug_port_mappings, mode),
            };
            (key, new_value)
        })
        .collect()
}

fn replace_domain_refs(
    value: &str,
    service_slugs: &HashMap<String, String>,
    slug_port_mappings: &HashMap<String, HashMap<i64, u16>>,
    mode: OverrideMode,
) -> String {
    let mut result = value.to_string();

    for slug in service_slugs.values() {
        let port_mapping = slug_port_mappings.get(slug);

        // Replace {slug}.railway.internal:{port} patterns
        let railway_domain = format!("{}.railway.internal", slug);
        if result.contains(&railway_domain) {
            match mode {
                OverrideMode::DockerNetwork => {
                    // For docker network, just use the slug (containers resolve by name)
                    result = result.replace(&railway_domain, slug);
                }
                OverrideMode::HostNetwork => {
                    // For host network, replace with localhost and map ports
                    if let Some(ports) = port_mapping {
                        result = replace_domain_with_port_mapping(&result, &railway_domain, ports);
                    } else {
                        result = result.replace(&railway_domain, "localhost");
                    }
                }
            }
        }

        // For host network mode, also replace bare {slug}:{port} patterns
        // Only replace exact patterns to avoid replacing protocol schemes like redis://
        if mode == OverrideMode::HostNetwork {
            if let Some(ports) = port_mapping {
                for (internal, external) in ports {
                    let old_pattern = format!("{}:{}", slug, internal);
                    let new_pattern = format!("localhost:{}", external);
                    result = result.replace(&old_pattern, &new_pattern);
                }
            }
        }
    }

    result
}

/// Replace domain:port patterns with localhost:external_port
/// Also replaces bare domain references (for .railway.internal domains)
fn replace_domain_with_port_mapping(
    value: &str,
    domain: &str,
    port_mapping: &HashMap<i64, u16>,
) -> String {
    let mut result = value.to_string();

    for (internal, external) in port_mapping {
        let old_pattern = format!("{}:{}", domain, internal);
        let new_pattern = format!("localhost:{}", external);
        result = result.replace(&old_pattern, &new_pattern);
    }

    // Replace any remaining bare domain references with localhost
    // This is safe for .railway.internal domains but should not be used for bare slugs
    result = result.replace(domain, "localhost");

    result
}
