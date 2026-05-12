use super::*;
use crate::{
    controllers::environment::get_matched_environment,
    controllers::project::{
        ProjectEnvironmentInstances, ProjectVolumeInstanceNode,
        ensure_project_and_environment_exist, find_service_instance, get_environment_instances,
        get_project, volume_instances_in_env,
    },
    controllers::volume_browser::{VolumeBrowserParams, run as run_volume_browser},
    errors::RailwayError,
    queries::project::ProjectProject,
    util::{
        progress::create_spinner,
        prompt::{fake_select, prompt_confirm_with_default, prompt_options, prompt_text},
        two_factor::validate_two_factor_if_enabled,
    },
};
use anyhow::{anyhow, bail};
use clap::Parser;
use is_terminal::IsTerminal;
use std::{fmt::Display, path::PathBuf};

/// Manage project volumes
#[derive(Parser)]
#[clap(
    after_help = "Examples:\n\n  railway volume list --json\n  railway volume add --service api --mount-path /data --json\n  railway volume update --volume volume-id --name data --mount-path /data --json\n  railway volume delete --volume data --yes --json\n\nAliases:\n  list: ls\n  add: create, new\n  delete: remove, rm\n  update: edit, rename\n\nAutomation notes:\n  Mount paths must start with `/`. Use volume IDs from `railway volume list --json` when names may collide."
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Service ID
    #[clap(long, short)]
    service: Option<String>,

    /// Environment ID
    #[clap(long, short)]
    environment: Option<String>,

    /// Project ID to use (defaults to linked project)
    #[clap(short = 'p', long, value_name = "PROJECT_ID")]
    project: Option<String>,
}
structstruck::strike! {
    #[strikethrough[derive(Parser)]]
    enum Commands {
        /// List volumes
        #[clap(visible_alias = "ls")]
        List(struct {
            /// Output in JSON format
            #[clap(long)]
            json: bool,
        }),

        /// Add a new volume
        #[clap(visible_alias = "create", visible_alias = "new")]
        Add(struct {
            /// The mount path of the volume
            #[clap(long, short)]
            mount_path: Option<String>,

            /// Output in JSON format
            #[clap(long)]
            json: bool,
        }),

        /// Delete a volume
        #[clap(visible_alias = "remove", visible_alias = "rm")]
        Delete(struct {
            /// The ID/name of the volume you wish to delete
            #[clap(long, short)]
            volume: Option<String>,

            /// Skip confirmation dialog
            #[clap(short = 'y', long = "yes")]
            yes: bool,

            /// Output in JSON format
            #[clap(long)]
            json: bool,

            /// 2FA code for verification (required if 2FA is enabled in non-interactive mode)
            #[clap(long = "2fa-code")]
            two_factor_code: Option<String>,
        }),

        /// Update a volume
        #[clap(visible_alias = "edit", visible_alias = "rename")]
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
        }),

        /// Browse volume files over SSH/SCP
        Browse(struct {
            /// The ID/name of the volume you wish to browse
            #[clap(long, short)]
            volume: Option<String>,

            /// Path to identity (private key) file to use, like `ssh -i`
            #[clap(short = 'i', long = "identity-file", value_name = "PATH")]
            identity_file: Option<PathBuf>,

            /// Local directory used for downloads and relative uploads
            #[clap(long, value_name = "PATH")]
            local_dir: Option<PathBuf>,
        })
    }
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    if args.project.is_some() && args.environment.is_none() {
        bail!("--environment is required when using --project");
    }
    let linked_project = if args.project.is_none() {
        Some(configs.get_linked_project().await?)
    } else {
        None
    };

    if let Some(ref linked_project) = linked_project {
        ensure_project_and_environment_exist(&client, &configs, linked_project).await?;
    }

    let project_id = args
        .project
        .clone()
        .or_else(|| linked_project.as_ref().map(|lp| lp.project.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!("No project specified. Use --project or run `railway link` first")
        })?;
    let project = get_project(&client, &configs, project_id.clone()).await?;
    let service = args
        .service
        .or_else(|| linked_project.as_ref().and_then(|lp| lp.service.clone()));
    let environment_input = match args.environment.clone() {
        Some(env) => env,
        None => linked_project
            .as_ref()
            .context("No environment linked. Use --environment when using --project")?
            .environment_id()?
            .to_string(),
    };
    let environment = get_matched_environment(&project, environment_input)?.id;
    let environment_instances =
        get_environment_instances(&client, &configs, &project_id, &environment).await?;

    match args.command {
        Commands::Add(a) => {
            add(
                service,
                environment,
                &environment_instances,
                project,
                a.mount_path,
                a.json,
            )
            .await?
        }
        Commands::List(l) => list(environment, &environment_instances, project, l.json).await?,
        Commands::Delete(d) => {
            delete(
                environment,
                &environment_instances,
                d.volume,
                project,
                d.yes,
                d.json,
                d.two_factor_code,
            )
            .await?
        }
        Commands::Update(u) => {
            update(
                environment,
                &environment_instances,
                u.volume,
                u.mount_path,
                u.name,
                project,
                u.json,
            )
            .await?
        }
        Commands::Detach(d) => {
            detach(
                environment,
                &environment_instances,
                d.volume,
                project,
                d.yes,
                d.json,
            )
            .await?
        }
        Commands::Attach(a) => {
            attach(
                environment,
                &environment_instances,
                a.volume,
                service,
                project,
                a.yes,
                a.json,
            )
            .await?
        }
        Commands::Browse(b) => {
            browse(
                environment,
                &environment_instances,
                b.volume,
                project,
                b.identity_file,
                b.local_dir,
                &client,
                &configs,
            )
            .await?
        }
    }

    Ok(())
}

async fn browse(
    environment: String,
    environment_instances: &ProjectEnvironmentInstances,
    volume: Option<String>,
    project: ProjectProject,
    identity_file: Option<PathBuf>,
    local_dir: Option<PathBuf>,
    client: &reqwest::Client,
    configs: &Configs,
) -> Result<()> {
    let is_terminal = std::io::stdout().is_terminal();
    if !is_terminal {
        bail!("Volume browsing requires an interactive terminal");
    }

    let volume = select_volume(
        project.clone(),
        environment_instances,
        environment.as_str(),
        volume,
        is_terminal,
    )?
    .0;

    let service_id = volume.service_id.clone().ok_or_else(|| {
        anyhow!(
            "Volume {} is not attached to any service. Attach it before browsing.",
            volume.volume.name
        )
    })?;

    let service_name = project
        .services
        .edges
        .iter()
        .find(|service| service.node.id == service_id)
        .map(|service| service.node.name.clone())
        .ok_or_else(|| anyhow!("The service attached to this volume doesn't exist"))?;

    let service_instance_id = find_service_instance(environment_instances, &service_id)
        .map(|instance| instance.id.clone())
        .ok_or_else(|| anyhow!("No active service instance found for service {service_name}"))?;

    if identity_file.is_none() {
        super::ssh::native::ensure_ssh_key(client, configs).await?;
    }

    run_volume_browser(VolumeBrowserParams {
        service_instance_id,
        service_name,
        volume_name: volume.volume.name,
        mount_path: PathBuf::from(volume.mount_path),
        local_dir: local_dir.unwrap_or(std::env::current_dir()?),
        identity_file,
    })?;

    Ok(())
}

async fn attach(
    environment: String,
    environment_instances: &ProjectEnvironmentInstances,
    volume: Option<String>,
    service: Option<String>,
    project: ProjectProject,
    yes: bool,
    json: bool,
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let is_terminal = std::io::stdout().is_terminal();
    let volume = select_volume(
        project.clone(),
        environment_instances,
        environment.as_str(),
        volume,
        is_terminal,
    )?
    .0;
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
    environment_instances: &ProjectEnvironmentInstances,
    volume: Option<String>,
    project: ProjectProject,
    yes: bool,
    json: bool,
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let is_terminal = std::io::stdout().is_terminal();
    let volume = select_volume(
        project.clone(),
        environment_instances,
        environment.as_str(),
        volume,
        is_terminal,
    )?
    .0;

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
    environment_instances: &ProjectEnvironmentInstances,
    volume: Option<String>,
    mount_path: Option<String>,
    name: Option<String>,
    project: ProjectProject,
    json: bool,
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let is_terminal = std::io::stdout().is_terminal();
    let volume = select_volume(
        project,
        environment_instances,
        environment.as_str(),
        volume,
        is_terminal,
    )?;

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
    environment_instances: &ProjectEnvironmentInstances,
    volume: Option<String>,
    project: ProjectProject,
    yes: bool,
    json: bool,
    two_factor_code: Option<String>,
) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let is_terminal = std::io::stdout().is_terminal();
    let volume = select_volume(
        project,
        environment_instances,
        environment.as_str(),
        volume,
        is_terminal,
    )?;

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
        validate_two_factor_if_enabled(&client, &configs, is_terminal, two_factor_code).await?;

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

async fn list(
    environment: String,
    environment_instances: &ProjectEnvironmentInstances,
    project: ProjectProject,
    json: bool,
) -> Result<()> {
    let environment_name = project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment)
        .map(|e| e.node.name.clone())
        .ok_or_else(|| anyhow!("Environment not found"))?;

    let volumes = volume_instances_in_env(environment_instances);

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
    environment_instances: &ProjectEnvironmentInstances,
    project: ProjectProject,
    mount: Option<String>,
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
    if !project
        .environments
        .edges
        .iter()
        .any(|e| e.node.id == environment)
    {
        bail!("Environment not found");
    }
    if volume_instances_in_env(environment_instances)
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
    environment_instances: &ProjectEnvironmentInstances,
    environment: &str,
    volume: Option<String>,
    is_terminal: bool,
) -> Result<Volume, anyhow::Error> {
    project
        .environments
        .edges
        .iter()
        .find(|e| e.node.id == environment)
        .ok_or_else(|| anyhow!("Environment not found"))?;
    let volumes: Vec<Volume> = volume_instances_in_env(environment_instances)
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
struct Volume(ProjectVolumeInstanceNode);

impl Display for Volume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.volume.name)
    }
}
