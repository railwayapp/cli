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
        variables::get_service_variables,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    networks: Option<DockerComposeNetworks>,
}

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
    networks: Vec<String>,
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

    println!(
        "\n{} image-based service(s) in '{}':",
        image_services.len(),
        env_name
    );

    let mut compose_services = BTreeMap::new();

    for (service_id, svc) in image_services {
        let service_name = service_names
            .get(service_id)
            .cloned()
            .unwrap_or_else(|| service_id.clone());
        let slug = slugify(&service_name);

        let image = svc.source.as_ref().unwrap().image.clone().unwrap();

        // Build port mapping: internal_port -> external_port
        let port_mapping: HashMap<i64, u16> = svc
            .get_ports()
            .into_iter()
            .map(|internal_port| (internal_port, generate_port(service_id, internal_port)))
            .collect();

        // Get resolved variables and override Railway-specific vars
        let raw_vars = resolved_vars.get(service_id).cloned().unwrap_or_default();
        let environment = override_railway_vars(raw_vars, &slug, &port_mapping, &service_slugs);

        let external_ports: Vec<u16> = port_mapping.values().copied().collect();
        let ports: Vec<String> = port_mapping
            .iter()
            .map(|(internal, external)| format!("{}:{}", external, internal))
            .collect();
        // Escape $ as $$ so docker-compose passes them to the container shell
        let start_command = svc
            .deploy
            .as_ref()
            .and_then(|d| d.start_command.clone())
            .map(|cmd| cmd.replace('$', "$$"));

        println!("  {} {}", "•".green(), service_name);
        if !external_ports.is_empty() {
            for port in &external_ports {
                println!("    http://localhost:{}", port);
            }
        }

        compose_services.insert(
            slug,
            DockerComposeService {
                image,
                command: start_command,
                environment,
                ports,
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

    println!(
        "{}",
        format!("{} service(s) started", service_count).green()
    );

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
