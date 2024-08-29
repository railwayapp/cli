use super::*;
use crate::{
    consts::TICK_STRING,
    controllers::project::{ensure_project_and_environment_exist, get_project},
    errors::RailwayError,
    queries::project::{
        ProjectProject, ProjectProjectVolumesEdges,
        ProjectProjectVolumesEdgesNodeVolumeInstancesEdgesNode,
    },
    util::prompt::{fake_select, prompt_confirm_with_default, prompt_options, prompt_text},
};
use anyhow::{anyhow, bail};
use clap::Parser;
use is_terminal::IsTerminal;
use std::{fmt::Display, time::Duration};

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
        List,

        /// Add a new volume
        #[clap(alias = "create")]
        Add(struct {
            /// The mount path of the volume
            #[clap(long, short)]
            mount_path: Option<String>,
        }),

        /// Delete a volume
        #[clap(alias = "remove", alias = "rm")]
        Delete(struct {
            /// The ID/name of the volume you wish to delete
            #[clap(long, short)]
            volume: Option<String>
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

        }),

        /// Detach a volume from a service
        Detach(struct {
            /// The ID/name of the volume you wish to detach
            #[clap(long, short)]
            volume: Option<String>
        })

        /// Attach a volume to a service
        Attach(struct {
            /// The ID/name of the volume you wish to attach
            #[clap(long, short)]
            volume: Option<String>
        })
    }
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
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
        Commands::Add(a) => add(service, environment, a.mount_path, project).await?,
        Commands::List => list(environment, project).await?,
        Commands::Delete(d) => delete(environment, d.volume, project).await?,
        Commands::Update(u) => update(environment, u.volume, u.mount_path, u.name, project).await?,
        Commands::Detach(d) => detach(environment, d.volume, project).await?,
        Commands::Attach(a) => attach(environment, a.volume, service, project).await?,
    }

    Ok(())
}

async fn attach(
    environment: String,
    volume: Option<String>,
    service: Option<String>,
    project: ProjectProject,
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let volume = select_volume(project.clone(), environment.as_str(), volume)?.0;
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
        bail!("Volume {} is already mounted to service {}. Please detach it via `railway volume detach` first.", volume.volume.name, project
        .services
        .edges
        .iter()
        .find(|a| a.node.id == volume.service_id.clone().unwrap_or_default())
        .ok_or(anyhow!(
            "The service the volume is attcahed to doesn't exist"
        ))?.node.name)
    }
    let confirm = prompt_confirm_with_default(
        format!(
            "Are you sure you want to attach the volume {} to service {}?",
            volume.volume.name, service_name
        )
        .as_str(),
        false,
    )?;
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
            println!(
                "Volume \"{}\" attached to service \"{}\"",
                volume.volume.name.blue(),
                service_name.blue()
            );
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
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let volume = select_volume(project.clone(), environment.as_str(), volume)?.0;

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
            "The service the volume is attcahed to doesn't exist"
        ))?;
    let confirm = prompt_confirm_with_default(
        format!(
            "Are you sure you want to detach the volume {} from service {}?",
            volume.volume.name, service.node.name
        )
        .as_str(),
        false,
    )?;
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
            println!(
                "Volume \"{}\" detached from service \"{}\"",
                volume.volume.name.blue(),
                service.node.name.blue()
            );
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
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let volume = select_volume(project, environment.as_str(), volume)?;

    if mount_path.is_none() && name.is_none() {
        bail!("In order to use the update command, please provide a new mount path or a new name via the flags");
    }

    if let Some(mount_path) = mount_path {
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

        println!(
            "Succesfully updated the mount path of volume \"{}\" to \"{}\"",
            volume.0.volume.name.blue(),
            mount_path.purple()
        );
    }

    if let Some(name) = name {
        post_graphql::<mutations::VolumeNameUpdate, _>(
            &client,
            configs.get_backboard(),
            mutations::volume_name_update::Variables {
                volume_id: volume.0.volume.id.clone(),
                name: name.clone(),
            },
        )
        .await?;

        println!(
            "Succesfully updated the name of volume \"{}\" to \"{}\"",
            volume.0.volume.name.blue(),
            name.purple()
        );
    }

    Ok(())
}

async fn delete(
    environment: String,
    volume: Option<String>,
    project: ProjectProject,
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let volume = select_volume(project, environment.as_str(), volume)?;

    let confirm = prompt_confirm_with_default(
        format!(
            r#"Are you sure you want to delete the volume "{}"?"#,
            volume.0.volume.name
        )
        .as_str(),
        false,
    )?;
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
            let token = prompt_text("Enter your 2FA code")?;
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
            mutations::volume_delete::Variables { id: volume_id },
        )
        .await?;
        if p.volume_delete {
            println!("Volume \"{}\" deleted", volume.0.volume.name.blue());
        } else {
            bail!("Failed to delete volume");
        }
    }
    Ok(())
}

async fn list(environment: String, project: ProjectProject) -> Result<()> {
    let volumes: Vec<&ProjectProjectVolumesEdges> = project
        .volumes
        .edges
        .iter()
        .filter(|v| {
            v.node
                .volume_instances
                .edges
                .iter()
                .any(|a| a.node.environment_id == environment.clone())
        })
        .collect();
    let environment_name = project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment)
        .map(|e| e.node.name.clone())
        .unwrap();

    if volumes.is_empty() {
        bail!(
            "No volumes found in environment {}",
            environment_name.blue()
        );
    } else {
        println!("Project: {}", project.name.cyan().bold());
        println!("Environment: {}", environment_name.cyan().bold());
        for volume in volumes {
            println!();
            let volume = volume
                .node
                .volume_instances
                .edges
                .iter()
                .find(|a| a.node.environment_id == environment.clone())
                .unwrap();
            println!("Volume: {}", volume.node.volume.name.green());
            let service = project
                .services
                .edges
                .iter()
                .find(|s| s.node.id == volume.node.service_id.clone().unwrap_or_default());
            println!(
                "Attached to: {}",
                if let Some(service) = service {
                    service.node.name.purple()
                } else {
                    "N/A".dimmed()
                }
            );
            println!("Mount path: {}", volume.node.mount_path.yellow());
            println!(
                "Storage used: {}{}/{}{}",
                volume.node.current_size_mb.round().to_string().blue(),
                "MB".blue(),
                volume.node.size_mb.to_string().red(),
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
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let service = service.ok_or_else(|| anyhow!("No service found. Please link one via `railway link` or specify one via the `--service` flag."))?;
    let mount = if let Some(mount) = mount {
        if mount.starts_with('/') {
            fake_select("Enter the mount path of the volume", mount.as_str());
            mount
        } else {
            bail!("Mount path must start with a `/`")
        }
    } else {
        prompt_text("Enter the mount path of the volume")?
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
    if project.volumes.edges.iter().any(|v| {
        v.node.volume_instances.edges.iter().any(|a| {
            a.node.service_id == Some(service.clone())
                && a.node.environment_id == environment.clone()
        })
    }) {
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
    if std::io::stdout().is_terminal() {
        let spinner = indicatif::ProgressBar::new_spinner()
            .with_style(
                indicatif::ProgressStyle::default_spinner()
                    .tick_chars(TICK_STRING)
                    .template("{spinner:.green} {msg}")?,
            )
            .with_message("Creating volume..");
        spinner.enable_steady_tick(Duration::from_millis(100));

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
    } else {
        println!("Creating volume..");
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
) -> Result<Volume, anyhow::Error> {
    let volumes: Vec<Volume> = project
        .volumes
        .edges
        .iter()
        .filter_map(|v| {
            v.node
                .volume_instances
                .edges
                .iter()
                .find(|a| a.node.environment_id == environment)
                .map(|a| Volume(a.node.clone()))
        })
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
    } else {
        // prompt
        let volume = prompt_options("Select a volume", volumes)?;
        volume.clone()
    };
    Ok(volume)
}

#[derive(Debug, Clone)]
struct Volume(ProjectProjectVolumesEdgesNodeVolumeInstancesEdgesNode);

impl Display for Volume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.volume.name)
    }
}
