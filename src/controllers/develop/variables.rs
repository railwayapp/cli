use std::collections::{BTreeMap, HashMap};

/// Mode for variable overrides - affects how domains/ports are transformed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    /// Docker network: services resolve each other by container name
    Docker,
    /// Host network: everything on localhost with port mapping
    Host,
}

/// HTTPS configuration for local development
#[derive(Debug, Clone)]
pub struct HttpsDomainConfig {
    pub base_domain: String,
    pub use_port_443: bool,
}

/// Per-service domain configuration (input)
#[derive(Debug, Clone, Default)]
pub struct ServiceDomainConfig {
    pub slug: String,
    pub port_mapping: HashMap<i64, u16>,
    pub public_domain_prod: Option<String>,
    pub https_proxy_port: Option<u16>,
}

/// Resolved local domain values for a service (output)
#[derive(Debug, Clone)]
pub struct ServiceLocalDomains {
    pub private_domain: String,
    pub public_domain: Option<String>,
    pub tcp_domain: String,
    pub tcp_port: Option<u16>,
}

/// Environment-wide context for local development
#[derive(Debug, Clone)]
pub struct LocalDevelopContext {
    pub mode: NetworkMode,
    pub https_config: Option<HttpsDomainConfig>,
    pub services: HashMap<String, ServiceDomainConfig>,
}

impl LocalDevelopContext {
    pub fn new(mode: NetworkMode) -> Self {
        Self {
            mode,
            https_config: None,
            services: HashMap::new(),
        }
    }

    pub fn https_enabled(&self) -> bool {
        self.https_config.is_some()
    }

    /// Get resolved local domain values for a service
    pub fn for_service(&self, service_id: &str) -> Option<ServiceLocalDomains> {
        let config = self.services.get(service_id)?;

        let private_domain = match self.mode {
            NetworkMode::Docker => config.slug.clone(),
            NetworkMode::Host => "localhost".to_string(),
        };

        let public_domain = self.resolve_public_domain(config);

        let tcp_port = config.port_mapping.values().next().copied();

        Some(ServiceLocalDomains {
            private_domain,
            public_domain,
            tcp_domain: "localhost".to_string(),
            tcp_port,
        })
    }

    fn resolve_public_domain(&self, config: &ServiceDomainConfig) -> Option<String> {
        let https = self.https_config.as_ref()?;

        Some(if https.use_port_443 {
            format!("{}.{}", config.slug, https.base_domain)
        } else {
            let port = config
                .https_proxy_port
                .or_else(|| config.port_mapping.values().next().copied())
                .unwrap_or(443);
            format!("{}:{}", https.base_domain, port)
        })
    }

    /// Build public domain mapping (prod -> local) for cross-service replacement
    pub fn public_domain_mapping(&self) -> HashMap<String, String> {
        self.services
            .values()
            .filter_map(|config| {
                let prod = config.public_domain_prod.as_ref()?;
                let local = self.resolve_public_domain(config)?;
                Some((prod.clone(), local))
            })
            .collect()
    }

    /// Get all service slugs for cross-service private domain replacement
    pub fn service_slugs(&self) -> Vec<&str> {
        self.services.values().map(|c| c.slug.as_str()).collect()
    }

    /// Get port mapping for a slug
    pub fn port_mapping_for_slug(&self, slug: &str) -> Option<&HashMap<i64, u16>> {
        self.services
            .values()
            .find(|c| c.slug == slug)
            .map(|c| &c.port_mapping)
    }
}

/// Check if a Railway variable is deprecated and should be filtered out
pub fn is_deprecated_railway_var(key: &str) -> bool {
    if key == "RAILWAY_STATIC_URL" {
        return true;
    }
    if key.starts_with("RAILWAY_SERVICE_") && key.ends_with("_URL") {
        return true;
    }
    false
}

pub fn print_domain_info(service_name: &str, domains: &ServiceLocalDomains) {
    use colored::Colorize;
    println!();
    println!("{} {}", "Domain info for".dimmed(), service_name.cyan());
    println!(
        "  {} {}",
        "RAILWAY_PRIVATE_DOMAIN →".dimmed(),
        domains.private_domain.green()
    );
    if let Some(ref public) = domains.public_domain {
        println!(
            "  {} {}",
            "RAILWAY_PUBLIC_DOMAIN  →".dimmed(),
            public.green()
        );
    }
    println!(
        "  {} {}",
        "RAILWAY_TCP_PROXY_DOMAIN →".dimmed(),
        domains.tcp_domain.green()
    );
    if let Some(port) = domains.tcp_port {
        println!(
            "  {} {}",
            "RAILWAY_TCP_PROXY_PORT →".dimmed(),
            port.to_string().green()
        );
    }
}

pub fn print_context_info(ctx: &LocalDevelopContext) {
    use colored::Colorize;
    println!();
    println!("{}", "Cross-service domain mappings:".dimmed());
    let public_mapping = ctx.public_domain_mapping();
    if public_mapping.is_empty() {
        println!("  {}", "(none)".dimmed());
    } else {
        for (prod, local) in &public_mapping {
            println!("  {} {} {}", prod.yellow(), "→".dimmed(), local.green());
        }
    }
}

/// Transform Railway variables for local development
pub fn override_railway_vars(
    vars: BTreeMap<String, String>,
    service: &ServiceLocalDomains,
    ctx: &LocalDevelopContext,
) -> BTreeMap<String, String> {
    let public_domain_mapping = ctx.public_domain_mapping();

    vars.into_iter()
        .filter(|(key, _)| !is_deprecated_railway_var(key))
        .map(|(key, value)| {
            let new_value = match key.as_str() {
                "RAILWAY_PRIVATE_DOMAIN" => service.private_domain.clone(),
                "RAILWAY_PUBLIC_DOMAIN" => service
                    .public_domain
                    .clone()
                    .unwrap_or_else(|| "localhost".to_string()),
                "RAILWAY_TCP_PROXY_DOMAIN" => service.tcp_domain.clone(),
                "RAILWAY_TCP_PROXY_PORT" => {
                    service.tcp_port.map(|p| p.to_string()).unwrap_or(value)
                }
                _ => replace_domain_refs(&value, ctx, &public_domain_mapping),
            };
            (key, new_value)
        })
        .collect()
}

fn replace_domain_refs(
    value: &str,
    ctx: &LocalDevelopContext,
    public_domain_mapping: &HashMap<String, String>,
) -> String {
    let mut result = value.to_string();

    for slug in ctx.service_slugs() {
        let port_mapping = ctx.port_mapping_for_slug(slug);

        // Replace {slug}.railway.internal:{port} patterns
        let railway_domain = format!("{}.railway.internal", slug);
        if result.contains(&railway_domain) {
            match ctx.mode {
                NetworkMode::Docker => {
                    result = result.replace(&railway_domain, slug);
                }
                NetworkMode::Host => {
                    if let Some(ports) = port_mapping {
                        result = replace_domain_with_port_mapping(&result, &railway_domain, ports);
                    } else {
                        result = result.replace(&railway_domain, "localhost");
                    }
                }
            }
        }

        // Host mode: also replace bare {slug}:{port} patterns
        if ctx.mode == NetworkMode::Host {
            if let Some(ports) = port_mapping {
                for (internal, external) in ports {
                    let old_pattern = format!("{}:{}", slug, internal);
                    let new_pattern = format!("localhost:{}", external);
                    result = result.replace(&old_pattern, &new_pattern);
                }
            }
        }
    }

    // Replace public domains (prod -> local)
    for (prod_domain, local_domain) in public_domain_mapping {
        if !ctx.https_enabled() {
            // When HTTPS disabled, also replace https://prod -> http://local
            let https_prod = format!("https://{}", prod_domain);
            let http_local = format!("http://{}", local_domain);
            result = result.replace(&https_prod, &http_local);
        }
        result = result.replace(prod_domain, local_domain);
    }

    result
}

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

    result = result.replace(domain, "localhost");

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_context(mode: NetworkMode) -> LocalDevelopContext {
        LocalDevelopContext::new(mode)
    }

    fn make_service(private_domain: &str) -> ServiceLocalDomains {
        ServiceLocalDomains {
            private_domain: private_domain.to_string(),
            public_domain: None,
            tcp_domain: "localhost".to_string(),
            tcp_port: None,
        }
    }

    #[test]
    fn test_is_deprecated_railway_var() {
        assert!(is_deprecated_railway_var("RAILWAY_STATIC_URL"));
        assert!(is_deprecated_railway_var("RAILWAY_SERVICE_API_URL"));
        assert!(!is_deprecated_railway_var("RAILWAY_SERVICE_ID"));
        assert!(!is_deprecated_railway_var("RAILWAY_SERVICE_NAME"));
        assert!(!is_deprecated_railway_var("DATABASE_URL"));
    }

    #[test]
    fn test_override_private_domain_docker() {
        let mut vars = BTreeMap::new();
        vars.insert(
            "RAILWAY_PRIVATE_DOMAIN".to_string(),
            "old.value".to_string(),
        );

        let ctx = make_context(NetworkMode::Docker);
        let service = make_service("my-service");

        let result = override_railway_vars(vars, &service, &ctx);

        assert_eq!(
            result.get("RAILWAY_PRIVATE_DOMAIN"),
            Some(&"my-service".to_string())
        );
    }

    #[test]
    fn test_override_private_domain_host() {
        let mut vars = BTreeMap::new();
        vars.insert(
            "RAILWAY_PRIVATE_DOMAIN".to_string(),
            "old.value".to_string(),
        );

        let ctx = make_context(NetworkMode::Host);
        let service = make_service("localhost");

        let result = override_railway_vars(vars, &service, &ctx);

        assert_eq!(
            result.get("RAILWAY_PRIVATE_DOMAIN"),
            Some(&"localhost".to_string())
        );
    }

    #[test]
    fn test_override_public_domain_with_https_port_443() {
        let mut vars = BTreeMap::new();
        vars.insert("RAILWAY_PUBLIC_DOMAIN".to_string(), "old.value".to_string());

        let ctx = make_context(NetworkMode::Host);
        let service = ServiceLocalDomains {
            private_domain: "localhost".to_string(),
            public_domain: Some("api.myproject.localhost".to_string()),
            tcp_domain: "localhost".to_string(),
            tcp_port: None,
        };

        let result = override_railway_vars(vars, &service, &ctx);

        assert_eq!(
            result.get("RAILWAY_PUBLIC_DOMAIN"),
            Some(&"api.myproject.localhost".to_string())
        );
    }

    #[test]
    fn test_filter_deprecated_vars() {
        let mut vars = BTreeMap::new();
        vars.insert("RAILWAY_STATIC_URL".to_string(), "value".to_string());
        vars.insert("RAILWAY_SERVICE_API_URL".to_string(), "value".to_string());
        vars.insert("DATABASE_URL".to_string(), "postgres://...".to_string());

        let ctx = make_context(NetworkMode::Host);
        let service = make_service("localhost");

        let result = override_railway_vars(vars, &service, &ctx);

        assert!(!result.contains_key("RAILWAY_STATIC_URL"));
        assert!(!result.contains_key("RAILWAY_SERVICE_API_URL"));
        assert!(result.contains_key("DATABASE_URL"));
    }

    #[test]
    fn test_replace_cross_service_domains() {
        let mut vars = BTreeMap::new();
        vars.insert(
            "REDIS_URL".to_string(),
            "redis://redis.railway.internal:6379".to_string(),
        );
        vars.insert(
            "API_URL".to_string(),
            "https://api-prod.up.railway.app/v1".to_string(),
        );
        vars.insert(
            "CUSTOM_URL".to_string(),
            "https://api.mycompany.io/graphql".to_string(),
        );
        vars.insert(
            "COMBINED".to_string(),
            "api=https://api-prod.up.railway.app,custom=https://api.mycompany.io".to_string(),
        );

        let mut ctx = make_context(NetworkMode::Host);

        let mut redis_ports = HashMap::new();
        redis_ports.insert(6379i64, 16379u16);
        ctx.services.insert(
            "svc-redis".to_string(),
            ServiceDomainConfig {
                slug: "redis".to_string(),
                port_mapping: redis_ports,
                public_domain_prod: None,
                https_proxy_port: None,
            },
        );

        let mut api_ports = HashMap::new();
        api_ports.insert(3000i64, 13000u16);
        ctx.services.insert(
            "svc-api".to_string(),
            ServiceDomainConfig {
                slug: "api".to_string(),
                port_mapping: api_ports,
                public_domain_prod: Some("api-prod.up.railway.app".to_string()),
                https_proxy_port: None,
            },
        );

        ctx.services.insert(
            "svc-custom".to_string(),
            ServiceDomainConfig {
                slug: "custom".to_string(),
                port_mapping: HashMap::new(),
                public_domain_prod: Some("api.mycompany.io".to_string()),
                https_proxy_port: None,
            },
        );

        ctx.https_config = Some(HttpsDomainConfig {
            base_domain: "local.railway.localhost".to_string(),
            use_port_443: true,
        });

        let service = make_service("localhost");
        let result = override_railway_vars(vars, &service, &ctx);

        assert_eq!(
            result.get("REDIS_URL"),
            Some(&"redis://localhost:16379".to_string())
        );
        assert_eq!(
            result.get("API_URL"),
            Some(&"https://api.local.railway.localhost/v1".to_string())
        );
        assert_eq!(
            result.get("CUSTOM_URL"),
            Some(&"https://custom.local.railway.localhost/graphql".to_string())
        );
        assert_eq!(
            result.get("COMBINED"),
            Some(
                &"api=https://api.local.railway.localhost,custom=https://custom.local.railway.localhost"
                    .to_string()
            )
        );
    }

    #[test]
    fn test_private_domain_docker_mode() {
        let mut vars = BTreeMap::new();
        vars.insert(
            "REDIS_URL".to_string(),
            "redis://redis.railway.internal:6379".to_string(),
        );

        let mut ctx = make_context(NetworkMode::Docker);
        ctx.services.insert(
            "svc-redis".to_string(),
            ServiceDomainConfig {
                slug: "redis".to_string(),
                port_mapping: HashMap::new(),
                public_domain_prod: None,
                https_proxy_port: None,
            },
        );

        let service = make_service("my-service");
        let result = override_railway_vars(vars, &service, &ctx);

        assert_eq!(
            result.get("REDIS_URL"),
            Some(&"redis://redis:6379".to_string())
        );
    }

    #[test]
    fn test_no_public_domain_mapping_when_https_disabled() {
        let mut vars = BTreeMap::new();
        vars.insert(
            "API_URL".to_string(),
            "https://api-prod.up.railway.app/v1".to_string(),
        );

        let mut ctx = make_context(NetworkMode::Host);
        // https_config is None, so no public domain mapping happens

        let mut api_ports = HashMap::new();
        api_ports.insert(3000i64, 13000u16);
        ctx.services.insert(
            "svc-api".to_string(),
            ServiceDomainConfig {
                slug: "api".to_string(),
                port_mapping: api_ports,
                public_domain_prod: Some("api-prod.up.railway.app".to_string()),
                https_proxy_port: None,
            },
        );

        let service = make_service("localhost");
        let result = override_railway_vars(vars, &service, &ctx);

        // Without https_config, prod domain is not replaced
        assert_eq!(
            result.get("API_URL"),
            Some(&"https://api-prod.up.railway.app/v1".to_string())
        );
    }

    #[test]
    fn test_for_service_docker_mode() {
        let mut ctx = LocalDevelopContext::new(NetworkMode::Docker);
        ctx.https_config = Some(HttpsDomainConfig {
            base_domain: "myproject.localhost".to_string(),
            use_port_443: true,
        });

        let mut ports = HashMap::new();
        ports.insert(3000i64, 13000u16);
        ctx.services.insert(
            "svc-api".to_string(),
            ServiceDomainConfig {
                slug: "api".to_string(),
                port_mapping: ports,
                public_domain_prod: Some("api.railway.app".to_string()),
                https_proxy_port: None,
            },
        );

        let service = ctx.for_service("svc-api").unwrap();
        assert_eq!(service.private_domain, "api");
        assert_eq!(
            service.public_domain,
            Some("api.myproject.localhost".to_string())
        );
        assert_eq!(service.tcp_domain, "localhost");
        assert_eq!(service.tcp_port, Some(13000));
    }

    #[test]
    fn test_for_service_host_mode() {
        let mut ctx = LocalDevelopContext::new(NetworkMode::Host);
        ctx.https_config = Some(HttpsDomainConfig {
            base_domain: "myproject.localhost".to_string(),
            use_port_443: false,
        });

        let mut ports = HashMap::new();
        ports.insert(3000i64, 13000u16);
        ctx.services.insert(
            "svc-api".to_string(),
            ServiceDomainConfig {
                slug: "api".to_string(),
                port_mapping: ports,
                public_domain_prod: Some("api.railway.app".to_string()),
                https_proxy_port: Some(41191),
            },
        );

        let service = ctx.for_service("svc-api").unwrap();
        assert_eq!(service.private_domain, "localhost");
        assert_eq!(
            service.public_domain,
            Some("myproject.localhost:41191".to_string())
        );
    }
}
