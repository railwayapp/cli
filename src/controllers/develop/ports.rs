use std::path::PathBuf;

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
/// Range: 10000-60000
pub fn generate_port(service_id: &str, internal_port: i64) -> u16 {
    let mut hash: u32 = 5381;
    for b in service_id.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u32);
    }
    hash = hash.wrapping_add(internal_port as u32);
    10000 + (hash % 50000) as u16
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
    let mode_file = get_develop_dir(project_id)
        .join("certs")
        .join("https_mode");
    std::fs::read_to_string(mode_file)
        .map(|m| m.trim() == "port_443")
        .unwrap_or(false)
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
        assert!((10000..60000).contains(&port));
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
}
