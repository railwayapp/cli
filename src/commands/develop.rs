use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{client::post_graphql, controllers::project::ensure_project_and_environment_exist};

use super::*;

/// Run image-based Railway services locally with docker compose
#[derive(Debug, Parser)]
pub struct Args {
    /// Environment to use (defaults to linked environment)
    #[clap(short, long)]
    environment: Option<String>,

    /// Output path for docker-compose.yml (defaults to ~/.railway/docker-compose.yml)
    #[clap(short, long)]
    output: Option<PathBuf>,

    /// Only generate docker-compose.yml, don't run docker compose up
    #[clap(long)]
    dry_run: bool,

    /// Run in detached mode (-d)
    #[clap(short, long)]
    detach: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentConfigData {
    #[serde(default)]
    services: BTreeMap<String, ServiceConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceConfig {
    #[serde(default)]
    source: Option<ServiceSource>,
    #[serde(default)]
    deploy: Option<ServiceDeploy>,
    #[serde(default)]
    variables: BTreeMap<String, VariableValue>,
    #[serde(default)]
    networking: Option<ServiceNetworking>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceSource {
    image: Option<String>,
    repo: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceDeploy {
    start_command: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceNetworking {
    #[serde(default)]
    service_domains: Vec<ServiceDomain>,
    #[serde(default)]
    tcp_proxies: Vec<TcpProxy>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum VariableValue {
    Simple(String),
    Complex { default: Option<String> },
}

impl VariableValue {
    fn as_string(&self) -> Option<String> {
        match self {
            VariableValue::Simple(s) => Some(s.clone()),
            VariableValue::Complex { default } => default.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceDomain {
    target_port: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TcpProxy {
    application_port: Option<i64>,
}

#[derive(Debug, Serialize)]
struct DockerComposeFile {
    services: BTreeMap<String, DockerComposeService>,
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
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let environment_id = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());

    println!("Fetching environment config...");

    let vars = queries::get_environment_config::Variables {
        id: environment_id.clone(),
        decrypt_variables: Some(true),
    };

    let data =
        post_graphql::<queries::GetEnvironmentConfig, _>(&client, configs.get_backboard(), vars)
            .await?;

    println!("Data: {:?}", data);

    let env_name = data.environment.name;
    let config_json = data.environment.config;

    let config: EnvironmentConfigData =
        serde_json::from_value(config_json).context("Failed to parse environment config")?;

    println!("Config: {:?}", config);

    let image_services: Vec<_> = config
        .services
        .iter()
        .filter(|(_, svc)| {
            svc.source
                .as_ref()
                .map(|s| s.image.is_some() && s.repo.is_none())
                .unwrap_or(false)
        })
        .collect();

    if image_services.is_empty() {
        println!(
            "No image-based services found in environment '{}'",
            env_name
        );
        return Ok(());
    }

    println!(
        "\n{} image-based service(s) found in '{}':",
        image_services.len(),
        env_name
    );
    for (name, svc) in &image_services {
        println!("  {} {}", "•".green(), name);
        if let Some(source) = &svc.source {
            if let Some(image) = &source.image {
                println!("    image: {}", image.dimmed());
            }
        }
    }

    let mut compose_services = BTreeMap::new();

    for (name, svc) in image_services {
        let image = svc.source.as_ref().unwrap().image.clone().unwrap();

        let environment: BTreeMap<String, String> = svc
            .variables
            .iter()
            .filter_map(|(k, v)| v.as_string().map(|val| (k.clone(), val)))
            .collect();

        let mut ports: Vec<String> = Vec::new();

        if let Some(networking) = &svc.networking {
            for domain in &networking.service_domains {
                if let Some(port) = domain.target_port {
                    let port_str = format!("{}:{}", port, port);
                    if !ports.contains(&port_str) {
                        ports.push(port_str);
                    }
                }
            }

            for proxy in &networking.tcp_proxies {
                if let Some(port) = proxy.application_port {
                    let port_str = format!("{}:{}", port, port);
                    if !ports.contains(&port_str) {
                        ports.push(port_str);
                    }
                }
            }
        }

        let start_command = svc.deploy.as_ref().and_then(|d| d.start_command.clone());

        compose_services.insert(
            name.clone(),
            DockerComposeService {
                image,
                command: start_command,
                environment,
                ports,
            },
        );
    }

    let compose = DockerComposeFile {
        services: compose_services,
    };

    let output_path = args.output.unwrap_or_else(|| {
        let home = dirs::home_dir().expect("Unable to get home directory");
        home.join(".railway").join("docker-compose.yml")
    });

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let yaml = serde_yaml::to_string(&compose)?;

    let tmp_path = output_path.with_extension("yml.tmp");
    std::fs::write(&tmp_path, &yaml)?;
    std::fs::rename(&tmp_path, &output_path)?;

    println!("\n{} {}", "Generated".green(), output_path.display());

    if args.dry_run {
        println!("\n{}", "Dry run mode, not starting containers".yellow());
        println!("\nTo start manually:");
        println!("  docker compose -f {} up", output_path.display());
        return Ok(());
    }

    println!("\n{}", "Starting containers with docker compose...".cyan());

    let mut cmd_args = vec!["compose", "-f", output_path.to_str().unwrap(), "up"];

    if args.detach {
        cmd_args.push("-d");
    }

    let exit_status = tokio::process::Command::new("docker")
        .args(&cmd_args)
        .status()
        .await?;

    if let Some(code) = exit_status.code() {
        if code != 0 {
            bail!("docker compose exited with code {}", code);
        }
    }

    Ok(())
}
