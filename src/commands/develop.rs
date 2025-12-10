use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use serde::Serialize;

use crate::{
    client::post_graphql,
    controllers::{
        environment_config::EnvironmentConfig,
        project::{self, ensure_project_and_environment_exist},
    },
};

use super::*;

/// Run image-based Railway services locally with docker compose
#[derive(Debug, Parser)]
pub struct Args {
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

    let project_data =
        project::get_project(&client, &configs, linked_project.project.clone()).await?;

    let service_names: HashMap<String, String> = project_data
        .services
        .edges
        .iter()
        .map(|e| (e.node.id.clone(), e.node.name.clone()))
        .collect();

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

    for (service_id, svc) in image_services {
        let service_name = service_names
            .get(service_id)
            .cloned()
            .unwrap_or_else(|| service_id.clone());
        let slug = slugify(&service_name);

        let image = svc.source.as_ref().unwrap().image.clone().unwrap();
        let environment = svc.get_env_vars();
        let ports: Vec<String> = svc
            .get_ports()
            .into_iter()
            .map(|internal_port| {
                let external_port = generate_port(service_id, internal_port);
                format!("{}:{}", external_port, internal_port)
            })
            .collect();
        let start_command = svc.deploy.as_ref().and_then(|d| d.start_command.clone());

        compose_services.insert(
            slug,
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
        home.join(".railway")
            .join("develop")
            .join(&linked_project.project)
            .join("docker-compose.yml")
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

    println!(
        "\n{}",
        "Starting containers in background with docker compose...".cyan()
    );

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

    println!("{}", "Containers started in background".green());

    Ok(())
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
