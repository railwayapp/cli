use std::collections::{BTreeMap, HashMap};

use anyhow::Result;

use crate::{
    config::Configs,
    controllers::{
        config::{ServiceInstance, fetch_environment_config},
        develop::{
            HttpsDomainConfig, LocalDevConfig, LocalDevelopContext, NetworkMode,
            ServiceDomainConfig, build_service_endpoints, generate_port, get_https_domain,
            get_https_mode, override_railway_vars,
        },
    },
    gql::queries::project::ProjectProject,
};

pub use crate::controllers::develop::ports::is_local_develop_active;

/// Build context from environment config (fetches from API)
pub async fn build_local_override_context(
    client: &reqwest::Client,
    configs: &Configs,
    project: &ProjectProject,
    environment_id: &str,
) -> Result<LocalDevelopContext> {
    build_local_override_context_with_config(client, configs, project, environment_id, None).await
}

/// Build context from environment config with optional LocalDevConfig for code services
pub async fn build_local_override_context_with_config(
    client: &reqwest::Client,
    configs: &Configs,
    project: &ProjectProject,
    environment_id: &str,
    local_dev_config: Option<&LocalDevConfig>,
) -> Result<LocalDevelopContext> {
    let env_response = fetch_environment_config(client, configs, environment_id, false).await?;
    let config = env_response.config;

    let service_names: HashMap<String, String> = project
        .services
        .edges
        .iter()
        .map(|e| (e.node.id.clone(), e.node.name.clone()))
        .collect();

    let service_slugs = build_service_endpoints(&service_names, &config);

    let mut ctx = LocalDevelopContext::new(NetworkMode::Host);
    ctx.https_config = get_https_domain(environment_id).map(|domain| HttpsDomainConfig {
        base_domain: domain,
        use_port_443: get_https_mode(environment_id),
    });

    for (service_id, svc) in config.services.iter() {
        let slug = service_slugs.get(service_id).cloned().unwrap_or_default();

        if svc.is_image_based() {
            let port_mapping = build_port_mapping(service_id, svc);
            ctx.services.insert(
                service_id.clone(),
                ServiceDomainConfig {
                    slug,
                    port_mapping,
                    public_domain_prod: None,
                    https_proxy_port: None,
                },
            );
        }
    }

    if let Some(dev_config) = local_dev_config {
        for (service_id, svc) in config.services.iter() {
            if svc.is_code_based() {
                if let Some(code_config) = dev_config.services.get(service_id) {
                    let slug = service_slugs.get(service_id).cloned().unwrap_or_default();

                    let port = code_config
                        .port
                        .map(|p| p as i64)
                        .or_else(|| svc.get_ports().first().copied())
                        .unwrap_or(3000);

                    let external_port = code_config
                        .port
                        .unwrap_or_else(|| generate_port(service_id, port));

                    let mut port_mapping = HashMap::new();
                    for internal in svc.get_ports() {
                        port_mapping.insert(internal, external_port);
                    }
                    port_mapping.insert(port, external_port);

                    let https_proxy_port = Some(generate_port(service_id, port));
                    ctx.services.insert(
                        service_id.clone(),
                        ServiceDomainConfig {
                            slug,
                            port_mapping,
                            public_domain_prod: None,
                            https_proxy_port,
                        },
                    );
                }
            }
        }
    }

    Ok(ctx)
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
    ctx: &LocalDevelopContext,
) -> BTreeMap<String, String> {
    let service = match ctx.for_service(service_id) {
        Some(s) => s,
        None => return vars,
    };

    override_railway_vars(vars, &service, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_context(
        https_domain: Option<&str>,
        use_port_443: bool,
        services: HashMap<String, ServiceDomainConfig>,
    ) -> LocalDevelopContext {
        let mut ctx = LocalDevelopContext::new(NetworkMode::Host);
        ctx.https_config = https_domain.map(|domain| HttpsDomainConfig {
            base_domain: domain.to_string(),
            use_port_443,
        });
        ctx.services = services;
        ctx
    }

    #[test]
    fn test_apply_local_overrides_public_domain_with_port_mapping() {
        let mut vars = BTreeMap::new();
        vars.insert(
            "RAILWAY_PUBLIC_DOMAIN".to_string(),
            "old.railway.app".to_string(),
        );

        let mut services = HashMap::new();
        let mut port_mapping = HashMap::new();
        port_mapping.insert(3000, 12345u16);
        services.insert(
            "svc-1".to_string(),
            ServiceDomainConfig {
                slug: "api".to_string(),
                port_mapping,
                public_domain_prod: None,
                https_proxy_port: None,
            },
        );

        let ctx = make_context(Some("myproject.localhost"), true, services);

        let result = apply_local_overrides(vars, "svc-1", &ctx);
        assert_eq!(
            result.get("RAILWAY_PUBLIC_DOMAIN"),
            Some(&"api.myproject.localhost".to_string())
        );
    }

    #[test]
    fn test_apply_local_overrides_public_domain_without_port_mapping() {
        let mut vars = BTreeMap::new();
        vars.insert(
            "RAILWAY_PUBLIC_DOMAIN".to_string(),
            "old.railway.app".to_string(),
        );

        let mut services = HashMap::new();
        services.insert(
            "svc-1".to_string(),
            ServiceDomainConfig {
                slug: "api".to_string(),
                port_mapping: HashMap::new(),
                public_domain_prod: None,
                https_proxy_port: None,
            },
        );

        let ctx = make_context(Some("myproject.localhost"), true, services);

        let result = apply_local_overrides(vars, "svc-1", &ctx);
        assert_eq!(
            result.get("RAILWAY_PUBLIC_DOMAIN"),
            Some(&"api.myproject.localhost".to_string())
        );
    }

    #[test]
    fn test_apply_local_overrides_public_domain_port_mode() {
        let mut vars = BTreeMap::new();
        vars.insert(
            "RAILWAY_PUBLIC_DOMAIN".to_string(),
            "old.railway.app".to_string(),
        );

        let mut services = HashMap::new();
        let mut port_mapping = HashMap::new();
        port_mapping.insert(3000, 12345u16);
        services.insert(
            "svc-1".to_string(),
            ServiceDomainConfig {
                slug: "api".to_string(),
                port_mapping,
                public_domain_prod: None,
                https_proxy_port: Some(54321),
            },
        );

        let ctx = make_context(Some("myproject.localhost"), false, services);

        let result = apply_local_overrides(vars, "svc-1", &ctx);
        assert_eq!(
            result.get("RAILWAY_PUBLIC_DOMAIN"),
            Some(&"myproject.localhost:54321".to_string())
        );
    }

    #[test]
    fn test_apply_local_overrides_no_https_domain() {
        let mut vars = BTreeMap::new();
        vars.insert(
            "RAILWAY_PUBLIC_DOMAIN".to_string(),
            "old.railway.app".to_string(),
        );

        let mut services = HashMap::new();
        services.insert(
            "svc-1".to_string(),
            ServiceDomainConfig {
                slug: "api".to_string(),
                port_mapping: HashMap::new(),
                public_domain_prod: None,
                https_proxy_port: None,
            },
        );

        let ctx = make_context(None, false, services);

        let result = apply_local_overrides(vars, "svc-1", &ctx);
        assert_eq!(
            result.get("RAILWAY_PUBLIC_DOMAIN"),
            Some(&"localhost".to_string())
        );
    }

    #[test]
    fn test_apply_local_overrides_private_domain() {
        let mut vars = BTreeMap::new();
        vars.insert(
            "RAILWAY_PRIVATE_DOMAIN".to_string(),
            "old.railway.internal".to_string(),
        );

        let mut services = HashMap::new();
        services.insert(
            "svc-1".to_string(),
            ServiceDomainConfig {
                slug: "api".to_string(),
                port_mapping: HashMap::new(),
                public_domain_prod: None,
                https_proxy_port: None,
            },
        );

        let ctx = make_context(None, false, services);

        let result = apply_local_overrides(vars, "svc-1", &ctx);
        assert_eq!(
            result.get("RAILWAY_PRIVATE_DOMAIN"),
            Some(&"localhost".to_string())
        );
    }
}
