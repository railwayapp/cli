use super::*;
use crate::{
    controllers::project::get_project,
    queries::project::{ProjectProject, ProjectProjectVolumesEdges},
    util::prompt::{fake_select, prompt_text},
};
use anyhow::{anyhow, bail};
use clap::Parser;

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
        Delete,

        /// Update a volume
        Update,
    }
}

pub async fn command(args: Args, _json: bool) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;
    let service = args.service.or_else(|| linked_project.service.clone());
    let environment = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());

    match args.command {
        Commands::Add(a) => add(service, environment, a.mount_path, project).await?,
        Commands::List => list(environment, project).await?,
        _ => unimplemented!(),
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
            println!(
                "Attached to: {}",
                project
                    .services
                    .edges
                    .iter()
                    .find(|s| s.node.id == volume.node.service_id.clone().unwrap())
                    .unwrap()
                    .node
                    .name
                    .purple()
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

    Ok(())
}
