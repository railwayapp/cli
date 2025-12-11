use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use serde::Serialize;

use crate::{
    client::post_graphql,
    controllers::{
        environment_config::{EnvironmentConfig, ServiceInstance},
        local_https::{
            HttpsConfig, ServicePort, certs_exist, check_mkcert_installed, ensure_mkcert_ca,
            generate_caddyfile, generate_certs, get_existing_certs,
        },
        local_override::{
            HttpsOverride, OverrideMode, generate_port,
            get_compose_path as get_default_compose_path, get_develop_dir, override_railway_vars,
            slugify,
        },
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

    /// Disable HTTPS and pretty URLs (use localhost instead)
    #[clap(long)]
    no_https: bool,
}

#[derive(Debug, Parser)]
struct DownArgs {
    /// Output path for docker-compose.yml (defaults to ~/.railway/develop/<project_id>/docker-compose.yml)
    #[clap(short, long)]
    output: Option<PathBuf>,

    /// Remove volumes and delete compose files
    #[clap(long)]
    clean: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    restart: Option<String>,
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
    Ok(get_default_compose_path(&linked_project.environment))
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

    if args.clean {
        let confirmed = crate::util::prompt::prompt_confirm_with_default(
            "Stop services and remove volume data?",
            false,
        )?;
        if !confirmed {
            return Ok(());
        }
    }

    println!("{}", "Stopping services...".cyan());

    let mut docker_args = vec!["compose", "-f", compose_path.to_str().unwrap(), "down"];
    if args.clean {
        docker_args.push("-v");
    }

    let exit_status = tokio::process::Command::new("docker")
        .args(&docker_args)
        .status()
        .await?;

    if let Some(code) = exit_status.code() {
        if code != 0 {
            bail!("docker compose down exited with code {}", code);
        }
    }

    if args.clean {
        if let Some(parent) = compose_path.parent() {
            std::fs::remove_dir_all(parent)?;
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

    // Set up HTTPS if not disabled
    let https_config = if args.no_https {
        None
    } else {
        setup_https(&project_data.name, &environment_id)?
    };

    // Build slug -> port mappings for all image services (needed for variable substitution)
    let slug_port_mappings: HashMap<String, HashMap<i64, u16>> = image_services
        .iter()
        .filter_map(|(service_id, svc)| {
            let slug = service_slugs.get(*service_id)?;
            let ports = build_slug_port_mapping(service_id, svc);
            Some((slug.clone(), ports))
        })
        .collect();

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

        // Build HTTPS override with first HTTP port for this service
        let https_override = https_config.as_ref().and_then(|config| {
            port_infos
                .iter()
                .find(|p| matches!(p.port_type, PortType::Http))
                .map(|p| HttpsOverride {
                    domain: &config.domain,
                    port: p.external,
                })
        });

        let environment = override_railway_vars(
            raw_vars,
            &slug,
            &port_mapping,
            &service_slugs,
            &slug_port_mappings,
            OverrideMode::DockerNetwork,
            https_override,
        );

        // When HTTPS is enabled, only expose TCP ports directly - HTTP goes through proxy
        let ports: Vec<String> = port_infos
            .iter()
            .filter(|p| {
                // If HTTPS enabled, don't expose HTTP ports (proxy handles them)
                if https_config.is_some() {
                    !matches!(p.port_type, PortType::Http)
                } else {
                    true
                }
            })
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
                restart: Some("on-failure".to_string()),
                environment,
                ports,
                volumes: service_volumes,
                networks: vec!["railway".to_string()],
            },
        );
    }

    let service_count = compose_services.len();

    // Add proxy service if HTTPS is enabled
    if let Some(ref config) = https_config {
        // Collect HTTP service ports for Caddyfile generation
        let service_ports: Vec<ServicePort> = service_summaries
            .iter()
            .flat_map(|s| {
                s.ports.iter().map(|p| ServicePort {
                    slug: slugify(&s.name),
                    internal_port: p.internal,
                    external_port: p.external,
                    is_http: matches!(p.port_type, PortType::Http),
                })
            })
            .collect();

        // Build port mappings for Caddy - only HTTP services go through proxy
        let proxy_ports: Vec<String> = service_ports
            .iter()
            .filter(|p| p.is_http)
            .map(|p| format!("{}:{}", p.external_port, p.external_port))
            .collect();

        if !proxy_ports.is_empty() {
            compose_services.insert(
                "railway-proxy".to_string(),
                DockerComposeService {
                    image: "caddy:2-alpine".to_string(),
                    command: None,
                    restart: Some("on-failure".to_string()),
                    environment: BTreeMap::new(),
                    ports: proxy_ports,
                    volumes: vec![
                        "./Caddyfile:/etc/caddy/Caddyfile:ro".to_string(),
                        "./certs:/certs:ro".to_string(),
                    ],
                    networks: vec!["railway".to_string()],
                },
            );
        }

        // Generate config files
        let develop_dir = get_develop_dir(&environment_id);
        std::fs::create_dir_all(&develop_dir)?;

        // Write Caddyfile
        let caddyfile = generate_caddyfile(&service_ports, config);
        std::fs::write(develop_dir.join("Caddyfile"), caddyfile)?;

        // Save https_domain for railway run to pick up
        std::fs::write(develop_dir.join("https_domain"), &config.domain)?;
    }

    let compose = DockerComposeFile {
        services: compose_services,
        networks: Some(DockerComposeNetworks {
            railway: DockerComposeNetwork {
                driver: "bridge".to_string(),
            },
        }),
        volumes: compose_volumes,
    };

    let output_path = args
        .output
        .unwrap_or_else(|| get_develop_dir(&environment_id).join("docker-compose.yml"));

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

    println!("{}", "Starting services...".cyan());

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
                .map(|p| match (&https_config, &p.port_type) {
                    (Some(config), PortType::Http) => {
                        format!("https://{}:{}", config.domain, p.external)
                    }
                    (None, PortType::Http) => format!("http://localhost:{}", p.external),
                    (_, PortType::Tcp) => format!("localhost:{}", p.external),
                })
                .collect();
            println!("  {}: {}", "Networking".dimmed(), networking.join(", "));
        }
        if !summary.volumes.is_empty() {
            let label = if summary.volumes.len() == 1 {
                "Volume"
            } else {
                "Volumes"
            };
            println!("  {}: {}", label.dimmed(), summary.volumes.join(", "));
        }
        println!();
    }

    Ok(())
}

fn build_port_infos(service_id: &str, svc: &ServiceInstance) -> Vec<PortInfo> {
    let mut port_infos = Vec::new();
    if let Some(networking) = &svc.networking {
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

fn build_slug_port_mapping(service_id: &str, svc: &ServiceInstance) -> HashMap<i64, u16> {
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

fn setup_https(project_name: &str, environment_id: &str) -> Result<Option<HttpsConfig>> {
    use colored::Colorize;

    if !check_mkcert_installed() {
        println!("{}", "mkcert not found, falling back to HTTP mode".yellow());
        println!("Install mkcert for HTTPS support: https://github.com/FiloSottile/mkcert");
        return Ok(None);
    }

    let project_slug = slugify(project_name);
    let certs_dir = get_develop_dir(environment_id).join("certs");

    // Check if certs already exist
    let config = if certs_exist(&project_slug, &certs_dir) {
        get_existing_certs(&project_slug, &certs_dir)
    } else {
        println!("{}", "Setting up local HTTPS...".cyan());

        // Ensure CA is installed
        if let Err(e) = ensure_mkcert_ca() {
            println!("{}: {}", "Warning: Failed to install mkcert CA".yellow(), e);
            println!("Run 'mkcert -install' manually to trust local certificates");
        }

        match generate_certs(&project_slug, &certs_dir) {
            Ok(config) => {
                println!("  {} Generated certs for {}", "✓".green(), config.domain);
                config
            }
            Err(e) => {
                println!(
                    "{}: {}",
                    "Warning: Failed to generate certificates".yellow(),
                    e
                );
                println!("Falling back to HTTP mode");
                return Ok(None);
            }
        }
    };

    Ok(Some(config))
}
