use std::collections::{BTreeMap, HashMap};

/// Mode for variable overrides - affects how domains/ports are transformed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideMode {
    /// For docker-compose services - use service slugs for inter-container communication
    DockerNetwork,
    /// For host commands - use localhost with external ports
    HostNetwork,
}

/// HTTPS configuration for local development
pub struct HttpsOverride<'a> {
    pub domain: &'a str,
    pub port: u16,
    /// Service slug for subdomain-based routing (port 443 mode)
    pub slug: Option<String>,
    /// Whether using port 443 mode (prettier URLs without port numbers)
    pub use_port_443: bool,
}

/// Check if a Railway variable is deprecated and should be filtered out
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

/// Maps production public domain -> local public domain for cross-service references
pub type PublicDomainMapping = HashMap<String, String>;

/// Transform Railway variables for local development
#[allow(clippy::too_many_arguments)]
pub fn override_railway_vars(
    vars: BTreeMap<String, String>,
    service_slug: &str,
    port_mapping: &HashMap<i64, u16>,
    service_slugs: &HashMap<String, String>,
    slug_port_mappings: &HashMap<String, HashMap<i64, u16>>,
    public_domain_mapping: &PublicDomainMapping,
    mode: OverrideMode,
    https: Option<HttpsOverride>,
) -> BTreeMap<String, String> {
    vars.into_iter()
        .filter(|(key, _)| !is_deprecated_railway_var(key))
        .map(|(key, value)| {
            let new_value = match key.as_str() {
                "RAILWAY_PRIVATE_DOMAIN" => match mode {
                    OverrideMode::DockerNetwork => service_slug.to_string(),
                    OverrideMode::HostNetwork => "localhost".to_string(),
                },
                "RAILWAY_PUBLIC_DOMAIN" => match &https {
                    Some(h) if h.use_port_443 => {
                        // Port 443 mode: use subdomain (no port in URL)
                        match &h.slug {
                            Some(slug) => format!("{}.{}", slug, h.domain),
                            None => format!("{}.{}", service_slug, h.domain),
                        }
                    }
                    Some(h) => format!("{}:{}", h.domain, h.port),
                    None => "localhost".to_string(),
                },
                "RAILWAY_TCP_PROXY_DOMAIN" => "localhost".to_string(),
                "RAILWAY_TCP_PROXY_PORT" => port_mapping
                    .values()
                    .next()
                    .map(|p| p.to_string())
                    .unwrap_or(value),
                _ => replace_domain_refs(
                    &value,
                    service_slugs,
                    slug_port_mappings,
                    public_domain_mapping,
                    mode,
                ),
            };
            (key, new_value)
        })
        .collect()
}

fn replace_domain_refs(
    value: &str,
    service_slugs: &HashMap<String, String>,
    slug_port_mappings: &HashMap<String, HashMap<i64, u16>>,
    public_domain_mapping: &PublicDomainMapping,
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

    // Replace production public domains with local equivalents
    for (prod_domain, local_domain) in public_domain_mapping {
        result = result.replace(prod_domain, local_domain);
    }

    result
}

/// Replace domain:port patterns with localhost:external_port
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
    result = result.replace(domain, "localhost");

    result
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let result = override_railway_vars(
            vars,
            "my-service",
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            OverrideMode::DockerNetwork,
            None,
        );

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

        let result = override_railway_vars(
            vars,
            "my-service",
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            OverrideMode::HostNetwork,
            None,
        );

        assert_eq!(
            result.get("RAILWAY_PRIVATE_DOMAIN"),
            Some(&"localhost".to_string())
        );
    }

    #[test]
    fn test_override_public_domain_with_https_port_443() {
        let mut vars = BTreeMap::new();
        vars.insert("RAILWAY_PUBLIC_DOMAIN".to_string(), "old.value".to_string());

        let https = HttpsOverride {
            domain: "myproject.localhost",
            port: 443,
            slug: Some("api".to_string()),
            use_port_443: true,
        };

        let result = override_railway_vars(
            vars,
            "api",
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            OverrideMode::HostNetwork,
            Some(https),
        );

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

        let result = override_railway_vars(
            vars,
            "service",
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            OverrideMode::HostNetwork,
            None,
        );

        assert!(!result.contains_key("RAILWAY_STATIC_URL"));
        assert!(!result.contains_key("RAILWAY_SERVICE_API_URL"));
        assert!(result.contains_key("DATABASE_URL"));
    }

    #[test]
    fn test_replace_cross_service_domains() {
        let mut vars = BTreeMap::new();
        // Private domain reference
        vars.insert(
            "REDIS_URL".to_string(),
            "redis://redis.railway.internal:6379".to_string(),
        );
        // Public domain references (railway + custom)
        vars.insert(
            "API_URL".to_string(),
            "https://api-prod.up.railway.app/v1".to_string(),
        );
        vars.insert(
            "CUSTOM_URL".to_string(),
            "https://api.mycompany.io/graphql".to_string(),
        );
        // Multiple domains in one var
        vars.insert(
            "COMBINED".to_string(),
            "api=https://api-prod.up.railway.app,custom=https://api.mycompany.io".to_string(),
        );

        let mut service_slugs = HashMap::new();
        service_slugs.insert("svc-redis".to_string(), "redis".to_string());

        let mut slug_port_mappings = HashMap::new();
        let mut redis_ports = HashMap::new();
        redis_ports.insert(6379i64, 16379u16);
        slug_port_mappings.insert("redis".to_string(), redis_ports);

        let mut public_domain_mapping = HashMap::new();
        public_domain_mapping.insert(
            "api-prod.up.railway.app".to_string(),
            "api.local.railway.localhost".to_string(),
        );
        public_domain_mapping.insert(
            "api.mycompany.io".to_string(),
            "custom.local.railway.localhost".to_string(),
        );

        let result = override_railway_vars(
            vars,
            "my-service",
            &HashMap::new(),
            &service_slugs,
            &slug_port_mappings,
            &public_domain_mapping,
            OverrideMode::HostNetwork,
            None,
        );

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

        let mut service_slugs = HashMap::new();
        service_slugs.insert("svc-redis".to_string(), "redis".to_string());

        let result = override_railway_vars(
            vars,
            "my-service",
            &HashMap::new(),
            &service_slugs,
            &HashMap::new(),
            &HashMap::new(),
            OverrideMode::DockerNetwork,
            None,
        );

        assert_eq!(
            result.get("REDIS_URL"),
            Some(&"redis://redis:6379".to_string())
        );
    }
}
