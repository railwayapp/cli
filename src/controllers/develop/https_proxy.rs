use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

#[allow(dead_code)]
pub struct HttpsConfig {
    pub project_slug: String,
    pub base_domain: String,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    pub use_port_443: bool,
}

/// Check if port 443 is available for binding.
/// On macOS Mojave+, unprivileged processes can bind to 443 on 0.0.0.0.
pub fn is_port_443_available() -> bool {
    TcpListener::bind("0.0.0.0:443").is_ok()
}

/// Check if this project's railway-proxy container is running with port 443
pub fn is_project_proxy_on_443(project_id: &str) -> bool {
    let output = Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("name={}-railway-proxy", project_id),
            "--format",
            "{{.Ports}}",
        ])
        .output()
        .ok();

    output
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("443"))
        .unwrap_or(false)
}

pub fn check_mkcert_installed() -> bool {
    Command::new("mkcert")
        .arg("-help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn check_docker_compose_installed() -> bool {
    Command::new("docker")
        .args(["compose", "version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn ensure_mkcert_ca() -> Result<()> {
    let output = Command::new("mkcert")
        .arg("-install")
        .output()
        .context("Failed to run mkcert -install")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("mkcert -install failed: {}", stderr);
    }

    Ok(())
}

/// Check if certs already exist for a project with the required type
pub fn certs_exist(output_dir: &Path, use_port_443: bool) -> bool {
    let cert_path = output_dir.join("cert.pem");
    let key_path = output_dir.join("key.pem");
    let mode_file = output_dir.join("https_mode");

    if !cert_path.exists() || !key_path.exists() {
        return false;
    }

    if let Ok(mode) = std::fs::read_to_string(&mode_file) {
        let stored_443 = mode.trim() == "port_443";
        stored_443 == use_port_443
    } else {
        // No mode file = old certs, need regeneration for port 443
        !use_port_443
    }
}

/// Get config for existing certs without regenerating
pub fn get_existing_certs(
    project_slug: &str,
    output_dir: &Path,
    use_port_443: bool,
) -> HttpsConfig {
    let base_domain = format!("{}.railway.localhost", project_slug);
    HttpsConfig {
        project_slug: project_slug.to_string(),
        base_domain,
        cert_path: output_dir.join("cert.pem"),
        key_path: output_dir.join("key.pem"),
        use_port_443,
    }
}

pub fn generate_certs(
    project_slug: &str,
    output_dir: &Path,
    use_port_443: bool,
) -> Result<HttpsConfig> {
    let base_domain = format!("{}.railway.localhost", project_slug);
    let wildcard_domain = format!("*.{}", base_domain);

    let cert_path = output_dir.join("cert.pem");
    let key_path = output_dir.join("key.pem");

    std::fs::create_dir_all(output_dir)?;

    let mut cmd = Command::new("mkcert");
    cmd.arg("-cert-file")
        .arg(&cert_path)
        .arg("-key-file")
        .arg(&key_path);

    // For port 443 mode, generate wildcard cert for all service subdomains
    if use_port_443 {
        cmd.arg(&wildcard_domain);
    }
    cmd.arg(&base_domain);

    let output = cmd.output().context("Failed to run mkcert")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("mkcert failed: {}", stderr);
    }

    // Save the mode so we know what type of certs we have
    let mode_file = output_dir.join("https_mode");
    let mode = if use_port_443 { "port_443" } else { "fallback" };
    std::fs::write(&mode_file, mode)?;

    Ok(HttpsConfig {
        project_slug: project_slug.to_string(),
        base_domain,
        cert_path,
        key_path,
        use_port_443,
    })
}

pub struct ServicePort {
    pub slug: String,
    pub internal_port: i64,
    pub external_port: u16,
    pub is_http: bool,
    pub is_code_service: bool,
}

pub fn generate_caddyfile(services: &[ServicePort], https_config: &HttpsConfig) -> String {
    let mut caddyfile = String::new();

    caddyfile.push_str("{\n");
    caddyfile.push_str("    auto_https off\n");
    caddyfile.push_str("}\n\n");

    for svc in services.iter().filter(|s| s.is_http) {
        // Port 443 mode: SNI routing with subdomains (no port in URL)
        // Fallback mode: per-service ports (current behavior)
        let site_address = if https_config.use_port_443 {
            format!("{}.{}", svc.slug, https_config.base_domain)
        } else {
            format!("{}:{}", https_config.base_domain, svc.external_port)
        };

        caddyfile.push_str(&format!("{} {{\n", site_address));
        caddyfile.push_str("    tls /certs/cert.pem /certs/key.pem\n");

        // Code services run on host network, image services run in Docker network
        let upstream = if svc.is_code_service {
            format!("host.docker.internal:{}", svc.internal_port)
        } else {
            format!("{}:{}", svc.slug, svc.internal_port)
        };

        caddyfile.push_str(&format!("    reverse_proxy {}\n", upstream));
        caddyfile.push_str("}\n\n");
    }

    caddyfile
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_caddyfile_port_443_mode() {
        let services = vec![ServicePort {
            slug: "api".to_string(),
            internal_port: 3000,
            external_port: 12345,
            is_http: true,
            is_code_service: false,
        }];
        let config = HttpsConfig {
            project_slug: "myproj".to_string(),
            base_domain: "myproj.railway.localhost".to_string(),
            cert_path: PathBuf::from("/certs/cert.pem"),
            key_path: PathBuf::from("/certs/key.pem"),
            use_port_443: true,
        };
        let result = generate_caddyfile(&services, &config);
        assert!(result.contains("api.myproj.railway.localhost {"));
        assert!(result.contains("reverse_proxy api:3000"));
    }

    #[test]
    fn test_generate_caddyfile_fallback_mode() {
        let services = vec![ServicePort {
            slug: "api".to_string(),
            internal_port: 3000,
            external_port: 12345,
            is_http: true,
            is_code_service: false,
        }];
        let config = HttpsConfig {
            project_slug: "myproj".to_string(),
            base_domain: "myproj.railway.localhost".to_string(),
            cert_path: PathBuf::from("/certs/cert.pem"),
            key_path: PathBuf::from("/certs/key.pem"),
            use_port_443: false,
        };
        let result = generate_caddyfile(&services, &config);
        assert!(result.contains("myproj.railway.localhost:12345 {"));
        assert!(result.contains("reverse_proxy api:3000"));
    }

    #[test]
    fn test_generate_caddyfile_code_service() {
        let services = vec![ServicePort {
            slug: "web".to_string(),
            internal_port: 8080,
            external_port: 54321,
            is_http: true,
            is_code_service: true,
        }];
        let config = HttpsConfig {
            project_slug: "myproj".to_string(),
            base_domain: "myproj.railway.localhost".to_string(),
            cert_path: PathBuf::from("/certs/cert.pem"),
            key_path: PathBuf::from("/certs/key.pem"),
            use_port_443: false,
        };
        let result = generate_caddyfile(&services, &config);
        assert!(result.contains("reverse_proxy host.docker.internal:8080"));
    }

    #[test]
    fn test_generate_caddyfile_filters_non_http() {
        let services = vec![
            ServicePort {
                slug: "api".to_string(),
                internal_port: 3000,
                external_port: 12345,
                is_http: true,
                is_code_service: false,
            },
            ServicePort {
                slug: "redis".to_string(),
                internal_port: 6379,
                external_port: 54321,
                is_http: false,
                is_code_service: false,
            },
        ];
        let config = HttpsConfig {
            project_slug: "myproj".to_string(),
            base_domain: "myproj.railway.localhost".to_string(),
            cert_path: PathBuf::from("/certs/cert.pem"),
            key_path: PathBuf::from("/certs/key.pem"),
            use_port_443: false,
        };
        let result = generate_caddyfile(&services, &config);
        assert!(result.contains("api"));
        assert!(!result.contains("redis"));
    }

    #[test]
    fn test_certs_exist_returns_false_when_missing() {
        let temp = TempDir::new().unwrap();
        assert!(!certs_exist(temp.path(), false));
    }

    #[test]
    fn test_certs_exist_returns_true_when_present() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("cert.pem"), "cert").unwrap();
        std::fs::write(temp.path().join("key.pem"), "key").unwrap();
        assert!(certs_exist(temp.path(), false));
    }
}
