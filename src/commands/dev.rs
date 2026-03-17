use std::{
    collections::{BTreeMap, HashMap},
    io::IsTerminal,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::{
    controllers::{
        config::{ServiceInstance, fetch_environment_config},
        develop::{
            CodeServiceConfig, ComposeServiceStatus, DEFAULT_PORT, DevSession, DockerComposeFile,
            DockerComposeNetwork, DockerComposeNetworks, DockerComposeService, DockerComposeVolume,
            HttpsConfig, HttpsDomainConfig, LocalDevConfig, LocalDevelopContext, NetworkMode,
            PortType, ServiceDomainConfig, ServicePort, ServiceSummary, build_port_infos,
            build_service_endpoints, build_slug_port_mapping, certs_exist,
            check_docker_compose_installed, check_mkcert_installed, ensure_mkcert_ca,
            generate_caddyfile, generate_certs, generate_port, generate_random_port,
            get_compose_path as develop_get_compose_path, get_develop_dir, get_existing_certs,
            is_port_443_available, is_project_proxy_on_443, override_railway_vars,
            print_context_info, print_domain_info, resolve_path, slugify, volume_name,
        },
        project::{self, ensure_project_and_environment_exist},
        variables::get_service_variables,
    },
    util::prompt::{prompt_multi_options, prompt_options, prompt_path_with_default, prompt_text},
};

use clap::Subcommand;

use super::*;

/// Run Railway services locally
#[derive(Debug, Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<DevelopCommand>,

    /// Show verbose domain replacement info (for default 'up' command)
    #[clap(short, long)]
    verbose: bool,
}

#[derive(Debug, Subcommand)]
enum DevelopCommand {
    /// Start services (default when no subcommand provided)
    Up(UpArgs),
    /// Stop services
    Down(DownArgs),
    /// Stop services and remove volumes/data
    Clean(CleanArgs),
    /// Configure local code services
    Configure(ConfigureArgs),
}

#[derive(Debug, Parser)]
struct ConfigureArgs {
    /// Specific service to configure (by name)
    #[clap(long)]
    service: Option<String>,

    /// Remove configuration for a service (optionally specify service name)
    #[clap(long, num_args = 0..=1, default_missing_value = "")]
    remove: Option<String>,
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

    /// Show verbose domain replacement info
    #[clap(short, long)]
    verbose: bool,

    /// Disable TUI, stream logs to stdout instead
    #[clap(long)]
    no_tui: bool,
}

#[derive(Debug, Parser)]
struct DownArgs {
    /// Output path for docker-compose.yml (defaults to ~/.railway/develop/<project_id>/docker-compose.yml)
    #[clap(short, long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct CleanArgs {
    /// Output path for docker-compose.yml (defaults to ~/.railway/develop/<project_id>/docker-compose.yml)
    #[clap(short, long)]
    output: Option<PathBuf>,
}

pub async fn command(args: Args) -> Result<()> {
    eprintln!(
        "{}",
        "Experimental feature. API may change without notice.".yellow()
    );

    match args.command {
        Some(DevelopCommand::Up(up_args)) => up_command(up_args).await,
        Some(DevelopCommand::Down(down_args)) => down_command(down_args).await,
        Some(DevelopCommand::Clean(clean_args)) => clean_command(clean_args).await,
        Some(DevelopCommand::Configure(cfg_args)) => configure_command(cfg_args).await,
        None => {
            up_command(UpArgs {
                verbose: args.verbose,
                ..Default::default()
            })
            .await
        }
    }
}

async fn get_compose_path(output: &Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = output {
        return Ok(path.clone());
    }

    let configs = Configs::new()?;
    let linked_project = configs.get_linked_project().await?;
    Ok(develop_get_compose_path(&linked_project.project))
}

fn docker_install_url() -> &'static str {
    match std::env::consts::OS {
        "macos" => "https://docs.docker.com/desktop/setup/install/mac-install",
        "windows" => "https://docs.docker.com/desktop/setup/install/windows-install",
        _ => "https://docs.docker.com/desktop/setup/install/linux",
    }
}

fn require_docker_compose() {
    if !check_docker_compose_installed() {
        eprintln!();
        eprintln!("{}", "Docker Compose not found.".yellow());
        eprintln!("Install Docker:");
        eprintln!("  {}", docker_install_url());
        std::process::exit(1);
    }
}

async fn down_command(args: DownArgs) -> Result<()> {
    require_docker_compose();

    let compose_path = get_compose_path(&args.output).await?;

    if !compose_path.exists() {
        println!("{}", "Services already stopped".green());
        return Ok(());
    }

    println!("{}", "Stopping services...".cyan());

    let exit_status = tokio::process::Command::new("docker")
        .args(["compose", "-f", &*compose_path.to_string_lossy(), "down"])
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

async fn clean_command(args: CleanArgs) -> Result<()> {
    require_docker_compose();

    let compose_path = get_compose_path(&args.output).await?;

    if !compose_path.exists() {
        println!("{}", "Nothing to clean".green());
        return Ok(());
    }

    let confirmed = crate::util::prompt::prompt_confirm_with_default(
        "Stop services and remove volume data?",
        false,
    )?;
    if !confirmed {
        return Ok(());
    }

    println!("{}", "Cleaning up services...".cyan());

    let exit_status = tokio::process::Command::new("docker")
        .args([
            "compose",
            "-f",
            &*compose_path.to_string_lossy(),
            "down",
            "-v",
        ])
        .status()
        .await?;

    if let Some(code) = exit_status.code() {
        if code != 0 {
            bail!("docker compose down exited with code {}", code);
        }
    }

    if let Some(parent) = compose_path.parent() {
        std::fs::remove_dir_all(parent)?;
    }

    println!("{}", "Services cleaned".green());
    Ok(())
}

struct CodeServiceDisplay {
    service_id: String,
    name: String,
    configured: bool,
}

impl std::fmt::Display for CodeServiceDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.configured {
            write!(f, "{} (configured)", self.name)
        } else {
            write!(f, "{}", self.name)
        }
    }
}

async fn configure_command(args: ConfigureArgs) -> Result<()> {
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

    let project_id = linked_project.project.clone();
    let environment_id = linked_project.environment.clone();

    let env_response = fetch_environment_config(&client, &configs, &environment_id, false).await?;
    let config = env_response.config;

    if config.services.is_empty() {
        println!(
            "{}",
            "No services in this environment. Add services with 'railway add'.".yellow()
        );
        return Ok(());
    }

    let code_services: Vec<_> = config
        .services
        .iter()
        .filter(|(_, svc)| svc.is_code_based())
        .collect();

    if code_services.is_empty() {
        println!(
            "{}",
            "No code-based services found. This environment only has image-based services."
                .yellow()
        );
        return Ok(());
    }

    let mut local_dev_config = LocalDevConfig::load(&project_id)?;

    if let Some(ref remove_arg) = args.remove {
        let service_to_remove = if !remove_arg.is_empty() {
            // --remove <service_name>
            code_services
                .iter()
                .find(|(id, _)| {
                    service_names
                        .get(*id)
                        .map(|n| n == remove_arg)
                        .unwrap_or(false)
                })
                .map(|(id, _)| (*id).clone())
        } else {
            // --remove (no arg) - prompt for selection
            let configured: Vec<_> = code_services
                .iter()
                .filter(|(id, _)| local_dev_config.services.contains_key(*id))
                .map(|(id, _)| CodeServiceDisplay {
                    service_id: (*id).clone(),
                    name: service_names
                        .get(*id)
                        .cloned()
                        .unwrap_or_else(|| (*id).clone()),
                    configured: true,
                })
                .collect();

            if configured.is_empty() {
                println!("{}", "No configured services to remove".yellow());
                return Ok(());
            }

            let selected = prompt_options("Select service to remove configuration:", configured)?;
            Some(selected.service_id)
        };

        if let Some(service_id) = service_to_remove {
            let name = service_names
                .get(&service_id)
                .cloned()
                .unwrap_or_else(|| service_id.clone());
            if local_dev_config.remove_service(&service_id).is_some() {
                local_dev_config.save(&project_id)?;
                println!("{} Removed configuration for '{}'", "✓".green(), name);
            } else {
                println!("{}", format!("Service '{name}' is not configured").yellow());
            }
        }

        return Ok(());
    }

    // Service list loop
    loop {
        let service_id_to_configure = if let Some(ref name) = args.service {
            code_services
                .iter()
                .find(|(id, _)| service_names.get(*id).map(|n| n == name).unwrap_or(false))
                .map(|(id, _)| (*id).clone())
        } else {
            let options: Vec<_> = code_services
                .iter()
                .map(|(id, _)| CodeServiceDisplay {
                    service_id: (*id).clone(),
                    name: service_names
                        .get(*id)
                        .cloned()
                        .unwrap_or_else(|| (*id).clone()),
                    configured: local_dev_config.services.contains_key(*id),
                })
                .collect();

            let selected = prompt_options("Select service to configure:", options)?;
            Some(selected.service_id)
        };

        let Some(service_id) = service_id_to_configure else {
            return Ok(());
        };

        let svc = config
            .services
            .get(&service_id)
            .context("Service not found")?;
        let name = service_names
            .get(&service_id)
            .cloned()
            .unwrap_or_else(|| service_id.clone());

        // If no existing config, do initial setup first
        if local_dev_config.get_service(&service_id).is_none() {
            let mut new_config = prompt_service_config(&name, svc, None)?;

            if let Some(port) = new_config.port {
                let conflicts: Vec<_> = local_dev_config
                    .services
                    .iter()
                    .filter(|(id, cfg)| *id != &service_id && cfg.port == Some(port))
                    .map(|(id, _)| service_names.get(id).cloned().unwrap_or_else(|| id.clone()))
                    .collect();

                if !conflicts.is_empty() {
                    println!(
                        "\n{} Port {} is already used by: {}",
                        "Warning:".yellow().bold(),
                        port,
                        conflicts.join(", ")
                    );
                    let suggested = generate_random_port();
                    let port_input =
                        prompt_text(&format!("Choose a different port [{suggested}]:"))?;
                    new_config.port = Some(if port_input.is_empty() {
                        suggested
                    } else {
                        port_input.parse().context("Invalid port number")?
                    });
                }
            }

            local_dev_config.set_service(service_id.clone(), new_config);
            local_dev_config.save(&project_id)?;
            println!("{} Configured '{}'", "✓".green(), name);
        }

        // Service config menu loop
        loop {
            let action = show_service_config_menu(
                &name,
                local_dev_config.get_service(&service_id).unwrap(),
            )?;

            match action {
                ConfigAction::ChangeCommand => {
                    let existing = local_dev_config.get_service(&service_id).unwrap();
                    let new_command = prompt_text(&format!(
                        "Dev command for '{}' [{}]:",
                        name, existing.command
                    ))
                    .map(|s| {
                        if s.is_empty() {
                            existing.command.clone()
                        } else {
                            s
                        }
                    })?;
                    let mut updated = existing.clone();
                    updated.command = new_command;
                    local_dev_config.set_service(service_id.clone(), updated);
                    local_dev_config.save(&project_id)?;
                    println!("{} Updated command for '{}'", "✓".green(), name);
                }
                ConfigAction::ChangeDirectory => {
                    let existing = local_dev_config.get_service(&service_id).unwrap();
                    let cwd = std::env::current_dir().context("Failed to get current directory")?;
                    let default_dir = PathBuf::from(&existing.directory)
                        .strip_prefix(&cwd)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| existing.directory.clone());

                    let input_path = prompt_path_with_default(
                        &format!("Directory for '{name}' (relative to cwd):"),
                        &default_dir,
                    )?;

                    let directory = if input_path.is_absolute() {
                        input_path.to_string_lossy().to_string()
                    } else {
                        resolve_path(cwd.join(&input_path))
                            .to_string_lossy()
                            .to_string()
                    };

                    let mut updated = existing.clone();
                    updated.directory = directory;
                    local_dev_config.set_service(service_id.clone(), updated);
                    local_dev_config.save(&project_id)?;
                    println!("{} Updated directory for '{}'", "✓".green(), name);
                }
                ConfigAction::ChangePort => {
                    let existing = local_dev_config.get_service(&service_id).unwrap();
                    let railway_port = svc.get_ports().first().map(|&p| p as u16);
                    let current_port = existing.port.or(railway_port).unwrap_or(DEFAULT_PORT);

                    let port_input = prompt_text(&format!("Port for '{name}' [{current_port}]:"))?;

                    let mut new_port = if port_input.is_empty() {
                        current_port
                    } else {
                        port_input.parse().context("Invalid port number")?
                    };

                    let conflicts: Vec<_> = local_dev_config
                        .services
                        .iter()
                        .filter(|(id, cfg)| *id != &service_id && cfg.port == Some(new_port))
                        .map(|(id, _)| service_names.get(id).cloned().unwrap_or_else(|| id.clone()))
                        .collect();

                    if !conflicts.is_empty() {
                        println!(
                            "\n{} Port {} is already used by: {}",
                            "Warning:".yellow().bold(),
                            new_port,
                            conflicts.join(", ")
                        );
                        let suggested = generate_random_port();
                        let port_input =
                            prompt_text(&format!("Choose a different port [{suggested}]:"))?;
                        new_port = if port_input.is_empty() {
                            suggested
                        } else {
                            port_input.parse().context("Invalid port number")?
                        };
                    }

                    let mut updated = existing.clone();
                    updated.port = Some(new_port);
                    local_dev_config.set_service(service_id.clone(), updated);
                    local_dev_config.save(&project_id)?;
                    println!("{} Updated port for '{}'", "✓".green(), name);
                }
                ConfigAction::Remove => {
                    local_dev_config.remove_service(&service_id);
                    local_dev_config.save(&project_id)?;
                    println!("{} Removed configuration for '{}'", "✓".green(), name);
                    break; // Back to service list
                }
                ConfigAction::Back => {
                    break; // Back to service list
                }
            }
        }

        // If --service was specified, exit after handling that service
        if args.service.is_some() {
            return Ok(());
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ConfigAction {
    ChangeCommand,
    ChangeDirectory,
    ChangePort,
    Remove,
    Back,
}

impl std::fmt::Display for ConfigAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigAction::ChangeCommand => write!(f, "Change command"),
            ConfigAction::ChangeDirectory => write!(f, "Change directory"),
            ConfigAction::ChangePort => write!(f, "Change port"),
            ConfigAction::Remove => write!(f, "Remove configuration"),
            ConfigAction::Back => write!(f, "← Configure another service"),
        }
    }
}

fn show_service_config_menu(name: &str, config: &CodeServiceConfig) -> Result<ConfigAction> {
    let display_dir = match std::env::current_dir() {
        Ok(cwd) => PathBuf::from(&config.directory)
            .strip_prefix(&cwd)
            .map(|p| format!("./{}", p.display()))
            .unwrap_or_else(|_| config.directory.clone()),
        Err(_) => config.directory.clone(),
    };

    println!("\n{}", format!("Service '{name}'").cyan().bold());
    println!("  {}: {}", "command".dimmed(), config.command);
    println!("  {}: {}", "directory".dimmed(), display_dir);
    if let Some(port) = config.port {
        println!("  {}: {}", "port".dimmed(), port);
    }
    println!();

    let options = vec![
        ConfigAction::ChangeCommand,
        ConfigAction::ChangeDirectory,
        ConfigAction::ChangePort,
        ConfigAction::Remove,
        ConfigAction::Back,
    ];

    prompt_options("", options)
}

fn prompt_service_config(
    name: &str,
    svc: &ServiceInstance,
    existing: Option<&CodeServiceConfig>,
) -> Result<CodeServiceConfig> {
    println!("\n{}", format!("Configure '{name}'").cyan().bold());

    let default_command = existing.map(|e| e.command.as_str()).unwrap_or("");
    let command = if default_command.is_empty() {
        prompt_text(&format!("Dev command for '{name}':"))?
    } else {
        prompt_text(&format!("Dev command for '{name}' [{default_command}]:")).map(|s| {
            if s.is_empty() {
                default_command.to_string()
            } else {
                s
            }
        })?
    };

    let cwd = std::env::current_dir().context("Failed to get current directory")?;
    let default_dir = existing
        .map(|e| {
            PathBuf::from(&e.directory)
                .strip_prefix(&cwd)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| e.directory.clone())
        })
        .unwrap_or_else(|| ".".to_string());

    let input_path = prompt_path_with_default(
        &format!("Directory for '{name}' (relative to current directory):"),
        &default_dir,
    )?;

    let directory = if input_path.is_absolute() {
        input_path.to_string_lossy().to_string()
    } else {
        resolve_path(cwd.join(&input_path))
            .to_string_lossy()
            .to_string()
    };

    // Prompt for port if service has networking config
    let inferred_port = svc.get_ports().first().map(|&p| p as u16);
    let default_port = existing.and_then(|e| e.port).or(inferred_port);

    let port = if let Some(default) = default_port {
        let port_input = prompt_text(&format!("Port for '{name}' [{default}]:"))?;
        if port_input.is_empty() {
            Some(default)
        } else {
            Some(port_input.parse().context("Invalid port number")?)
        }
    } else {
        None
    };

    Ok(CodeServiceConfig {
        command,
        directory,
        port,
    })
}

/// Prompts user to select and configure multiple services at once
fn prompt_initial_service_setup(
    code_services: &[(&String, &ServiceInstance)],
    service_names: &HashMap<String, String>,
    config: &crate::controllers::config::EnvironmentConfig,
    local_dev_config: &mut LocalDevConfig,
) -> Result<()> {
    println!("\n{}", "Configure local code services".cyan().bold());
    println!("{}", "(Press space to select, enter to confirm)".dimmed());

    let options: Vec<_> = code_services
        .iter()
        .map(|(id, _)| CodeServiceDisplay {
            service_id: (*id).clone(),
            name: service_names
                .get(*id)
                .cloned()
                .unwrap_or_else(|| (*id).clone()),
            configured: false,
        })
        .collect();

    let selected = prompt_multi_options("Select services to configure:", options)?;

    for service_display in &selected {
        let svc = config
            .services
            .get(&service_display.service_id)
            .context("Service not found")?;
        let name = &service_display.name;

        let mut new_config = prompt_service_config(name, svc, None)?;

        // Check for port conflicts with already-configured services
        if let Some(port) = new_config.port {
            let conflicts: Vec<_> = local_dev_config
                .services
                .iter()
                .filter(|(id, cfg)| *id != &service_display.service_id && cfg.port == Some(port))
                .map(|(id, _)| service_names.get(id).cloned().unwrap_or_else(|| id.clone()))
                .collect();

            if !conflicts.is_empty() {
                println!(
                    "\n{} Port {} is already used by: {}",
                    "Warning:".yellow().bold(),
                    port,
                    conflicts.join(", ")
                );
                let suggested = generate_random_port();
                let port_input = prompt_text(&format!("Choose a different port [{suggested}]:"))?;
                new_config.port = Some(if port_input.is_empty() {
                    suggested
                } else {
                    port_input.parse().context("Invalid port number")?
                });
            }
        }

        local_dev_config.set_service(service_display.service_id.clone(), new_config);
    }

    // Show summary if any services were configured
    if !selected.is_empty() {
        println!("\n{}", "Configured services:".green().bold());
        for service_display in &selected {
            if let Some(cfg) = local_dev_config.get_service(&service_display.service_id) {
                let port_str = cfg
                    .port
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "  {} {} (port {})",
                    "•".dimmed(),
                    service_display.name.cyan(),
                    port_str
                );
            }
        }
    }

    Ok(())
}

/// Returns a list of (port, service_names) for ports that have multiple services
fn detect_port_conflicts(
    configs: &HashMap<String, CodeServiceConfig>,
    service_names: &HashMap<String, String>,
) -> Vec<(u16, Vec<String>)> {
    let mut port_to_services: HashMap<u16, Vec<String>> = HashMap::new();

    for (service_id, config) in configs {
        if let Some(port) = config.port {
            let name = service_names
                .get(service_id)
                .cloned()
                .unwrap_or_else(|| service_id.clone());
            port_to_services.entry(port).or_default().push(name);
        }
    }

    port_to_services
        .into_iter()
        .filter(|(_, services)| services.len() > 1)
        .collect()
}

/// Detects and resolves port conflicts. Returns true if conflicts were resolved, false if none.
fn resolve_port_conflicts(
    local_dev_config: &mut LocalDevConfig,
    service_names: &HashMap<String, String>,
    project_id: &str,
) -> Result<bool> {
    let conflicts = detect_port_conflicts(&local_dev_config.services, service_names);
    if conflicts.is_empty() {
        return Ok(false);
    }

    if !std::io::stdout().is_terminal() {
        for (port, services) in &conflicts {
            eprintln!(
                "{} Port {} is used by multiple services: {}",
                "Error:".red().bold(),
                port,
                services.join(", ")
            );
        }
        anyhow::bail!("Port conflicts detected. Run 'railway develop configure' to resolve.");
    }

    println!("\n{} Port conflicts detected:", "Warning:".yellow().bold());
    for (port, services) in &conflicts {
        println!("  Port {}: {}", port, services.join(", "));
    }
    println!();

    // Prompt to resolve each conflict - skip first service, reconfigure the rest
    for (port, conflicting_services) in conflicts {
        for service_name in conflicting_services.iter().skip(1) {
            let service_id = service_names
                .iter()
                .find(|(_, name)| *name == service_name)
                .map(|(id, _)| id.clone());

            if let Some(service_id) = service_id {
                let suggested = generate_random_port();
                let port_input = prompt_text(&format!(
                    "New port for '{service_name}' (currently {port}) [{suggested}]:"
                ))?;

                let new_port = if port_input.is_empty() {
                    suggested
                } else {
                    port_input.parse().context("Invalid port number")?
                };

                if let Some(mut cfg) = local_dev_config.get_service(&service_id).cloned() {
                    cfg.port = Some(new_port);
                    local_dev_config.set_service(service_id, cfg);
                }
            }
        }
    }
    local_dev_config.save(project_id)?;
    println!();
    Ok(true)
}

async fn up_command(args: UpArgs) -> Result<()> {
    require_docker_compose();

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

    let project_id = linked_project.project.clone();
    let environment_id = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());

    let env_response = fetch_environment_config(&client, &configs, &environment_id, true).await?;
    let env_name = env_response.name;
    let config = env_response.config;

    let service_slugs = build_service_endpoints(&service_names, &config);

    let image_services: Vec<_> = config
        .services
        .iter()
        .filter(|(_, svc)| svc.is_image_based())
        .collect();

    let code_services: Vec<_> = config
        .services
        .iter()
        .filter(|(_, svc)| svc.is_code_based())
        .collect();

    let mut local_dev_config = LocalDevConfig::load(&project_id)?;
    let config_file_exists = LocalDevConfig::path(&project_id).exists();

    // Only prompt for first-time setup (no local-dev.json file yet)
    if !config_file_exists && !code_services.is_empty() && std::io::stdout().is_terminal() {
        prompt_initial_service_setup(
            &code_services,
            &service_names,
            &config,
            &mut local_dev_config,
        )?;
        local_dev_config.save(&project_id)?;
        println!();
    }

    let configured_code_services: Vec<_> = code_services
        .iter()
        .filter(|(id, _)| local_dev_config.services.contains_key(*id))
        .collect();

    // Check for and resolve port conflicts among configured code services
    resolve_port_conflicts(&mut local_dev_config, &service_names, &project_id)?;

    if image_services.is_empty() && configured_code_services.is_empty() {
        if config.services.is_empty() {
            println!();
            println!("No services in environment {}", env_name.blue().bold());
            println!("Add services with {}", "railway add".cyan());
        } else {
            println!();
            println!(
                "No services to run in environment {}",
                env_name.blue().bold()
            );
            println!(
                "Use {} to set up code services",
                "railway develop configure".cyan()
            );
        }
        println!();
        return Ok(());
    }

    let https_config = if args.no_https {
        None
    } else {
        setup_https(&project_data.name, &project_id)?
    };

    // Build LocalDevelopContext with all service domain info
    let mut ctx = LocalDevelopContext::new(NetworkMode::Docker);
    ctx.https_config = https_config.as_ref().map(|c| HttpsDomainConfig {
        base_domain: c.base_domain.clone(),
        use_port_443: c.use_port_443,
    });

    // Add image services to context (public_domain_prod populated after fetching vars)
    for (service_id, svc) in &image_services {
        let slug = service_slugs.get(*service_id).cloned().unwrap_or_default();
        let port_mapping = build_slug_port_mapping(service_id, svc);
        ctx.services.insert(
            (*service_id).clone(),
            ServiceDomainConfig {
                slug,
                port_mapping,
                public_domain_prod: None,
                https_proxy_port: None, // port_mapping already has generated ports
            },
        );
    }

    // Add configured code services to context
    for (service_id, svc) in &configured_code_services {
        let slug = service_slugs
            .get(*service_id)
            .cloned()
            .unwrap_or_else(|| slugify(service_id));
        if let Some(dev_config) = local_dev_config.get_service(service_id) {
            let internal_port = dev_config
                .port
                .map(|p| p as i64)
                .or_else(|| svc.get_ports().first().copied())
                .unwrap_or(DEFAULT_PORT as i64);
            let mut port_mapping = HashMap::new();
            for port in svc.get_ports() {
                port_mapping.insert(port, internal_port as u16);
            }
            port_mapping.insert(internal_port, internal_port as u16);
            let https_proxy_port = Some(generate_port(service_id, internal_port));
            ctx.services.insert(
                (*service_id).clone(),
                ServiceDomainConfig {
                    slug,
                    port_mapping,
                    public_domain_prod: None,
                    https_proxy_port,
                },
            );
        }
    }

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

    // Update context with public domain info from resolved variables
    for (service_id, vars) in &resolved_vars {
        if let Some(prod_domain) = vars.get("RAILWAY_PUBLIC_DOMAIN") {
            if let Some(config) = ctx.services.get_mut(service_id) {
                config.public_domain_prod = Some(prod_domain.clone());
            }
        }
    }

    let compose_result = build_image_service_compose(
        &image_services,
        &service_names,
        &resolved_vars,
        &environment_id,
        &ctx,
    );

    let mut compose_services = compose_result.services;
    let compose_volumes = compose_result.volumes;
    let service_summaries = compose_result.summaries;
    let service_count = compose_services.len();

    // Print verbose domain info for image services
    if args.verbose {
        for (service_id, _) in &image_services {
            let name = service_names
                .get(*service_id)
                .cloned()
                .unwrap_or_else(|| (*service_id).clone());
            if let Some(domains) = ctx.for_service(service_id) {
                print_domain_info(&name, &domains);
            }
        }
        print_context_info(&ctx);
    }

    if let Some(ref config) = https_config {
        setup_caddy_proxy(
            &mut compose_services,
            &service_summaries,
            &configured_code_services,
            &local_dev_config,
            &service_slugs,
            config,
            &project_id,
        )?;
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
        .unwrap_or_else(|| get_develop_dir(&project_id).join("docker-compose.yml"));

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let yaml = serde_yaml::to_string(&compose)?;

    let tmp_path = output_path.with_extension("yml.tmp");
    std::fs::write(&tmp_path, &yaml)?;
    std::fs::rename(&tmp_path, &output_path)?;

    if args.dry_run {
        println!("\n{} {}", "Generated".green(), output_path.display());
        println!("\n{}", "Dry run mode, not starting services".yellow());
        println!("\nTo start manually:");
        println!("  docker compose -f {} up", output_path.display());
        return Ok(());
    }

    if !image_services.is_empty() {
        println!("{}", "Starting image services...".cyan());

        let output_path_str = output_path.to_string_lossy();
        let exit_status = tokio::process::Command::new("docker")
            .args(["compose", "-f", &*output_path_str, "up", "-d"])
            .status()
            .await?;

        if let Some(code) = exit_status.code() {
            if code != 0 {
                bail!("docker compose exited with code {}", code);
            }
        }

        // Wait for containers before starting code services that depend on them
        if !configured_code_services.is_empty() {
            println!("\n{}", "Waiting for services to be ready...".dimmed());
            wait_for_services(&output_path, Duration::from_secs(60)).await?;
        }
    }

    if !service_summaries.is_empty() {
        let svc_word = if service_count == 1 {
            "service"
        } else {
            "services"
        };
        println!(
            " {} Started {} image {}",
            "✓".green(),
            service_count,
            svc_word
        );
        println!();

        let use_tui =
            !args.no_tui && std::io::stdout().is_terminal() && !configured_code_services.is_empty();
        if !use_tui {
            for summary in &service_summaries {
                print_image_service_summary(summary, &https_config);
            }
        }
    }

    if configured_code_services.is_empty() {
        print_next_steps(&code_services, &service_names);
        return Ok(());
    }

    // Fetch variables for code services
    let code_var_futures: Vec<_> = configured_code_services
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

    let code_var_results = futures::future::join_all(code_var_futures).await;

    let code_resolved_vars: HashMap<String, BTreeMap<String, String>> = configured_code_services
        .iter()
        .zip(code_var_results.into_iter())
        .filter_map(|((service_id, _), result)| {
            result.ok().map(|vars| ((*service_id).clone(), vars))
        })
        .collect();

    // Update context with code services' public domain info
    for (service_id, vars) in &code_resolved_vars {
        if let Some(prod_domain) = vars.get("RAILWAY_PUBLIC_DOMAIN") {
            if let Some(config) = ctx.services.get_mut(service_id) {
                config.public_domain_prod = Some(prod_domain.clone());
            }
        }
    }

    // Switch to host network mode for code services
    ctx.mode = NetworkMode::Host;

    let use_tui = !args.no_tui && std::io::stdout().is_terminal();

    let mut session = DevSession::start(
        &project_id,
        &configured_code_services,
        &service_names,
        &local_dev_config,
        &code_resolved_vars,
        &ctx,
        &https_config,
        &service_summaries,
        output_path,
        !image_services.is_empty(),
        use_tui,
        args.verbose,
    )
    .await?;

    session.run(use_tui).await?;
    session.shutdown().await;

    Ok(())
}

fn print_next_steps(
    unconfigured_code_services: &[(&String, &ServiceInstance)],
    service_names: &HashMap<String, String>,
) {
    println!("{}", "Next steps".cyan().bold());
    println!();

    println!(
        "  {} Run a command with access to these services:",
        "•".dimmed()
    );
    println!("    {}", "railway run <command>".cyan());
    println!();

    if !unconfigured_code_services.is_empty() {
        println!("  {} Configure code services to run locally:", "•".dimmed());
        println!("    {}", "railway dev configure".cyan());
        println!();
        println!("    {}", "Available:".dimmed());
        for (id, _) in unconfigured_code_services {
            if let Some(name) = service_names.get(*id) {
                println!("      {} {}", "·".dimmed(), name);
            }
        }
        println!();
    }
}

fn print_image_service_summary(summary: &ServiceSummary, https_config: &Option<HttpsConfig>) {
    println!("{}", summary.name.green().bold());
    println!("  {}: {}", "Image".dimmed(), summary.image);
    println!(
        "  {}: {} variables",
        "Variables".dimmed(),
        summary.var_count
    );
    if !summary.ports.is_empty() {
        println!("  {}:", "Networking".dimmed());
        let slug = slugify(&summary.name);
        for p in &summary.ports {
            match (https_config, &p.port_type) {
                (Some(config), PortType::Http) => {
                    println!(
                        "    {}: http://localhost:{}",
                        "Private".dimmed(),
                        p.external
                    );
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
                            p.public_port
                        );
                    }
                }
                (_, PortType::Tcp) => {
                    println!("    {}:     localhost:{}", "TCP".dimmed(), p.external);
                }
                (None, PortType::Http) => {
                    println!("    http://localhost:{}", p.external);
                }
            }
        }
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

struct ImageServiceComposeResult {
    services: BTreeMap<String, DockerComposeService>,
    volumes: BTreeMap<String, DockerComposeVolume>,
    summaries: Vec<ServiceSummary>,
}

fn build_image_service_compose(
    image_services: &[(&String, &ServiceInstance)],
    service_names: &HashMap<String, String>,
    resolved_vars: &HashMap<String, BTreeMap<String, String>>,
    environment_id: &str,
    ctx: &LocalDevelopContext,
) -> ImageServiceComposeResult {
    let mut compose_services = BTreeMap::new();
    let mut compose_volumes = BTreeMap::new();
    let mut service_summaries = Vec::new();

    for (service_id, svc) in image_services {
        let service_name = service_names
            .get(*service_id)
            .cloned()
            .unwrap_or_else(|| (*service_id).clone());
        let slug = slugify(&service_name);

        let image = svc.source.as_ref().unwrap().image.clone().unwrap();

        let port_infos = build_port_infos(service_id, svc);

        let raw_vars = resolved_vars.get(*service_id).cloned().unwrap_or_default();

        let service_domains = ctx
            .for_service(service_id)
            .expect("image services added to ctx before calling this fn");

        let environment = override_railway_vars(raw_vars, Some(&service_domains), ctx);

        let ports: Vec<String> = port_infos
            .iter()
            .map(|p| format!("{}:{}", p.external, p.internal))
            .collect();

        let mut service_volumes = Vec::new();
        for (vol_id, vol_mount) in &svc.volume_mounts {
            if let Some(mount_path) = &vol_mount.mount_path {
                let vol_name = volume_name(environment_id, vol_id);
                service_volumes.push(format!("{vol_name}:{mount_path}"));
                compose_volumes.insert(vol_name, DockerComposeVolume {});
            }
        }

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
                extra_hosts: Vec::new(),
            },
        );
    }

    ImageServiceComposeResult {
        services: compose_services,
        volumes: compose_volumes,
        summaries: service_summaries,
    }
}

fn setup_caddy_proxy(
    compose_services: &mut BTreeMap<String, DockerComposeService>,
    service_summaries: &[ServiceSummary],
    configured_code_services: &[&(&String, &ServiceInstance)],
    local_dev_config: &LocalDevConfig,
    service_slugs: &HashMap<String, String>,
    https_config: &HttpsConfig,
    project_id: &str,
) -> Result<()> {
    let mut service_ports: Vec<ServicePort> = service_summaries
        .iter()
        .flat_map(|s| {
            s.ports.iter().map(|p| ServicePort {
                slug: slugify(&s.name),
                internal_port: p.internal,
                external_port: p.public_port,
                is_http: matches!(p.port_type, PortType::Http),
                is_code_service: false,
            })
        })
        .collect();

    for &(service_id, svc) in configured_code_services {
        if let Some(dev_config) = local_dev_config.get_service(service_id) {
            let slug = service_slugs
                .get(*service_id)
                .cloned()
                .unwrap_or_else(|| slugify(service_id));
            let internal_port = dev_config
                .port
                .map(|p| p as i64)
                .or_else(|| svc.get_ports().first().copied())
                .unwrap_or(DEFAULT_PORT as i64);
            let proxy_port = generate_port(service_id, internal_port);

            service_ports.push(ServicePort {
                slug,
                internal_port,
                external_port: proxy_port,
                is_http: true,
                is_code_service: true,
            });
        }
    }

    let proxy_ports: Vec<String> = if https_config.use_port_443 {
        vec!["443:443".to_string()]
    } else {
        service_ports
            .iter()
            .filter(|p| p.is_http)
            .map(|p| format!("{}:{}", p.external_port, p.external_port))
            .collect()
    };

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
                extra_hosts: vec!["host.docker.internal:host-gateway".to_string()],
            },
        );
    }

    let develop_dir = get_develop_dir(project_id);
    std::fs::create_dir_all(&develop_dir)?;

    let caddyfile = generate_caddyfile(&service_ports, https_config);
    std::fs::write(develop_dir.join("Caddyfile"), caddyfile)?;
    std::fs::write(develop_dir.join("https_domain"), &https_config.base_domain)?;

    Ok(())
}

async fn wait_for_services(compose_path: &Path, timeout: Duration) -> Result<()> {
    let start = Instant::now();

    loop {
        if start.elapsed() > timeout {
            bail!("Timeout waiting for services to be ready");
        }

        let output = tokio::process::Command::new("docker")
            .args([
                "compose",
                "-f",
                &*compose_path.to_string_lossy(),
                "ps",
                "--format",
                "json",
            ])
            .output()
            .await?;

        let services: Vec<ComposeServiceStatus> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();

        for s in &services {
            if s.state == "exited" && s.exit_code != 0 {
                bail!("Service '{}' exited with code {}", s.service, s.exit_code);
            }
        }

        let all_ready = services.iter().all(|s| {
            if !s.health.is_empty() {
                s.health == "healthy"
            } else {
                s.state == "running" || (s.state == "exited" && s.exit_code == 0)
            }
        });

        if all_ready {
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn setup_https(project_name: &str, project_id: &str) -> Result<Option<HttpsConfig>> {
    use colored::Colorize;

    if !check_mkcert_installed() {
        println!("{}", "mkcert not found, falling back to HTTP mode".yellow());
        println!("Install mkcert for HTTPS support: https://github.com/FiloSottile/mkcert");
        return Ok(None);
    }

    // Determine if we can use port 443
    let use_port_443 = if is_port_443_available() {
        true
    } else if is_project_proxy_on_443(project_id) {
        // Our proxy already has 443, we can reuse it
        true
    } else {
        // Something else has 443, fallback to per-service ports
        println!(
            "{}",
            "Port 443 in use by another process, using per-service ports".yellow()
        );
        false
    };

    let project_slug = slugify(project_name);
    let certs_dir = get_develop_dir(project_id).join("certs");

    // Check if certs already exist with the right mode
    let config = if certs_exist(&certs_dir, use_port_443) {
        get_existing_certs(&project_slug, &certs_dir, use_port_443)
    } else {
        println!("{}", "Setting up local HTTPS...".cyan());

        // Ensure CA is installed
        if let Err(e) = ensure_mkcert_ca() {
            println!("{}: {}", "Warning: Failed to install mkcert CA".yellow(), e);
            println!("Run 'mkcert -install' manually to trust local certificates");
        }

        match generate_certs(&project_slug, &certs_dir, use_port_443) {
            Ok(config) => {
                if use_port_443 {
                    println!(
                        "  {} Generated wildcard certs for *.{}",
                        "✓".green(),
                        config.base_domain
                    );
                } else {
                    println!(
                        "  {} Generated certs for {}",
                        "✓".green(),
                        config.base_domain
                    );
                }
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
