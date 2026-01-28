use super::*;
use crate::{
    controllers::project::{ensure_project_and_environment_exist, get_project},
    errors::RailwayError,
    queries::project::{
        ProjectProject, ProjectProjectEnvironmentsEdgesNodeVolumeInstancesEdgesNode,
    },
    util::{
        progress::create_spinner,
        prompt::{fake_select, prompt_confirm_with_default, prompt_options, prompt_text},
    },
};
use anyhow::{anyhow, bail};
use clap::Parser;
use is_terminal::IsTerminal;
use std::fmt::Display;

/// Manage project volumes
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Service ID
    #[clap(long, short)]
    service: Option<String>,

    /// Environment ID
    #[clap(long, short)]
    environment: Option<String>,
}
structstruck::strike! {
    #[strikethrough[derive(Parser)]]
    enum Commands {
        /// List volumes
        #[clap(alias = "ls")]
        List(struct {
            /// Output in JSON format
            #[clap(long)]
            json: bool,
        }),

        /// Add a new volume
        #[clap(alias = "create")]
        Add(struct {
            /// The mount path of the volume
            #[clap(long, short)]
            mount_path: Option<String>,

            /// Output in JSON format
            #[clap(long)]
            json: bool,
        }),

        /// Delete a volume
        #[clap(alias = "remove", alias = "rm")]
        Delete(struct {
            /// The ID/name of the volume you wish to delete
            #[clap(long, short)]
            volume: Option<String>,

            /// Skip confirmation dialog
            #[clap(short = 'y', long = "yes")]
            yes: bool,

            /// 2FA code for verification (required if 2FA is enabled in non-interactive mode)
            #[clap(long = "2fa-code")]
            two_factor_code: Option<String>,

            /// Output in JSON format
            #[clap(long)]
            json: bool,
        }),

        /// Update a volume
        #[clap(alias = "edit")]
        Update(struct {
            /// The ID/name of the volume you wish to update
            #[clap(long, short)]
            volume: Option<String>,

            /// The new mount path of the volume (optional)
            #[clap(long, short)]
            mount_path: Option<String>,

            /// The new name of the volume (optional)
            #[clap(long, short)]
            name: Option<String>,

            /// Output in JSON format
            #[clap(long)]
            json: bool,
        }),

        /// Detach a volume from a service
        Detach(struct {
            /// The ID/name of the volume you wish to detach
            #[clap(long, short)]
            volume: Option<String>,

            /// Skip confirmation dialog
            #[clap(short = 'y', long = "yes")]
            yes: bool,

            /// Output in JSON format
            #[clap(long)]
            json: bool,
        })

        /// Attach a volume to a service
        Attach(struct {
            /// The ID/name of the volume you wish to attach
            #[clap(long, short)]
            volume: Option<String>,

            /// Skip confirmation dialog
            #[clap(short = 'y', long = "yes")]
            yes: bool,

            /// Output in JSON format
            #[clap(long)]
            json: bool,
        })
    }
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;

    ensure_project_and_environment_exist(&client, &configs, &linked_project).await?;

    let project = get_project(&client, &configs, linked_project.project.clone()).await?;
    let service = args.service.or_else(|| linked_project.service.clone());
    let environment = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());

    match args.command {
        Commands::Add(a) => add(service, environment, a.mount_path, project, a.json).await?,
        Commands::List(l) => list(environment, project, l.json).await?,
        Commands::Delete(d) => {
            delete(
                environment,
                d.volume,
                project,
                d.yes,
                d.two_factor_code,
                d.json,
            )
            .await?
        }
        Commands::Update(u) => {
            update(environment, u.volume, u.mount_path, u.name, project, u.json).await?
        }
        Commands::Detach(d) => detach(environment, d.volume, project, d.yes, d.json).await?,
        Commands::Attach(a) => {
            attach(environment, a.volume, service, project, a.yes, a.json).await?
        }
    }

    Ok(())
}

async fn attach(
    environment: String,
    volume: Option<String>,
    service: Option<String>,
    project: ProjectProject,
    yes: bool,
    json: bool,
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let is_terminal = std::io::stdout().is_terminal();
    let volume = select_volume(project.clone(), environment.as_str(), volume, is_terminal)?.0;
    let service = service.ok_or_else(|| anyhow!("No service found. Please link one via `railway link` or specify one via the `--service` flag."))?;
    let service_name = &project
        .services
        .edges
        .iter()
        .find(|s| s.node.id == service)
        .ok_or_else(|| anyhow!("The service linked/provided doesn't exist"))?
        .node
        .name;
    if volume.service_id.is_some() {
        bail!(
            "Volume {} is already mounted to service {}. Please detach it via `railway volume detach` first.",
            volume.volume.name,
            project
                .services
                .edges
                .iter()
                .find(|a| a.node.id == volume.service_id.clone().unwrap_or_default())
                .ok_or(anyhow!(
                    "The service the volume is attached to doesn't exist"
                ))?
                .node
                .name
        )
    }
    let confirm = if yes {
        true
    } else if is_terminal {
        prompt_confirm_with_default(
            format!(
                "Are you sure you want to attach the volume {} to service {}?",
                volume.volume.name, service_name
            )
            .as_str(),
            false,
        )?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };
    if confirm {
        let p = post_graphql::<mutations::VolumeAttach, _>(
            &client,
            configs.get_backboard(),
            mutations::volume_attach::Variables {
                volume_id: volume.volume.id.clone(),
                service_id: service.clone(),
                environment_id: environment.clone(),
            },
        )
        .await?;

        if p.volume_instance_update {
            if json {
                println!("{}", serde_json::json!({"success": true}));
            } else {
                println!(
                    "Volume \"{}\" attached to service \"{}\"",
                    volume.volume.name.blue(),
                    service_name.blue()
                );
            }
        } else {
            bail!("Failed to attach volume");
        }
    }

    Ok(())
}

async fn detach(
    environment: String,
    volume: Option<String>,
    project: ProjectProject,
    yes: bool,
    json: bool,
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let is_terminal = std::io::stdout().is_terminal();
    let volume = select_volume(project.clone(), environment.as_str(), volume, is_terminal)?.0;

    if volume.service_id.is_none() {
        bail!(
            "Volume {} is not attached to any service",
            volume.volume.name
        );
    }

    let service = project
        .services
        .edges
        .iter()
        .find(|a| a.node.id == volume.service_id.clone().unwrap_or_default())
        .ok_or(anyhow!(
            "The service the volume is attached to doesn't exist"
        ))?;
    let confirm = if yes {
        true
    } else if is_terminal {
        prompt_confirm_with_default(
            format!(
                "Are you sure you want to detach the volume {} from service {}?",
                volume.volume.name, service.node.name
            )
            .as_str(),
            false,
        )?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };
    if confirm {
        let p = post_graphql::<mutations::VolumeDetach, _>(
            &client,
            configs.get_backboard(),
            mutations::volume_detach::Variables {
                volume_id: volume.volume.id.clone(),
                environment_id: environment,
            },
        )
        .await?;
        if p.volume_instance_update {
            if json {
                println!("{}", serde_json::json!({"success": true}));
            } else {
                println!(
                    "Volume \"{}\" detached from service \"{}\"",
                    volume.volume.name.blue(),
                    service.node.name.blue()
                );
            }
        } else {
            bail!("Failed to detach volume");
        }
    }

    Ok(())
}

async fn update(
    environment: String,
    volume: Option<String>,
    mount_path: Option<String>,
    name: Option<String>,
    project: ProjectProject,
    json: bool,
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let is_terminal = std::io::stdout().is_terminal();
    let volume = select_volume(project, environment.as_str(), volume, is_terminal)?;

    if mount_path.is_none() && name.is_none() {
        bail!(
            "In order to use the update command, please provide a new mount path or a new name via the flags"
        );
    }

    if let Some(ref mount_path) = mount_path {
        if !mount_path.starts_with('/') {
            bail!("All mount paths must start with /")
        }
        post_graphql::<mutations::VolumeMountPathUpdate, _>(
            &client,
            configs.get_backboard(),
            mutations::volume_mount_path_update::Variables {
                volume_id: volume.0.volume.id.clone(),
                service_id: volume.0.service_id.clone(),
                environment_id: environment.clone(),
                mount_path: mount_path.clone(),
            },
        )
        .await?;

        if !json {
            println!(
                "Successfully updated the mount path of volume \"{}\" to \"{}\"",
                volume.0.volume.name.blue(),
                mount_path.purple()
            );
        }
    }

    if let Some(ref name) = name {
        post_graphql::<mutations::VolumeNameUpdate, _>(
            &client,
            configs.get_backboard(),
            mutations::volume_name_update::Variables {
                volume_id: volume.0.volume.id.clone(),
                name: name.clone(),
            },
        )
        .await?;

        if !json {
            println!(
                "Successfully updated the name of volume \"{}\" to \"{}\"",
                volume.0.volume.name.blue(),
                name.purple()
            );
        }
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "id": volume.0.volume.id,
                "name": name.unwrap_or(volume.0.volume.name.clone()),
                "mountPath": mount_path.unwrap_or(volume.0.mount_path.clone())
            })
        );
    }

    Ok(())
}

async fn delete(
    environment: String,
    volume: Option<String>,
    project: ProjectProject,
    yes: bool,
    two_factor_code: Option<String>,
    json: bool,
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let is_terminal = std::io::stdout().is_terminal();
    let volume = select_volume(project, environment.as_str(), volume, is_terminal)?;

    let confirm = if yes {
        true
    } else if is_terminal {
        prompt_confirm_with_default(
            format!(
                r#"Are you sure you want to delete the volume "{}"?"#,
                volume.0.volume.name
            )
            .as_str(),
            false,
        )?
    } else {
        bail!(
            "Cannot prompt for confirmation in non-interactive mode. Use --yes to skip confirmation."
        );
    };
    if confirm {
        let is_two_factor_enabled = {
            let vars = queries::two_factor_info::Variables {};

            let info =
                post_graphql::<queries::TwoFactorInfo, _>(&client, configs.get_backboard(), vars)
                    .await?
                    .two_factor_info;

            info.is_verified
        };

        if is_two_factor_enabled {
            let token = if let Some(code) = two_factor_code {
                code
            } else if is_terminal {
                prompt_text("Enter your 2FA code")?
            } else {
                return Err(RailwayError::TwoFactorRequiresInteractive.into());
            };
            let vars = mutations::validate_two_factor::Variables { token };

            let valid = post_graphql::<mutations::ValidateTwoFactor, _>(
                &client,
                configs.get_backboard(),
                vars,
            )
            .await?
            .two_factor_info_validate;

            if !valid {
                return Err(RailwayError::InvalidTwoFactorCode.into());
            }
        }
        let volume_id = volume.0.volume.id.clone();
        let p = post_graphql::<mutations::VolumeDelete, _>(
            &client,
            configs.get_backboard(),
            mutations::volume_delete::Variables {
                id: volume_id.clone(),
            },
        )
        .await?;
        if p.volume_delete {
            if json {
                println!("{}", serde_json::json!({"id": volume_id}));
            } else {
                println!("Volume \"{}\" deleted", volume.0.volume.name.blue());
            }
        } else {
            bail!("Failed to delete volume");
        }
    }
    Ok(())
}

async fn list(environment: String, project: ProjectProject, json: bool) -> Result<()> {
    let env = project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment)
        .ok_or_else(|| anyhow!("Environment not found"))?;
    let environment_name = env.node.name.clone();

    let volumes = &env.node.volume_instances.edges;

    if volumes.is_empty() {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({ "volumes": [] }))?
            );
        } else {
            bail!(
                "No volumes found in environment {}",
                environment_name.blue()
            );
        }
    } else if json {
        let volumes_json: Vec<serde_json::Value> = volumes
            .iter()
            .map(|volume_edge| {
                let volume = &volume_edge.node;
                let service_name = project
                    .services
                    .edges
                    .iter()
                    .find(|s| s.node.id == volume.service_id.clone().unwrap_or_default())
                    .map(|s| s.node.name.clone());
                serde_json::json!({
                    "id": volume.volume.id,
                    "name": volume.volume.name,
                    "mountPath": volume.mount_path,
                    "serviceName": service_name,
                    "currentSizeMB": volume.current_size_mb,
                    "sizeMB": volume.size_mb,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "project": project.name,
                "environment": environment_name,
                "volumes": volumes_json,
            }))?
        );
    } else {
        println!("Project: {}", project.name.cyan().bold());
        println!("Environment: {}", environment_name.cyan().bold());
        for volume_edge in volumes {
            println!();
            let volume = &volume_edge.node;
            println!("Volume: {}", volume.volume.name.green());
            let service = project
                .services
                .edges
                .iter()
                .find(|s| s.node.id == volume.service_id.clone().unwrap_or_default());
            println!(
                "Attached to: {}",
                if let Some(service) = service {
                    service.node.name.purple()
                } else {
                    "N/A".dimmed()
                }
            );
            println!("Mount path: {}", volume.mount_path.yellow());
            println!(
                "Storage used: {}{}/{}{}",
                volume.current_size_mb.round().to_string().blue(),
                "MB".blue(),
                volume.size_mb.to_string().red(),
                "MB".red()
            )
        }
    }
    Ok(())
}

async fn add(
    service: Option<String>,
    environment: String,
    mount: Option<String>,
    project: ProjectProject,
    json: bool,
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let is_terminal = std::io::stdout().is_terminal();
    let service = service.ok_or_else(|| anyhow!("No service found. Please link one via `railway link` or specify one via the `--service` flag."))?;
    let mount = if let Some(mount) = mount {
        if mount.starts_with('/') {
            if !json {
                fake_select("Enter the mount path of the volume", mount.as_str());
            }
            mount
        } else {
            bail!("Mount path must start with a `/`")
        }
    } else if is_terminal {
        prompt_text("Enter the mount path of the volume")?
    } else {
        bail!("Mount path must be specified via --mount-path in non-interactive mode");
    };

    let service_name = project
        .services
        .edges
        .iter()
        .find(|s| s.node.id == service)
        .map(|s| s.node.name.clone())
        .unwrap();
    let environment_name = project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment)
        .map(|e| e.node.name.clone())
        .unwrap();

    // check if there is a volume already mounted on the service in that environment
    let env = project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment)
        .ok_or_else(|| anyhow!("Environment not found"))?;
    if env
        .node
        .volume_instances
        .edges
        .iter()
        .any(|a| a.node.service_id == Some(service.clone()))
    {
        bail!(
            "A volume is already mounted on service {} in environment {}",
            service_name.blue(),
            environment_name.blue()
        );
    }
    let volume = mutations::volume_create::Variables {
        service_id: service.clone(),
        environment_id: environment.clone(),
        mount_path: mount.clone(),
        project_id: project.id,
    };
    if is_terminal && !json {
        let spinner = create_spinner("Creating volume...".into());

        let details =
            post_graphql::<mutations::VolumeCreate, _>(&client, configs.get_backboard(), volume)
                .await?;

        spinner.finish_with_message(format!(
            "Volume \"{}\" created for service {} in environment {} at mount path \"{}\"",
            details.volume_create.name.blue(),
            service_name.blue(),
            environment_name.blue(),
            mount.cyan().bold()
        ));
    } else if json {
        let details =
            post_graphql::<mutations::VolumeCreate, _>(&client, configs.get_backboard(), volume)
                .await?;

        println!(
            "{}",
            serde_json::json!({
                "id": details.volume_create.id,
                "name": details.volume_create.name
            })
        );
    } else {
        println!("Creating volume...");
        let details =
            post_graphql::<mutations::VolumeCreate, _>(&client, configs.get_backboard(), volume)
                .await?;

        println!(
            "Volume \"{}\" created for service {} in environment {} at mount path \"{}\"",
            details.volume_create.name.blue(),
            service_name.blue(),
            environment_name.blue(),
            mount.cyan().bold()
        );
    }

    Ok(())
}

fn select_volume(
    project: ProjectProject,
    environment: &str,
    volume: Option<String>,
    is_terminal: bool,
) -> Result<Volume, anyhow::Error> {
    let env = project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment)
        .ok_or_else(|| anyhow!("Environment not found"))?;
    let volumes: Vec<Volume> = env
        .node
        .volume_instances
        .edges
        .iter()
        .map(|a| Volume(a.node.clone()))
        .collect();
    let volume = if let Some(vol) = volume {
        let norm_vol = volumes.iter().find(|v| {
            (v.0.volume.name.to_lowercase() == vol.to_lowercase())
                || (v.0.volume.id.to_lowercase() == vol.to_lowercase())
        });
        if let Some(volume) = norm_vol {
            fake_select("Select a volume", &volume.0.volume.name);
            volume.clone()
        } else {
            return Err(RailwayError::VolumeNotFound(vol).into());
        }
    } else if is_terminal {
        let volume = prompt_options("Select a volume", volumes)?;
        volume.clone()
    } else {
        bail!("Volume must be specified via --volume in non-interactive mode");
    };
    Ok(volume)
}

#[derive(Debug, Clone)]
struct Volume(ProjectProjectEnvironmentsEdgesNodeVolumeInstancesEdgesNode);

impl Display for Volume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.volume.name)
    }
}
