use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use serde::Serialize;

use crate::{
    client::post_graphql,
    controllers::{
        environment_config::{EnvironmentConfig, ServiceInstance},
        project::{self, ensure_project_and_environment_exist},
        variables::get_service_variables,
    },
};

use clap::Subcommand;

use super::*;

/// Run image-based Railway services locally with docker compose
#[derive(Debug, Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<DevelopCommand>,
}

#[derive(Debug, Subcommand)]
enum DevelopCommand {
    /// Start containers (default when no subcommand provided)
    Up(UpArgs),
    /// Stop and remove containers
    Down(DownArgs),
}

#[derive(Debug, Parser, Default)]
struct UpArgs {
    /// Environment to use (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Output path for docker-compose.yml (defaults to ~/.railway/develop/<project_id>/docker-compose.yml)
    #[clap(short, long)]
    output: Option<PathBuf>,

    /// Only generate docker-compose.yml, don't run docker compose up
    #[clap(long)]
    dry_run: bool,
}

#[derive(Debug, Parser)]
struct DownArgs {
    /// Output path for docker-compose.yml (defaults to ~/.railway/develop/<project_id>/docker-compose.yml)
    #[clap(short, long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Clone)]
enum PortType {
    Http,
    Tcp,
}

#[derive(Debug, Clone)]
struct PortInfo {
    internal: i64,
    external: u16,
    port_type: PortType,
}

struct ServiceSummary {
    name: String,
    image: String,
    var_count: usize,
    ports: Vec<PortInfo>,
    volumes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DockerComposeFile {
    services: BTreeMap<String, DockerComposeService>,
    #[serde(skip_serializing_if = "Option::is_none")]
    networks: Option<DockerComposeNetworks>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    volumes: BTreeMap<String, DockerComposeVolume>,
}

#[derive(Debug, Serialize)]
struct DockerComposeVolume {}

#[derive(Debug, Serialize)]
struct DockerComposeNetworks {
    railway: DockerComposeNetwork,
}

#[derive(Debug, Serialize)]
struct DockerComposeNetwork {
    driver: String,
}

#[derive(Debug, Serialize)]
struct DockerComposeService {
    image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    environment: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ports: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    volumes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    networks: Vec<String>,
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        Some(DevelopCommand::Up(args)) => up_command(args).await,
        Some(DevelopCommand::Down(args)) => down_command(args).await,
        None => up_command(UpArgs::default()).await,
    }
}

async fn get_compose_path(output: &Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = output {
        return Ok(path.clone());
    }

    let configs = Configs::new()?;
    let linked_project = configs.get_linked_project().await?;

    let home = dirs::home_dir().context("Unable to get home directory")?;
    Ok(home
        .join(".railway")
        .join("develop")
        .join(&linked_project.environment)
        .join("docker-compose.yml"))
}

fn volume_name(environment_id: &str, volume_id: &str) -> String {
    format!("railway_{}_{}", &environment_id[..8], &volume_id[..8])
}

async fn down_command(args: DownArgs) -> Result<()> {
    let compose_path = get_compose_path(&args.output).await?;

    if !compose_path.exists() {
        println!("{}", "Services already stopped".green());
        return Ok(());
    }

    println!("{}", "Stopping services...".cyan());

    let exit_status = tokio::process::Command::new("docker")
        .args(["compose", "-f", compose_path.to_str().unwrap(), "down"])
        .status()
        .await?;

    if let Some(code) = exit_status.code() {
        if code != 0 {
            bail!("docker compose down exited with code {}", code);
        }
    }

    println!("{}", "Services stopped".green());
    Ok(())
}

async fn up_command(args: UpArgs) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project_data =
        project::get_project(&client, &configs, linked_project.project.clone()).await?;

    let service_names: HashMap<String, String> = project_data
        .services
        .edges
        .iter()
        .map(|e| (e.node.id.clone(), e.node.name.clone()))
        .collect();

    let service_slugs: HashMap<String, String> = service_names
        .iter()
        .map(|(id, name)| (id.clone(), slugify(name)))
        .collect();

    let environment_id = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());

    let vars = queries::get_environment_config::Variables {
        id: environment_id.clone(),
        decrypt_variables: Some(true),
    };

    let data =
        post_graphql::<queries::GetEnvironmentConfig, _>(&client, configs.get_backboard(), vars)
            .await?;

    let env_name = data.environment.name;
    let config_json = data.environment.config;

    let config: EnvironmentConfig =
        serde_json::from_value(config_json).context("Failed to parse environment config")?;

    let image_services: Vec<_> = config
        .services
        .iter()
        .filter(|(_, svc)| svc.is_image_based())
        .collect();

    if image_services.is_empty() {
        println!(
            "No image-based services found in environment '{}'",
            env_name
        );
        return Ok(());
    }

    // Fetch resolved variables for each service in parallel
    let variable_futures: Vec<_> = image_services
        .iter()
        .map(|(service_id, _)| {
            get_service_variables(
                &client,
                &configs,
                linked_project.project.clone(),
                environment_id.clone(),
                (*service_id).clone(),
            )
        })
        .collect();

    let variable_results = futures::future::join_all(variable_futures).await;

    let resolved_vars: HashMap<String, BTreeMap<String, String>> = image_services
        .iter()
        .zip(variable_results.into_iter())
        .filter_map(|((service_id, _), result)| {
            result.ok().map(|vars| ((*service_id).clone(), vars))
        })
        .collect();

    let mut compose_services = BTreeMap::new();
    let mut compose_volumes = BTreeMap::new();
    let mut service_summaries = Vec::new();

    for (service_id, svc) in image_services {
        let service_name = service_names
            .get(service_id)
            .cloned()
            .unwrap_or_else(|| service_id.clone());
        let slug = slugify(&service_name);

        let image = svc.source.as_ref().unwrap().image.clone().unwrap();

        // Build port info with types
        let port_infos = build_port_infos(service_id, svc);
        let port_mapping: HashMap<i64, u16> = port_infos
            .iter()
            .map(|p| (p.internal, p.external))
            .collect();

        // Get resolved variables and override Railway-specific vars
        let raw_vars = resolved_vars.get(service_id).cloned().unwrap_or_default();
        let environment = override_railway_vars(raw_vars, &slug, &port_mapping, &service_slugs);

        let ports: Vec<String> = port_infos
            .iter()
            .map(|p| format!("{}:{}", p.external, p.internal))
            .collect();

        // Build volume mounts
        let mut service_volumes = Vec::new();
        for (vol_id, vol_mount) in &svc.volume_mounts {
            if let Some(mount_path) = &vol_mount.mount_path {
                let vol_name = volume_name(&environment_id, vol_id);
                service_volumes.push(format!("{}:{}", vol_name, mount_path));
                compose_volumes.insert(vol_name, DockerComposeVolume {});
            }
        }

        // Escape $ as $$ so docker-compose passes them to the container shell
        let start_command = svc
            .deploy
            .as_ref()
            .and_then(|d| d.start_command.clone())
            .map(|cmd| cmd.replace('$', "$$"));

        let volume_paths: Vec<String> = svc
            .volume_mounts
            .values()
            .filter_map(|v| v.mount_path.clone())
            .collect();

        service_summaries.push(ServiceSummary {
            name: service_name,
            image: image.clone(),
            var_count: environment.len(),
            ports: port_infos,
            volumes: volume_paths,
        });

        compose_services.insert(
            slug,
            DockerComposeService {
                image,
                command: start_command,
                environment,
                ports,
                volumes: service_volumes,
                networks: vec!["railway".to_string()],
            },
        );
    }

    let service_count = compose_services.len();

    let compose = DockerComposeFile {
        services: compose_services,
        networks: Some(DockerComposeNetworks {
            railway: DockerComposeNetwork {
                driver: "bridge".to_string(),
            },
        }),
        volumes: compose_volumes,
    };

    let output_path = args.output.unwrap_or_else(|| {
        let home = dirs::home_dir().expect("Unable to get home directory");
        home.join(".railway")
            .join("develop")
            .join(&environment_id)
            .join("docker-compose.yml")
    });

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let yaml = serde_yaml::to_string(&compose)?;

    let tmp_path = output_path.with_extension("yml.tmp");
    std::fs::write(&tmp_path, &yaml)?;
    std::fs::rename(&tmp_path, &output_path)?;

    if args.dry_run {
        println!("\n{} {}", "Generated".green(), output_path.display());
        println!("\n{}", "Dry run mode, not starting containers".yellow());
        println!("\nTo start manually:");
        println!("  docker compose -f {} up", output_path.display());
        return Ok(());
    }

    println!("\n{}", "Starting services...".cyan());

    let cmd_args = vec!["compose", "-f", output_path.to_str().unwrap(), "up", "-d"];

    let exit_status = tokio::process::Command::new("docker")
        .args(&cmd_args)
        .status()
        .await?;

    if let Some(code) = exit_status.code() {
        if code != 0 {
            bail!("docker compose exited with code {}", code);
        }
    }

    let svc_word = if service_count == 1 {
        "service"
    } else {
        "services"
    };
    println!(
        "\n{}\n",
        format!("Started {} image {} locally", service_count, svc_word).green()
    );

    for summary in &service_summaries {
        println!("{}", summary.name.green().bold());
        println!("  {}: {}", "Image".dimmed(), summary.image);
        println!(
            "  {}: {} variables",
            "Variables".dimmed(),
            summary.var_count
        );
        if !summary.ports.is_empty() {
            let networking: Vec<String> = summary
                .ports
                .iter()
                .map(|p| match p.port_type {
                    PortType::Http => format!("http://localhost:{}", p.external),
                    PortType::Tcp => format!(":{}", p.external),
                })
                .collect();
            println!("  {}: {}", "Networking".dimmed(), networking.join(", "));
        }
        if !summary.volumes.is_empty() {
            let label = if summary.volumes.len() == 1 { "Volume" } else { "Volumes" };
            println!("  {}: {}", label.dimmed(), summary.volumes.join(", "));
        }
        println!();
    }

    Ok(())
}

fn build_port_infos(service_id: &str, svc: &ServiceInstance) -> Vec<PortInfo> {
    let mut port_infos = Vec::new();
    if let Some(networking) = &svc.networking {
        // HTTP ports from service domains
        for config in networking.service_domains.values().flatten() {
            if let Some(port) = config.port {
                if !port_infos.iter().any(|p: &PortInfo| p.internal == port) {
                    port_infos.push(PortInfo {
                        internal: port,
                        external: generate_port(service_id, port),
                        port_type: PortType::Http,
                    });
                }
            }
        }
        // TCP ports from tcp_proxies
        for port_str in networking.tcp_proxies.keys() {
            if let Ok(port) = port_str.parse::<i64>() {
                if !port_infos.iter().any(|p| p.internal == port) {
                    port_infos.push(PortInfo {
                        internal: port,
                        external: generate_port(service_id, port),
                        port_type: PortType::Tcp,
                    });
                }
            }
        }
    }
    port_infos
}

fn slugify(name: &str) -> String {
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

fn generate_port(service_id: &str, internal_port: i64) -> u16 {
    let mut hash: u32 = 5381;
    for b in service_id.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u32);
    }
    hash = hash.wrapping_add(internal_port as u32);
    // range 10000-60000
    10000 + (hash % 50000) as u16
}

fn is_deprecated_railway_var(key: &str) -> bool {
    if key == "RAILWAY_STATIC_URL" {
        return true;
    }
    // RAILWAY_SERVICE_{name}_URL is deprecated, but RAILWAY_SERVICE_ID and RAILWAY_SERVICE_NAME are not
    if key.starts_with("RAILWAY_SERVICE_") && key.ends_with("_URL") {
        return true;
    }
    false
}

fn override_railway_vars(
    vars: BTreeMap<String, String>,
    service_slug: &str,
    port_mapping: &HashMap<i64, u16>,
    service_slugs: &HashMap<String, String>,
) -> BTreeMap<String, String> {
    vars.into_iter()
        .filter(|(key, _)| !is_deprecated_railway_var(key))
        .map(|(key, value)| {
            let new_value = match key.as_str() {
                "RAILWAY_PRIVATE_DOMAIN" => service_slug.to_string(),
                "RAILWAY_PUBLIC_DOMAIN" | "RAILWAY_TCP_PROXY_DOMAIN" => "localhost".to_string(),
                "RAILWAY_TCP_PROXY_PORT" => port_mapping
                    .values()
                    .next()
                    .map(|p| p.to_string())
                    .unwrap_or(value),
                _ => replace_private_domain_refs(&value, service_slugs),
            };
            (key, new_value)
        })
        .collect()
}

fn replace_private_domain_refs(value: &str, service_slugs: &HashMap<String, String>) -> String {
    let mut result = value.to_string();
    for slug in service_slugs.values() {
        let railway_domain = format!("{}.railway.internal", slug);
        if result.contains(&railway_domain) {
            result = result.replace(&railway_domain, slug);
        }
    }
    result
}
