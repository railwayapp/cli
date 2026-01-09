use std::collections::HashMap;
use std::path::PathBuf;

use crate::controllers::config::EnvironmentConfig;

/// Port range for deterministic port generation (10000-59999)
pub const PORT_RANGE_MIN: u16 = 10000;
pub const PORT_RANGE_SIZE: u16 = 50000;

/// Port range for random port generation during configuration
pub const RANDOM_PORT_MIN: u16 = 3000;
pub const RANDOM_PORT_MAX: u16 = 9000;

/// Converts a service name to a slug (lowercase, alphanumeric, dashes)
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

/// Generates a deterministic external port from service_id and internal_port
pub fn generate_port(service_id: &str, internal_port: i64) -> u16 {
    let mut hash: u32 = 5381;
    for b in service_id.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u32);
    }
    hash = hash.wrapping_add(internal_port as u32);
    PORT_RANGE_MIN + (hash % PORT_RANGE_SIZE as u32) as u16
}

/// Generates a random port for user configuration prompts
pub fn generate_random_port() -> u16 {
    use rand::Rng;
    rand::thread_rng().gen_range(RANDOM_PORT_MIN..RANDOM_PORT_MAX)
}

/// Resolves a path, canonicalizing on Unix but not on Windows (to avoid UNC path prefix)
pub fn resolve_path(path: std::path::PathBuf) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        path.canonicalize().unwrap_or(path)
    }
    #[cfg(windows)]
    {
        path
    }
}

/// Returns the develop directory for a given project
pub fn get_develop_dir(project_id: &str) -> PathBuf {
    dirs::home_dir()
        .expect("Unable to get home directory")
        .join(".railway")
        .join("develop")
        .join(project_id)
}

/// Returns the path to the docker-compose.yml for a given project
pub fn get_compose_path(project_id: &str) -> PathBuf {
    get_develop_dir(project_id).join("docker-compose.yml")
}

/// Check if local develop mode is active (compose file exists)
pub fn is_local_develop_active(project_id: &str) -> bool {
    get_compose_path(project_id).exists()
}

/// Reads the HTTPS domain from the https_domain file if it exists
pub fn get_https_domain(project_id: &str) -> Option<String> {
    let domain_file = get_develop_dir(project_id).join("https_domain");
    std::fs::read_to_string(domain_file).ok()
}

/// Reads the HTTPS mode from the https_mode file
pub fn get_https_mode(project_id: &str) -> bool {
    let mode_file = get_develop_dir(project_id).join("certs").join("https_mode");
    std::fs::read_to_string(mode_file)
        .map(|m| m.trim() == "port_443")
        .unwrap_or(false)
}

/// Build service_id -> private endpoint mapping.
/// Uses privateNetworkEndpoint from config when available, falls back to slugified name.
pub fn build_service_endpoints(
    service_names: &HashMap<String, String>,
    config: &EnvironmentConfig,
) -> HashMap<String, String> {
    service_names
        .iter()
        .map(|(id, name)| {
            let endpoint = config
                .services
                .get(id)
                .and_then(|svc| svc.networking.as_ref())
                .and_then(|n| n.private_network_endpoint.clone())
                .unwrap_or_else(|| slugify(name));
            (id.clone(), endpoint)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("My Service"), "my-service");
        assert_eq!(slugify("api-server"), "api-server");
        assert_eq!(slugify("API_SERVER"), "api-server");
        assert_eq!(slugify("  Test  "), "test");
        assert_eq!(slugify("hello@world!"), "helloworld");
    }

    #[test]
    fn test_generate_port_deterministic() {
        let port1 = generate_port("service-123", 3000);
        let port2 = generate_port("service-123", 3000);
        assert_eq!(port1, port2);
    }

    #[test]
    fn test_generate_port_in_range() {
        let port = generate_port("test-service", 8080);
        assert!((PORT_RANGE_MIN..(PORT_RANGE_MIN + PORT_RANGE_SIZE)).contains(&port));
    }

    #[test]
    fn test_generate_port_different_services() {
        let port1 = generate_port("service-a", 3000);
        let port2 = generate_port("service-b", 3000);
        assert_ne!(port1, port2);
    }

    #[test]
    fn test_generate_port_different_internal_ports() {
        let port1 = generate_port("service-a", 3000);
        let port2 = generate_port("service-a", 8080);
        assert_ne!(port1, port2);
    }

    #[test]
    fn test_build_service_endpoints() {
        use crate::controllers::config::{EnvironmentConfig, ServiceInstance, ServiceNetworking};
        use std::collections::BTreeMap;

        let mut service_names = HashMap::new();
        service_names.insert("svc-1".to_string(), "My PostgreSQL".to_string());
        service_names.insert("svc-2".to_string(), "Redis Cache".to_string());
        service_names.insert("svc-3".to_string(), "api-server".to_string());

        let mut services = BTreeMap::new();
        // svc-1: has privateNetworkEndpoint set
        services.insert(
            "svc-1".to_string(),
            ServiceInstance {
                networking: Some(ServiceNetworking {
                    private_network_endpoint: Some("postgres".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        // svc-2: no privateNetworkEndpoint, should fall back to slugified name
        services.insert("svc-2".to_string(), ServiceInstance::default());
        // svc-3: has networking but no privateNetworkEndpoint
        services.insert(
            "svc-3".to_string(),
            ServiceInstance {
                networking: Some(ServiceNetworking::default()),
                ..Default::default()
            },
        );

        let config = EnvironmentConfig {
            services,
            ..Default::default()
        };

        let result = build_service_endpoints(&service_names, &config);

        assert_eq!(result.get("svc-1"), Some(&"postgres".to_string()));
        assert_eq!(result.get("svc-2"), Some(&"redis-cache".to_string()));
        assert_eq!(result.get("svc-3"), Some(&"api-server".to_string()));
    }
}
