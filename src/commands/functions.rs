use super::*;
use crate::{
    consts::TICK_STRING,
    controllers::project::{ensure_project_and_environment_exist, get_project},
    errors::RailwayError,
    queries::project::{
        DeploymentStatus, ProjectProject, ProjectProjectVolumesEdges,
        ProjectProjectVolumesEdgesNodeVolumeInstancesEdgesNode,
    },
    util::prompt::{
        fake_select, prompt_confirm_with_default, prompt_options, prompt_path, prompt_text,
    },
};
use anyhow::{anyhow, bail};
use chrono_humanize::{HumanTime, Humanize};
use clap::Parser;
use is_terminal::IsTerminal;
use pathdiff::diff_paths;
use std::fmt::Write;
use std::{fmt::Display, path::PathBuf, time::Duration};

/// Manage project functions
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,

    /// Environment ID/name
    #[clap(long, short)]
    environment: Option<String>,
}
structstruck::strike! {
    #[strikethrough[derive(Parser)]]
    enum Commands {
        /// List functions
        #[clap(visible_alias = "ls")]
        List,

        /// Add a new volume
        #[clap(visible_alias = "create")]
        New(struct {
            /// The path to the function locally
            #[clap(long, short)]
            path: Option<PathBuf>,

            /// The name of the function
            #[clap(long, short)]
            name: Option<String>,

            /// Cron schedule to run the function
            #[clap(long, short)]
            cron: Option<String>,

            /// Generate a domain
            #[clap(long)]
            http: bool,

            /// Watch for changes of the file and deploy upon save
            #[clap(long, short)]
            watch: bool
        }),

        /// Delete a function
        #[clap(visible_alias = "remove", visible_alias = "rm")]
        Delete(struct {
            /// The ID/name of the function you wish to delete
            #[clap(long, short)]
            function: Option<String>
        }),

        /// Test a function locally (requires Docker)
        #[clap(visible_alias = "t")]
        Test(struct {
            /// Watch for changes of the file and re-run upon save
            #[clap(long, short)]
            watch: bool
        })

        /// Push a new change to the function
        #[clap(visible_alias = "up")]
        Push(struct {
            /// Watch for changes of the file and automatically push
            #[clap(long, short)]
            watch: bool
        })

        /// Pull changes from the linked function remotely
        Pull(struct {
            /// The ID/name of the volume you wish to attach
            #[clap(long, short)]
            volume: Option<String>
        })

        /// Link a function manually
        Link(struct {
            /// The path to the file
            #[clap(long, short)]
            path: PathBuf,

            /// The ID/name of the function you wish to delete
            #[clap(long, short)]
            function: Option<String>
        })
    }
}

pub async fn command(args: Args) -> Result<()> {
    let configs = Configs::new()?;
    let client = GQLClient::new_authorized(&configs)?;
    let linked_project = configs.get_linked_project().await?;
    let project = get_project(&client, &configs, linked_project.project.clone()).await?;
    let environment_input = args
        .environment
        .clone()
        .unwrap_or(linked_project.environment.clone());
    let environment = project
        .environments
        .edges
        .iter()
        .find(|e| {
            (e.node.id.to_lowercase() == environment_input.to_lowercase())
                || (e.node.name.to_lowercase() == environment_input.to_lowercase())
        })
        .ok_or_else(|| RailwayError::EnvironmentNotFound(environment_input))?;

    match args.command {
        Commands::List => list(environment, project.clone()).await,
        Commands::New(args) => new(environment, project.clone(), args).await,
        _ => unreachable!(),
    }?;

    Ok(())
}

async fn new(
    environment: &queries::project::ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
    args: New,
) -> Result<()> {
    let name = if let Some(name) = args.name {
        fake_select("Enter a name for your function", &name);
        name
    } else if std::io::stdout().is_terminal() {
        prompt_text("Enter a name for your function")?
    } else {
        bail!("Name must be provided when not running in a terminal");
    };
    if let Some(ref path) = args.path {
        let relative_path = diff_paths(
            path,
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        )
        .unwrap_or(path.to_path_buf());
        fake_select(
            "Enter the path to your function",
            relative_path.to_str().unwrap(),
        );
        if !path.exists() {
            bail!("Path provided does not exist");
        }
    } else {
        prompt_path("hello");
    };
    // } else if std::io::stdout().is_terminal() {
    //     PathBuf::from(prompt_text("Enter the path to your function")?)
    // } else {
    //     bail!("Path must be provided when not running in a terminal");
    // };
    Ok(())
}

async fn list(
    environment: &queries::project::ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
) -> Result<()> {
    // first get all services
    // then filter through all services that have a service instance in the specified environment
    // then check if the image is the bun runtime image
    let functions: Vec<&queries::project::ProjectProjectServicesEdgesNodeServiceInstancesEdges> =
        project
            .services
            .edges
            .iter()
            .filter_map(|s| {
                s.node
                    .service_instances
                    .edges
                    .iter()
                    .find(|e| e.node.environment_id == environment.node.id)
            })
            .filter(|s| {
                s.node.source.clone().is_some_and(|s| {
                    s.image
                        .unwrap_or_default()
                        .starts_with("ghcr.io/railwayapp/function") // there is only one runtime right now, in the format function-RUNTIME
                })
            })
            .collect();
    if functions.is_empty() {
        println!(
            "No functions in project {} and environment {}",
            project.name.magenta(),
            environment.node.name.magenta()
        );
        return Ok(());
    }

    let info = functions
        .iter()
        .map(|f| {
            let mut n = String::new();
            let coloured_name = if let Some(ref deployment) = f.node.latest_deployment {
                match deployment.status {
                    DeploymentStatus::BUILDING
                    | DeploymentStatus::DEPLOYING
                    | DeploymentStatus::INITIALIZING
                    | DeploymentStatus::QUEUED => f.node.service_name.blue(),
                    DeploymentStatus::CRASHED | DeploymentStatus::FAILED => {
                        f.node.service_name.red()
                    }
                    DeploymentStatus::SLEEPING => f.node.service_name.yellow(),
                    DeploymentStatus::SUCCESS => f.node.service_name.green(),
                    _ => f.node.service_name.dimmed(),
                }
            } else {
                f.node.service_name.dimmed()
            };
            write!(n, "{}", coloured_name).unwrap();
            // get runtime and version
            if let Some(ref source) = f.node.source {
                if let Some(image) = &source.image {
                    // function-RUNTIME:version of runtime
                    let runtime_unparsed = image.split("function-").nth(1).unwrap();
                    let mut runtime = runtime_unparsed.split(":");
                    write!(
                        n,
                        " ({} {}{})",
                        runtime.next().unwrap().blue(),
                        "v".purple(),
                        runtime.next().unwrap().purple()
                    )
                    .unwrap();
                }
            }
            if let Some(next_run) = f.node.next_cron_run_at {
                let ht = HumanTime::from(next_run);
                write!(n, " (next run {})", ht.to_string().yellow()).unwrap();
            }
            n
        })
        .collect::<Vec<String>>()
        .join("\n");
    println!(
        "Functions in project {} and environment {}:\n{}",
        project.name.magenta(),
        environment.node.name.magenta(),
        info
    );
    Ok(())
}
