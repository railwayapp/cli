use std::path::Path;

use colored::Colorize;

use super::compose::PortInfo;
use super::https_proxy::HttpsConfig;
use super::ports::slugify;

pub struct ServiceSummary {
    pub name: String,
    pub image: String,
    pub var_count: usize,
    pub ports: Vec<PortInfo>,
    pub volumes: Vec<String>,
}

pub fn print_code_service_summary(
    service_name: &str,
    command: &str,
    working_dir: &Path,
    var_count: usize,
    internal_port: Option<i64>,
    proxy_port: Option<u16>,
    https_config: &Option<HttpsConfig>,
) {
    let slug = slugify(service_name);
    println!("{}", service_name.green().bold());
    println!("  {}: {}", "Command".dimmed(), command);
    println!("  {}: {}", "Directory".dimmed(), working_dir.display());
    println!("  {}: {} variables", "Variables".dimmed(), var_count);
    if let (Some(port), Some(pport)) = (internal_port, proxy_port) {
        println!("  {}:", "Networking".dimmed());
        match https_config {
            Some(config) => {
                println!("    {}: http://localhost:{}", "Private".dimmed(), port);
                if config.use_port_443 {
                    println!(
                        "    {}:  https://{}.{}",
                        "Public".dimmed(),
                        slug,
                        config.base_domain
                    );
                } else {
                    println!(
                        "    {}:  https://{}:{}",
                        "Public".dimmed(),
                        config.base_domain,
                        pport
                    );
                }
            }
            None => {
                println!("    http://localhost:{port}");
            }
        }
    }
    println!();
}
