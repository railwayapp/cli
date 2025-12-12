use std::collections::{BTreeMap, HashMap};

use anyhow::Result;

use crate::{
    config::Configs,
    controllers::{
        config::{ServiceInstance, fetch_environment_config},
        develop::{
            HttpsOverride, LocalDevConfig, OverrideMode, generate_port, get_https_domain,
            get_https_mode, override_railway_vars, slugify,
        },
    },
    gql::queries::project::ProjectProject,
};

pub use crate::controllers::develop::ports::is_local_develop_active;

/// Context for applying local variable overrides
pub struct LocalOverrideContext {
    /// service_id -> service slug
    pub service_slugs: HashMap<String, String>,
    /// service_id -> (internal_port -> external_port)
    pub port_mappings: HashMap<String, HashMap<i64, u16>>,
    /// slug -> (internal_port -> external_port) for value substitution
    pub slug_port_mappings: HashMap<String, HashMap<i64, u16>>,
    /// HTTPS domain for pretty URLs (e.g., "myproject.railway.localhost")
    pub https_domain: Option<String>,
    /// Whether using port 443 mode (prettier URLs without port numbers)
    pub use_port_443: bool,
}

/// Build context from environment config (fetches from API)
pub async fn build_local_override_context(
    client: &reqwest::Client,
    configs: &Configs,
    project: &ProjectProject,
    environment_id: &str,
) -> Result<LocalOverrideContext> {
    build_local_override_context_with_config(client, configs, project, environment_id, None).await
}

/// Build context from environment config with optional LocalDevConfig for code services
pub async fn build_local_override_context_with_config(
    client: &reqwest::Client,
    configs: &Configs,
    project: &ProjectProject,
    environment_id: &str,
    local_dev_config: Option<&LocalDevConfig>,
) -> Result<LocalOverrideContext> {
    let env_response = fetch_environment_config(client, configs, environment_id, false).await?;
    let config = env_response.config;

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

    if let Some(dev_config) = local_dev_config {
        for (service_id, svc) in config.services.iter() {
            if svc.is_code_based() {
                if let Some(code_config) = dev_config.services.get(service_id) {
                    let port = code_config
                        .port
                        .map(|p| p as i64)
                        .or_else(|| svc.get_ports().first().copied())
                        .unwrap_or(3000);

                    let external_port = code_config
                        .port
                        .unwrap_or_else(|| generate_port(service_id, port));

                    let mut mapping = HashMap::new();
                    // For code services, map all internal ports to the configured external port
                    for internal in svc.get_ports() {
                        mapping.insert(internal, external_port);
                    }
                    // Also include the configured port itself
                    mapping.insert(port, external_port);

                    if let Some(slug) = service_slugs.get(service_id) {
                        slug_port_mappings.insert(slug.clone(), mapping.clone());
                    }
                    port_mappings.insert(service_id.clone(), mapping);
                }
            }
        }
    }

    let https_domain = get_https_domain(environment_id);
    let use_port_443 = get_https_mode(environment_id);

    Ok(LocalOverrideContext {
        service_slugs,
        port_mappings,
        slug_port_mappings,
        https_domain,
        use_port_443,
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

    // Get first HTTP port for this service for HTTPS override
    let https = ctx.https_domain.as_ref().and_then(|domain| {
        port_mapping.values().next().map(|&port| HttpsOverride {
            domain,
            port,
            slug: Some(service_slug.clone()),
            use_port_443: ctx.use_port_443,
        })
    });

    override_railway_vars(
        vars,
        &service_slug,
        &port_mapping,
        &ctx.service_slugs,
        &ctx.slug_port_mappings,
        OverrideMode::HostNetwork,
        https,
    )
}
