use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

#[allow(dead_code)]
pub struct HttpsConfig {
    pub project_slug: String,
    pub domain: String,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

pub fn check_mkcert_installed() -> bool {
    Command::new("mkcert")
        .arg("-help")
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

/// Check if certs already exist for a project
pub fn certs_exist(_project_slug: &str, output_dir: &Path) -> bool {
    let cert_path = output_dir.join("cert.pem");
    let key_path = output_dir.join("key.pem");
    cert_path.exists() && key_path.exists()
}

/// Get config for existing certs without regenerating
pub fn get_existing_certs(project_slug: &str, output_dir: &Path) -> HttpsConfig {
    let domain = format!("{}.railway.localhost", project_slug);
    HttpsConfig {
        project_slug: project_slug.to_string(),
        domain,
        cert_path: output_dir.join("cert.pem"),
        key_path: output_dir.join("key.pem"),
    }
}

pub fn generate_certs(project_slug: &str, output_dir: &Path) -> Result<HttpsConfig> {
    let domain = format!("{}.railway.localhost", project_slug);

    let cert_path = output_dir.join("cert.pem");
    let key_path = output_dir.join("key.pem");

    std::fs::create_dir_all(output_dir)?;

    let output = Command::new("mkcert")
        .arg("-cert-file")
        .arg(&cert_path)
        .arg("-key-file")
        .arg(&key_path)
        .arg(&domain)
        .output()
        .context("Failed to run mkcert")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("mkcert failed: {}", stderr);
    }

    Ok(HttpsConfig {
        project_slug: project_slug.to_string(),
        domain,
        cert_path,
        key_path,
    })
}

pub struct ServicePort {
    pub slug: String,
    pub internal_port: i64,
    pub external_port: u16,
    pub is_http: bool,
}

pub fn generate_caddyfile(services: &[ServicePort], https_config: &HttpsConfig) -> String {
    let mut caddyfile = String::new();

    caddyfile.push_str("{\n");
    caddyfile.push_str("    auto_https off\n");
    caddyfile.push_str("}\n\n");

    // Each HTTP service gets its own port block
    for svc in services.iter().filter(|s| s.is_http) {
        caddyfile.push_str(&format!(
            "{}:{} {{\n",
            https_config.domain, svc.external_port
        ));
        caddyfile.push_str("    tls /certs/cert.pem /certs/key.pem\n");
        caddyfile.push_str(&format!(
            "    reverse_proxy {}:{}\n",
            svc.slug, svc.internal_port
        ));
        caddyfile.push_str("}\n\n");
    }

    caddyfile
}
